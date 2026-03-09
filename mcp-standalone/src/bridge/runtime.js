import { randomUUID } from "node:crypto";
import { realpathSync, statSync } from "node:fs";
import { isAbsolute } from "node:path";

import { createAppServerClient } from "../app-server/client.js";
import { createBridgeStore } from "./store.js";

const MAX_SESSION_LIST_LIMIT = 200;
const MAX_SESSION_POLL_LIMIT = 500;

function nowMilliseconds() {
  return Date.now();
}

function normalizeTranscriptText(value) {
  if (typeof value !== "string") {
    return null;
  }
  const normalized = value.replace(/\r\n/g, "\n");
  return normalized.length > 0 ? normalized : null;
}

function shortId(value) {
  if (typeof value !== "string" || value.length === 0) {
    return "?";
  }
  return value.slice(0, 8);
}

function createBridgeRuntimeError(status, code, message, details = null) {
  const error = new Error(message);
  error.status = status;
  error.code = code;
  error.details = details;
  return error;
}

function createAssociationFailureError(message, details) {
  const error = new Error(message);
  error.code = "SESSION_ASSOCIATION_FAILED";
  error.details = details;
  return error;
}

function createQueryValidationError(message, details = null) {
  return createBridgeRuntimeError(400, "BAD_REQUEST", message, details);
}

function mapAppServerError(method, error) {
  if (error?.code === "APP_SERVER_RPC_ERROR") {
    return createBridgeRuntimeError(
      502,
      "APP_SERVER_RPC_ERROR",
      `app-server ${method} returned a JSON-RPC error.`,
      error.details ?? null,
    );
  }
  if (error?.code === "APP_SERVER_TIMEOUT") {
    return createBridgeRuntimeError(
      504,
      "APP_SERVER_TIMEOUT",
      `app-server ${method} timed out.`,
      error.details ?? null,
    );
  }
  if (
    error?.code === "APP_SERVER_NOT_CONNECTED"
    || error?.code === "APP_SERVER_EXITED"
    || error?.code === "APP_SERVER_STDIN_UNAVAILABLE"
  ) {
    return createBridgeRuntimeError(
      503,
      "APP_SERVER_UNAVAILABLE",
      `app-server is unavailable for ${method}.`,
      {
        code: error.code,
        details: error.details ?? null,
      },
    );
  }

  return createBridgeRuntimeError(
    502,
    "APP_SERVER_REQUEST_FAILED",
    `app-server ${method} request failed.`,
    {
      code: error?.code ?? null,
      message: error instanceof Error ? error.message : String(error),
      details: error?.details ?? null,
    },
  );
}

function normalizeSessionStatus(threadStatus) {
  if (!threadStatus || typeof threadStatus !== "object") {
    return "idle";
  }

  switch (threadStatus.type) {
    case "idle":
      return "idle";
    case "notLoaded":
      return "idle";
    case "systemError":
      return "failed";
    case "active": {
      const activeFlags = Array.isArray(threadStatus.activeFlags) ? threadStatus.activeFlags : [];
      if (activeFlags.includes("waitingOnApproval")) {
        return "waitingOnApproval";
      }
      if (activeFlags.includes("waitingOnUserInput")) {
        return "waitingOnUserInput";
      }
      return "running";
    }
    default:
      return "running";
  }
}

function normalizeTurnStatus(turnStatus) {
  switch (turnStatus) {
    case "completed":
      return "idle";
    case "failed":
      return "failed";
    case "interrupted":
      return "interrupted";
    case "inProgress":
      return "running";
    default:
      return "running";
  }
}

function extractThreadId(message) {
  const params = message?.params ?? {};
  if (typeof params.threadId === "string") {
    return params.threadId;
  }
  if (typeof params.thread?.id === "string") {
    return params.thread.id;
  }
  if (typeof params.turn?.threadId === "string") {
    return params.turn.threadId;
  }
  return null;
}

function extractTurnId(message) {
  const params = message?.params ?? {};
  if (typeof params.turnId === "string") {
    return params.turnId;
  }
  if (typeof params.turn?.id === "string") {
    return params.turn.id;
  }
  return null;
}

