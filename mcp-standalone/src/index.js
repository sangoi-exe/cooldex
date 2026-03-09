import { createApp } from "./app.js";
import { createBridgeRuntime } from "./bridge/runtime.js";
import { loadConfig } from "./config.js";
import { createLogger } from "./logger.js";

async function main() {
  const config = loadConfig();
  const logger = createLogger();

  let isShuttingDown = false;
  let server = null;

  async function stopRuntimeAndExit(exitCode, reason, error = null) {
    if (isShuttingDown) {
      return;
    }
    isShuttingDown = true;

    logger.error(
      {
        event: "bridge.runtime.fatal_exit",
        reason,
        error,
      },
      "standalone bridge runtime is exiting after a fatal failure",
    );

    if (server) {
      await new Promise((resolve) => {
        server.close(resolve);
      });
      logger.info({ event: "bridge.http.closed", reason }, "standalone bridge http server closed");
      server = null;
    }

    process.exit(exitCode);
  }

  const bridgeRuntime = createBridgeRuntime({
    config,
    logger,
    onFatal(error) {
      void stopRuntimeAndExit(1, "app_server_fatal", error);
    },
  });

  await bridgeRuntime.start();

  const app = createApp({ config, logger, bridgeRuntime });
  server = app.listen(config.port, () => {
    logger.info(
      {
        event: "bridge.runtime.ready",
        port: config.port,
        bridgeBasePath: config.bridgeBasePath,
        codexCommand: config.codexCommand,
      },
      "standalone bridge runtime slice ready",
    );
  });

  async function shutdown(signal) {
    if (isShuttingDown) {
      return;
    }
    isShuttingDown = true;

    logger.info({ event: "bridge.runtime.shutdown", signal }, "shutting down standalone bridge runtime");

    if (server) {
      await new Promise((resolve) => {
        server.close(resolve);
      });
      logger.info({ event: "bridge.http.closed", signal }, "standalone bridge http server closed");
      server = null;
    }

    await bridgeRuntime.stop();
    process.exit(0);
  }

  function handleProcessFatal(origin) {
    return (error) => {
      const normalizedError = error instanceof Error ? error : new Error(String(error));
      void stopRuntimeAndExit(1, origin, normalizedError);
    };
  }

  process.once("uncaughtException", handleProcessFatal("uncaught_exception"));
  process.once("unhandledRejection", handleProcessFatal("unhandled_rejection"));

  process.once("SIGINT", () => {
    void shutdown("SIGINT");
  });
  process.once("SIGTERM", () => {
    void shutdown("SIGTERM");
  });
}

main().catch((error) => {
  const logger = createLogger();
  logger.error({ error }, "failed to start standalone bridge runtime");
  process.exit(1);
});
