import { spawn } from "node:child_process";
import { EventEmitter, once } from "node:events";
import readline from "node:readline";

import { truncateLogText } from "../logger.js";

function createTimeoutError(method, timeoutMs) {
  const error = new Error(`${method} timed out after ${timeoutMs}ms.`);
  error.code = "APP_SERVER_TIMEOUT";
  error.details = {
    method,
    timeoutMs,
  };
  return error;
}

function createRpcError(method, rpcError) {
  const message = typeof rpcError?.message === "string"
    ? rpcError.message
    : `${method} failed with an unknown app-server error.`;
  const error = new Error(message);
  error.code = "APP_SERVER_RPC_ERROR";
  error.details = {
    method,
    rpcError,
  };
  return error;
}

export class AppServerTransport extends EventEmitter {
  constructor({ config, logger }) {
    super();
    this.config = config;
    this.logger = logger;
    this.child = null;
    this.pendingRequests = new Map();
    this.nextRequestId = 1;
    this.startPromise = null;
    this.started = false;
    this.stopping = false;
    this.stdoutReader = null;
    this.stderrReader = null;
  }

  async start() {
    if (this.started) {
      return;
    }
    if (this.startPromise) {
      return this.startPromise;
    }

    this.startPromise = this.#startInternal();
    try {
      await this.startPromise;
      this.started = true;
    } finally {
      this.startPromise = null;
    }
  }