function extractItemId(message) {
  const params = message?.params ?? {};
  if (typeof params.itemId === "string") {
    return params.itemId;
  }
  if (typeof params.item?.id === "string") {
    return params.item.id;
  }
  return null;
}

function extractAgentMessagePreview(item) {
  if (!item || item.type !== "agentMessage") {
    return null;
  }
  if (typeof item.text === "string" && item.text.length > 0) {
    return item.text;
  }
  if (Array.isArray(item.content)) {
    return item.content
      .map((part) => (typeof part?.text === "string" ? part.text : null))
      .filter(Boolean)
      .join("") || null;
  }
  return null;
}

function createApprovalPayload(serverRequest) {
  const params = serverRequest.params ?? {};
  const kindByMethod = {
    "item/commandExecution/requestApproval": "command",
    "item/fileChange/requestApproval": "fileChange",
    "item/tool/requestUserInput": "userInput",
    "mcpServer/elicitation/request": "mcpElicitation",
  };
  const kind = kindByMethod[serverRequest.method] ?? "userInput";

  return {
    approvalId: typeof params.approvalId === "string" ? params.approvalId : String(serverRequest.id),
    requestId: String(serverRequest.id),
    kind,
    status: "pending",
    title: typeof params.reason === "string" && params.reason.length > 0
      ? params.reason
      : serverRequest.method,
    detail: typeof params.detail === "string" ? params.detail : null,
    turnId: typeof params.turnId === "string" ? params.turnId : null,
    itemId: typeof params.itemId === "string" ? params.itemId : null,
    createdAt: nowMilliseconds(),
  };
}

function createSnapshotResponse(store, session) {
  return {
    session: store.toSessionSummary(session),
    snapshot: store.toSnapshot(session),
  };
}

function resolveSessionCwd(requestBody, defaultSessionCwd) {
  const rawCwd = requestBody?.cwd;
  if (rawCwd === undefined || rawCwd === null) {
    return defaultSessionCwd;
  }
  if (typeof rawCwd !== "string") {
    throw createBridgeRuntimeError(400, "BAD_REQUEST", "session_create cwd must be a string or null.", {
      cwd: rawCwd,
    });
  }

  const trimmed = rawCwd.trim();
  if (trimmed.length === 0) {
    throw createBridgeRuntimeError(400, "BAD_REQUEST", "session_create cwd must not be empty when provided.", {
      cwd: rawCwd,
    });
  }
  if (!isAbsolute(trimmed)) {
    throw createBridgeRuntimeError(400, "BAD_REQUEST", "session_create cwd must be an absolute directory path.", {
      cwd: rawCwd,
    });
  }

  let stats;
  try {
    stats = statSync(trimmed);
  } catch {
    throw createBridgeRuntimeError(400, "BAD_REQUEST", "session_create cwd must point to an existing directory.", {
      cwd: rawCwd,
    });
  }
  if (!stats.isDirectory()) {
    throw createBridgeRuntimeError(400, "BAD_REQUEST", "session_create cwd must point to a directory.", {
      cwd: rawCwd,
    });
  }

  return realpathSync(trimmed);
}

function extractTranscriptItemType(message) {
  const itemType = message?.params?.item?.type;
  return typeof itemType === "string" && itemType.length > 0 ? itemType : "item";
}

function extractTranscriptLines(text) {
  const normalized = normalizeTranscriptText(text);
  if (!normalized) {
    return [];
  }
  return normalized.split("\n").filter((line) => line.length > 0);
}

function parseOptionalQueryString(query, key) {
  const rawValue = query?.[key];
  if (rawValue === undefined) {
    return null;
  }
  if (Array.isArray(rawValue)) {
    throw createQueryValidationError(`Query parameter ${key} must appear at most once.`, {
      key,
      value: rawValue,
    });
  }
  if (typeof rawValue !== "string") {
    throw createQueryValidationError(`Query parameter ${key} must be a string when provided.`, {
      key,
      value: rawValue,
    });
  }
  const trimmed = rawValue.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function parseOffsetCursor(query) {
  const rawCursor = parseOptionalQueryString(query, "cursor");
  if (rawCursor === null) {
    return 0;
  }
  if (!/^\d+$/.test(rawCursor)) {
    throw createQueryValidationError("session_list cursor must be a non-negative integer string.", {
      cursor: rawCursor,
    });
  }
  return Number.parseInt(rawCursor, 10);
}

function parseLimit(query, { maxValue, routeName }) {
  const rawLimit = parseOptionalQueryString(query, "limit");
  if (rawLimit === null) {
    return null;
  }
  if (!/^\d+$/.test(rawLimit)) {
    throw createQueryValidationError(`${routeName} limit must be a positive integer string.`, {
      limit: rawLimit,
    });
  }

  const parsed = Number.parseInt(rawLimit, 10);
  if (parsed < 1) {
    throw createQueryValidationError(`${routeName} limit must be greater than zero.`, {
      limit: rawLimit,
    });
  }
  return Math.min(parsed, maxValue);
}

function createUnsupportedServerRequestResponse(serverRequest) {
  switch (serverRequest.method) {
    case "item/commandExecution/requestApproval":
      return {
        result: { decision: "decline" },
      };
    case "item/fileChange/requestApproval":
      return {
        result: { decision: "decline" },
      };
    case "item/tool/requestUserInput":
      return {
        result: { answers: {} },
      };
    case "mcpServer/elicitation/request":
      return {
        result: { action: "decline", content: null, _meta: null },
      };
    default:
      return {
        error: {
          code: -32601,
          message: `Unsupported client-side server request method ${serverRequest.method}.`,
          data: {
            method: serverRequest.method,
          },
        },
      };
  }
}

export function createBridgeRuntime({ config, logger, onFatal }) {
  const store = createBridgeStore();
  const appServerClient = createAppServerClient({ config, logger });
  const bufferedThreadMessages = new Map();
  const transcriptBuffers = new Map();
  const health = {
    appServerState: "starting",
    lastError: null,
  };

  function createTranscriptScope({ threadId = null, turnId = null, itemId = null }) {
    const session = typeof threadId === "string" ? store.getSessionByThreadId(threadId) : null;
    return {
      sessionId: session ? shortId(session.sessionId) : "unmapped",
      turnId: typeof turnId === "string" ? shortId(turnId) : "-",
      itemId: typeof itemId === "string" ? shortId(itemId) : "-",
    };
  }

  function writeTranscriptLine(channel, scope, text) {
    if (!config.bridgeDebugTranscript) {
      return;
    }
    process.stderr.write(
      `${channel.padEnd(7)} [s:${scope.sessionId} t:${scope.turnId} i:${scope.itemId}] ${text}\n`,
    );
  }

  function writeTranscriptText(channel, scope, text) {
    for (const line of extractTranscriptLines(text)) {
      writeTranscriptLine(channel, scope, line);
    }
  }

  function createTranscriptBufferKey(channel, threadId, turnId, itemId) {
    return `${channel}:${threadId ?? "-"}:${turnId ?? "-"}:${itemId ?? "-"}`;
  }

  function appendTranscriptDelta(channel, threadId, turnId, itemId, text) {
    if (!config.bridgeDebugTranscript) {
      return;
    }

    const normalized = normalizeTranscriptText(text);
    if (!normalized) {
      return;
    }

    const key = createTranscriptBufferKey(channel, threadId, turnId, itemId);
    const scope = createTranscriptScope({ threadId, turnId, itemId });
    let buffer = `${transcriptBuffers.get(key) ?? ""}${normalized}`;

    while (buffer.includes("\n")) {
      const newlineIndex = buffer.indexOf("\n");
      const line = buffer.slice(0, newlineIndex);
      if (line.length > 0) {
        writeTranscriptLine(channel, scope, line);
      }
      buffer = buffer.slice(newlineIndex + 1);
    }

    if (buffer.length > 0) {
      transcriptBuffers.set(key, buffer);
      return;
    }

    transcriptBuffers.delete(key);
  }

  function flushTranscriptBuffer(channel, threadId, turnId, itemId) {
    const key = createTranscriptBufferKey(channel, threadId, turnId, itemId);
    const pending = transcriptBuffers.get(key);
    if (!pending) {
      return false;
    }
    transcriptBuffers.delete(key);
    writeTranscriptLine(channel, createTranscriptScope({ threadId, turnId, itemId }), pending);
    return true;
  }

  function writeTurnTranscript(threadId, turnId, text) {
    writeTranscriptLine("TURN", createTranscriptScope({ threadId, turnId }), text);
  }

  function writeItemTranscript(threadId, turnId, itemId, text) {
    writeTranscriptLine("ITEM", createTranscriptScope({ threadId, turnId, itemId }), text);
  }

  function writeAgentTranscript(threadId, turnId, itemId, text) {
    appendTranscriptDelta("AGENT", threadId, turnId, itemId, text);
  }

  function writeReasoningTranscript(threadId, turnId, itemId, text) {
    appendTranscriptDelta("THINK", threadId, turnId, itemId, text);
  }

  function writeToolTranscript(threadId, turnId, itemId, text) {
    appendTranscriptDelta("TOOL", threadId, turnId, itemId, text);
  }

  function writeRequestTranscript(threadId, turnId, itemId, text) {
    writeTranscriptLine("REQUEST", createTranscriptScope({ threadId, turnId, itemId }), text);
  }

  function writeErrorTranscript(threadId, turnId, itemId, text) {
    writeTranscriptLine("ERROR", createTranscriptScope({ threadId, turnId, itemId }), text);
  }

  function setSessionStatusByThread(threadId, status) {
    const session = store.getSessionByThreadId(threadId);
    if (!session) {
      throw createAssociationFailureError(`No session mapped for threadId ${threadId}.`, {
        threadId,
        status,
      });
    }
    store.updateSession(session, { status });
  }

  function appendThreadEvent(threadId, event) {
    return store.appendThreadEvent(threadId, event);
  }

  function bufferThreadMessage(threadId, entry) {
    const buffered = bufferedThreadMessages.get(threadId) ?? [];
    buffered.push(entry);
    bufferedThreadMessages.set(threadId, buffered);
  }

  function flushBufferedThreadMessages(threadId) {
    const buffered = bufferedThreadMessages.get(threadId) ?? [];
    if (buffered.length === 0) {
      return;
    }

    bufferedThreadMessages.delete(threadId);
    for (const entry of buffered) {
      if (entry.kind === "notification") {
        processNotification(entry.message);
        continue;
      }
      processServerRequest(entry.message);
    }
  }

  function processNotification(message) {
    const threadId = extractThreadId(message);
    if (!threadId) {
      throw createAssociationFailureError(
        `Notification ${message.method} is missing thread identity.`,
        {
          method: message.method,
          params: message.params ?? null,
        },
      );
    }

    const turnId = extractTurnId(message);
    const itemId = extractItemId(message);
    const payload = {
      method: message.method,
      params: message.params ?? {},
    };

    switch (message.method) {
      case "thread/started":
      case "thread/status/changed":
      case "thread/closed": {
        const status = normalizeSessionStatus(message.params?.thread?.status ?? message.params?.status ?? null);
        setSessionStatusByThread(threadId, status);
        writeTranscriptLine(
          "SESSION",
          createTranscriptScope({ threadId, turnId, itemId }),
          `${message.method} -> ${status}`,
        );
        appendThreadEvent(threadId, {
          type: "session_status",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "turn/started": {
        setSessionStatusByThread(threadId, "running");
        writeTurnTranscript(threadId, turnId, "started");
        appendThreadEvent(threadId, {
          type: "turn_started",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "turn/completed": {
        const status = normalizeTurnStatus(message.params?.turn?.status ?? null);
        setSessionStatusByThread(threadId, status);
        flushTranscriptBuffer("THINK", threadId, turnId, itemId);
        flushTranscriptBuffer("AGENT", threadId, turnId, itemId);
        flushTranscriptBuffer("TOOL", threadId, turnId, itemId);
        writeTurnTranscript(threadId, turnId, `completed -> ${status}`);
        appendThreadEvent(threadId, {
          type: "turn_completed",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "item/agentMessage/delta": {
        writeAgentTranscript(threadId, turnId, itemId, message.params?.delta ?? "");
        appendThreadEvent(threadId, {
          type: "message_delta",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "item/reasoning/summaryTextDelta":
      case "item/reasoning/textDelta": {
        writeReasoningTranscript(threadId, turnId, itemId, message.params?.delta ?? "");
        return;
      }
      case "item/reasoning/summaryPartAdded": {
        const summaryIndex = message.params?.summaryIndex;
        const detail = Number.isFinite(summaryIndex)
          ? `summary part ${summaryIndex} added`
          : "summary part added";
        writeReasoningTranscript(threadId, turnId, itemId, detail);
        return;
      }
      case "turn/plan/updated": {
        writeTurnTranscript(threadId, turnId, "plan updated");
        return;
      }
      case "turn/diff/updated": {
        writeTurnTranscript(threadId, turnId, "diff updated");
        return;
      }
      case "item/commandExecution/outputDelta":
      case "item/fileChange/outputDelta": {
        writeToolTranscript(threadId, turnId, itemId, message.params?.delta ?? "");
        return;
      }
      case "item/mcpToolCall/progress": {
        writeToolTranscript(threadId, turnId, itemId, message.params?.message ?? "");
        return;
      }
      case "item/started": {
        writeItemTranscript(threadId, turnId, itemId, `${extractTranscriptItemType(message)} started`);
        appendThreadEvent(threadId, {
          type: "item_started",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "item/completed": {
        writeItemTranscript(threadId, turnId, itemId, `${extractTranscriptItemType(message)} completed`);
        const flushedReasoning = flushTranscriptBuffer("THINK", threadId, turnId, itemId);
        const flushedAgent = flushTranscriptBuffer("AGENT", threadId, turnId, itemId);
        flushTranscriptBuffer("TOOL", threadId, turnId, itemId);
        const preview = extractAgentMessagePreview(message.params?.item ?? null);
        if (preview) {
          if (!flushedAgent) {
            writeTranscriptText("AGENT", createTranscriptScope({ threadId, turnId, itemId }), preview);
          }
          const session = store.getSessionByThreadId(threadId);
          if (!session) {
            throw createAssociationFailureError(
              `No session mapped for threadId ${threadId} while updating message preview.`,
              {
                method: message.method,
                threadId,
              },
            );
          }
          store.updateSession(session, { lastMessagePreview: preview });
          appendThreadEvent(threadId, {
            type: "message_completed",
            turnId,
            itemId,
            payload,
          });
          return;
        }

        appendThreadEvent(threadId, {
          type: "item_completed",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "serverRequest/resolved": {
        store.resolvePendingApproval(threadId, String(message.params?.requestId ?? ""));
        const session = store.getSessionByThreadId(threadId);
        if (!session) {
          throw createAssociationFailureError(
            `No session mapped for threadId ${threadId} while resolving approval.`,
            {
              method: message.method,
              threadId,
            },
          );
        }
        if (session.status === "waitingOnApproval") {
          store.updateSession(session, { status: "running" });
        }
        writeRequestTranscript(
          threadId,
          turnId,
          itemId,
          `${message.params?.requestId ?? "unknown"} resolved`,
        );
        appendThreadEvent(threadId, {
          type: "approval_resolved",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      case "error": {
        setSessionStatusByThread(threadId, "failed");
        writeErrorTranscript(threadId, turnId, itemId, message.params?.message ?? "app-server error");
        appendThreadEvent(threadId, {
          type: "error",
          turnId,
          itemId,
          payload,
        });
        return;
      }
      default:
        logger.debug({
          event: "bridge.notification.ignored",
          method: message.method,
          threadId,
        }, "ignoring app-server notification outside Slice 1 journal mapping");
    }
  }

  function handleNotification(message) {
    const threadId = extractThreadId(message);
    if (!threadId) {
      logger.debug({
        event: "bridge.notification.ignored_unscoped",
        method: message.method,
        params: message.params ?? null,
      }, "ignoring app-server notification without thread identity in current bridge slice");
      return;
    }
    if (!store.getSessionByThreadId(threadId)) {
      bufferThreadMessage(threadId, { kind: "notification", message });
      logger.debug({
        event: "bridge.notification.buffered",
        method: message.method,
        threadId,
      }, "buffered app-server notification until session mapping exists");
      return;
    }
    processNotification(message);
  }

  function processServerRequest(serverRequest) {
    const threadId = extractThreadId(serverRequest);
    if (!threadId) {
      throw createAssociationFailureError(
        `Server request ${serverRequest.method} is missing thread identity.`,
        {
          method: serverRequest.method,
          params: serverRequest.params ?? null,
        },
      );
    }

    const response = createUnsupportedServerRequestResponse(serverRequest);
    if (response.result) {
      const approval = createApprovalPayload(serverRequest);
      let resolutionDetail = "auto-resolved";
      if (serverRequest.method === "item/commandExecution/requestApproval") {
        resolutionDetail = "auto-decline command approval";
      } else if (serverRequest.method === "item/fileChange/requestApproval") {
        resolutionDetail = "auto-decline file approval";
      } else if (serverRequest.method === "item/tool/requestUserInput") {
        resolutionDetail = "auto-empty user input";
      } else if (serverRequest.method === "mcpServer/elicitation/request") {
        resolutionDetail = "auto-decline MCP elicitation";
      }
      writeRequestTranscript(
        threadId,
        approval.turnId,
        approval.itemId,
        `${serverRequest.method} -> ${resolutionDetail}`,
      );
      appendThreadEvent(threadId, {
        type: "approval_requested",
        turnId: approval.turnId,
        itemId: approval.itemId,
        payload: {
          method: serverRequest.method,
          params: serverRequest.params ?? {},
        },
      });
      appServerClient.respondResult(serverRequest.id, response.result);
      return;
    }
    writeErrorTranscript(
      threadId,
      extractTurnId(serverRequest),
      extractItemId(serverRequest),
      `${serverRequest.method} -> ${response.error.message}`,
    );
    appendThreadEvent(threadId, {
      type: "error",
      turnId: extractTurnId(serverRequest),
      itemId: extractItemId(serverRequest),
      payload: {
        method: serverRequest.method,
        message: response.error.message,
        code: response.error.code,
      },
    });
    appServerClient.respondError(serverRequest.id, response.error);
  }

  function handleServerRequest(serverRequest) {
    const threadId = extractThreadId(serverRequest);
    if (!threadId) {
      logger.error({
        event: "bridge.server_request.unscoped",
        method: serverRequest.method,
        params: serverRequest.params ?? null,
      }, "received app-server server request without thread identity");
      appServerClient.respondError(serverRequest.id, {
        code: -32600,
        message: `Unsupported unscoped server request ${serverRequest.method}.`,
        data: {
          method: serverRequest.method,
        },
      });
      return;
    }
    if (!store.getSessionByThreadId(threadId)) {
      bufferThreadMessage(threadId, { kind: "serverRequest", message: serverRequest });
      logger.debug({
        event: "bridge.server_request.buffered",
        method: serverRequest.method,
        threadId,
      }, "buffered app-server server request until session mapping exists");
      return;
    }
    processServerRequest(serverRequest);
  }

  function handleFatal(error) {
    health.appServerState = "failed";
    health.lastError = {
      code: error.code ?? "APP_SERVER_FATAL",
      message: error.message,
    };
    logger.error({
      event: "bridge.app_server.fatal",
      error,
    }, "codex app-server transport failed");
    onFatal?.(error);
  }

  return {
    async start() {
      appServerClient.onNotification(handleNotification);
      appServerClient.onServerRequest(handleServerRequest);
      appServerClient.onFatal(handleFatal);
      await appServerClient.start();
      health.appServerState = "ready";
      health.lastError = null;
    },
    async stop() {
      await appServerClient.stop();
      health.appServerState = "stopped";
    },
    getHealth() {
      return {
        appServerState: health.appServerState,
        lastError: health.lastError,
      };
    },
    listSessions(query) {
      const offset = parseOffsetCursor(query);
      const limit = parseLimit(query, {
        maxValue: MAX_SESSION_LIST_LIMIT,
        routeName: "session_list",
      });
      return store.listSessions({ offset, limit });
    },
    openSession(sessionId) {
      const session = store.getSessionById(sessionId);
      if (!session) {
        throw createBridgeRuntimeError(404, "SESSION_NOT_FOUND", `Unknown sessionId ${sessionId}.`);
      }
      return createSnapshotResponse(store, session);
    },
    pollSession(sessionId, query) {
      const session = store.getSessionById(sessionId);
      if (!session) {
        throw createBridgeRuntimeError(404, "SESSION_NOT_FOUND", `Unknown sessionId ${sessionId}.`);
      }

      const afterCursor = parseOptionalQueryString(query, "cursor");
      const limit = parseLimit(query, {
        maxValue: MAX_SESSION_POLL_LIMIT,
        routeName: "session_poll",
      });

      const eventPage = store.readSessionEvents(session, { afterCursor, limit });
      if (!eventPage.cursorFound) {
        throw createQueryValidationError("session_poll cursor was not found in the current in-memory journal.", {
          sessionId,
          cursor: afterCursor,
        });
      }

      return {
        session: store.toSessionSummary(session),
        events: eventPage.events,
        nextCursor: eventPage.nextCursor,
        hasMore: eventPage.hasMore,
      };
    },
    async createSession(requestBody) {
      const cwd = resolveSessionCwd(requestBody, config.defaultSessionCwd);
      let threadStartResponse;
      try {
        threadStartResponse = await appServerClient.threadStart({
          cwd,
          ephemeral: false,
        });
      } catch (error) {
        throw mapAppServerError("thread/start", error);
      }
      const threadId = threadStartResponse?.thread?.id;
      if (typeof threadId !== "string" || threadId.length === 0) {
        throw createBridgeRuntimeError(
          502,
          "APP_SERVER_INVALID_RESPONSE",
          "thread/start did not return a thread id.",
          threadStartResponse ?? null,
        );
      }
      if (threadStartResponse?.thread?.cwd !== cwd) {
        throw createBridgeRuntimeError(
          502,
          "APP_SERVER_INVALID_RESPONSE",
          "thread/start response cwd does not match the resolved session cwd.",
          {
            expectedCwd: cwd,
            actualCwd: threadStartResponse?.thread?.cwd ?? null,
          },
        );
      }

      const title = typeof requestBody?.title === "string" && requestBody.title.trim().length > 0
        ? requestBody.title.trim()
        : null;
      const session = store.createSession({ title, threadId, cwd });
      store.updateSession(session, {
        status: normalizeSessionStatus(threadStartResponse?.thread?.status ?? null),
      });
      flushBufferedThreadMessages(threadId);

      return createSnapshotResponse(store, session);
    },
    async sendMessage(sessionId, requestBody) {
      const session = store.getSessionById(sessionId);
      if (!session) {
        throw createBridgeRuntimeError(404, "SESSION_NOT_FOUND", `Unknown sessionId ${sessionId}.`);
      }

      const text = typeof requestBody?.text === "string" ? requestBody.text.trim() : "";
      if (text.length === 0) {
        throw createBridgeRuntimeError(400, "BAD_REQUEST", "message_send requires a non-empty text field.");
      }

      writeTranscriptText("USER", createTranscriptScope({ threadId: session.threadId }), text);

      if (typeof session.cwd !== "string" || session.cwd.length === 0) {
        throw createBridgeRuntimeError(
          500,
          "SESSION_INVALID",
          `Session ${sessionId} is missing a resolved cwd.`,
        );
      }

      let turnStartResponse;
      try {
        turnStartResponse = await appServerClient.turnStart({
          cwd: session.cwd,
          threadId: session.threadId,
          input: [
            {
              type: "text",
              text,
              textElements: [],
            },
          ],
        });
      } catch (error) {
        throw mapAppServerError("turn/start", error);
      }

      store.updateSession(session, {
        lastMessagePreview: text,
        status: "running",
      });

      return {
        sessionId,
        accepted: true,
        messageId: randomUUID(),
        turnId: typeof turnStartResponse?.turn?.id === "string" ? turnStartResponse.turn.id : null,
        nextCursor: session.cursor,
      };
    },
  };
}