  async stop() {
    this.stopping = true;

    for (const pending of this.pendingRequests.values()) {
      clearTimeout(pending.timeoutId);
      pending.reject(new Error("app-server transport stopped before the request completed."));
    }
    this.pendingRequests.clear();

    if (!this.child) {
      return;
    }

    const child = this.child;
    this.child = null;

    if (this.stdoutReader) {
      this.stdoutReader.close();
      this.stdoutReader = null;
    }
    if (this.stderrReader) {
      this.stderrReader.close();
      this.stderrReader = null;
    }

    if (child.killed) {
      return;
    }

    child.kill("SIGTERM");
    const timeoutPromise = new Promise((resolve) => {
      setTimeout(resolve, this.config.appServerShutdownTimeoutMs);
    });
    await Promise.race([once(child, "exit"), timeoutPromise]);
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
      await once(child, "exit");
    }
  }

  async request(method, params) {
    await this.start();

    return this.#requestInternal(method, params, this.config.appServerRequestTimeoutMs);
  }

  async notify(method, params) {
    await this.start();
    this.#writeMessage(params === undefined ? { method } : { method, params });
  }

  respondResult(id, result) {
    this.#writeMessage({
      id,
      result,
    });
  }

  respondError(id, error) {
    this.#writeMessage({
      id,
      error,
    });
  }

  async #requestInternal(method, params, timeoutMs) {
    if (!this.child) {
      const error = new Error("codex app-server transport is not connected.");
      error.code = "APP_SERVER_NOT_CONNECTED";
      throw error;
    }

    const requestId = this.nextRequestId;
    this.nextRequestId += 1;

    return new Promise((resolve, reject) => {
      const requestKey = String(requestId);
      const timeoutId = setTimeout(() => {
        this.pendingRequests.delete(requestKey);
        reject(createTimeoutError(method, timeoutMs));
      }, timeoutMs);

      this.pendingRequests.set(requestKey, {
        method,
        resolve,
        reject,
        timeoutId,
      });

      try {
        this.#writeMessage({
          id: requestId,
          method,
          ...(params === undefined ? {} : { params }),
        });
      } catch (error) {
        clearTimeout(timeoutId);
        this.pendingRequests.delete(requestKey);
        reject(error);
      }
    });
  }

  async #startInternal() {
    // Merge anchor: process boot cwd must stay aligned with bridge default-session
    // cwd semantics used by runtime thread/start and thread/resume checks.
    const child = spawn(this.config.codexCommand, this.config.codexArgs, {
      cwd: this.config.defaultSessionCwd,
      env: process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.child = child;

    child.once("error", (error) => {
      this.#failTransport(error);
    });

    child.once("exit", (code, signal) => {
      const error = new Error(
        `codex app-server exited${code !== null ? ` with code ${code}` : ""}${signal ? ` via ${signal}` : ""}.`,
      );
      error.code = "APP_SERVER_EXITED";
      error.details = { code, signal };
      this.#failTransport(error);
    });

    if (!child.stdin || !child.stdout || !child.stderr) {
      const error = new Error("codex app-server process is missing required stdio pipes.");
      error.code = "APP_SERVER_STDIO_MISSING";
      throw error;
    }

    this.stdoutReader = readline.createInterface({
      input: child.stdout,
      crlfDelay: Infinity,
    });
    this.stdoutReader.on("line", (line) => {
      this.#handleStdoutLine(line);
    });

    this.stderrReader = readline.createInterface({
      input: child.stderr,
      crlfDelay: Infinity,
    });
    this.stderrReader.on("line", (line) => {
      this.logger.warn({
        event: "bridge.app_server.stderr",
        line: truncateLogText(line, 320),
      }, "codex app-server stderr");
    });

    try {
      const initializeResponse = await this.#requestInternal("initialize", {
        clientInfo: this.config.clientInfo,
      }, this.config.appServerStartupTimeoutMs);

      this.#writeMessage({ method: "initialized" });

      this.logger.info({
        event: "bridge.app_server.initialized",
        userAgent: initializeResponse?.userAgent ?? null,
        command: this.config.codexCommand,
        args: this.config.codexArgs,
        cwd: this.config.defaultSessionCwd,
      }, "codex app-server transport ready");
    } catch (error) {
      this.#failTransport(error);
      throw error;
    }
  }

  #handleStdoutLine(line) {
    if (typeof line !== "string" || line.trim().length === 0) {
      return;
    }

    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      const parseError = new Error(`Failed to parse app-server JSONL line: ${line}`);
      parseError.code = "APP_SERVER_INVALID_JSON";
      parseError.cause = error;
      this.#failTransport(parseError);
      return;
    }

    if (typeof message?.method === "string") {
      try {
        if (Object.hasOwn(message, "id")) {
          this.emit("serverRequest", message);
          return;
        }
        this.emit("notification", message);
      } catch (error) {
        const listenerError = new Error("Notification listener failed.");
        listenerError.code = "APP_SERVER_NOTIFICATION_HANDLER_FAILED";
        listenerError.details = {
          method: message.method,
          originalError: error instanceof Error ? error.message : String(error),
        };
        listenerError.cause = error;
        this.#failTransport(listenerError);
      }
      return;
    }

    if (!Object.hasOwn(message ?? {}, "id")) {
      const error = new Error("Received app-server message without method or id.");
      error.code = "APP_SERVER_PROTOCOL_ERROR";
      error.details = { message };
      this.#failTransport(error);
      return;
    }

    const pending = this.pendingRequests.get(String(message.id));
    if (!pending) {
      const error = new Error(`Received app-server response for unknown request id ${message.id}.`);
      error.code = "APP_SERVER_UNKNOWN_RESPONSE";
      error.details = { message };
      this.#failTransport(error);
      return;
    }

    clearTimeout(pending.timeoutId);
    this.pendingRequests.delete(String(message.id));

    if (message.error) {
      pending.reject(createRpcError(pending.method, message.error));
      return;
    }

    pending.resolve(message.result ?? null);
  }

  #writeMessage(message) {
    if (!this.child?.stdin?.writable) {
      const error = new Error("codex app-server stdin is not writable.");
      error.code = "APP_SERVER_STDIN_UNAVAILABLE";
      throw error;
    }

    const serialized = `${JSON.stringify(message)}\n`;
    const wrote = this.child.stdin.write(serialized);
    if (!wrote) {
      this.logger.debug({
        event: "bridge.app_server.stdin_backpressure",
      }, "codex app-server stdin reported backpressure");
    }
  }

  #failTransport(error) {
    if (this.stopping) {
      return;
    }

    for (const pending of this.pendingRequests.values()) {
      clearTimeout(pending.timeoutId);
      pending.reject(error);
    }
    this.pendingRequests.clear();

    this.started = false;
    const child = this.child;
    this.child = null;

    if (this.stdoutReader) {
      this.stdoutReader.close();
      this.stdoutReader = null;
    }
    if (this.stderrReader) {
      this.stderrReader.close();
      this.stderrReader = null;
    }

    if (child && !child.killed) {
      child.kill("SIGTERM");
    }

    this.emit("fatal", error);
  }
}
