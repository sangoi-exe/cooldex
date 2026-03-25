import express from "express";

function readRequestId(req) {
  const headerValue = req.get("X-Request-Id");
  if (typeof headerValue === "string" && headerValue.trim().length > 0) {
    return headerValue.trim();
  }

  const bodyValue = req.body?.requestId;
  if (typeof bodyValue === "string" && bodyValue.trim().length > 0) {
    return bodyValue.trim();
  }

  return null;
}

function sendBridgeError(req, res, status, code, message, details = null) {
  res.status(status).json({
    requestId: readRequestId(req),
    error: {
      code,
      message,
      details,
    },
  });
}

function requireBridgeAuth(config) {
  const configuredBridgeBearerToken =
    typeof config.bridgeBearerToken === "string" && config.bridgeBearerToken.trim().length > 0
      ? config.bridgeBearerToken.trim()
      : null;
  return (req, res, next) => {
    if (configuredBridgeBearerToken === null) {
      sendBridgeError(req, res, 503, "NOT_CONFIGURED", "Bridge bearer auth is not configured yet.");
      return;
    }

    const authorization = req.get("Authorization");
    if (typeof authorization !== "string" || !authorization.startsWith("Bearer ")) {
      sendBridgeError(req, res, 401, "UNAUTHORIZED", "Missing or invalid Authorization header.");
      return;
    }

    const token = authorization.slice("Bearer ".length).trim();
    if (token !== configuredBridgeBearerToken) {
      sendBridgeError(req, res, 403, "FORBIDDEN", "Bridge bearer token was rejected.");
      return;
    }

    next();
  };
}

function asyncRoute(handler) {
  return async (req, res, next) => {
    try {
      await handler(req, res);
    } catch (error) {
      next(error);
    }
  };
}

export function createApp({ config, logger, bridgeRuntime }) {
  const app = express();

  app.disable("x-powered-by");
  app.use(express.json({ limit: "1mb" }));

  app.use((req, _res, next) => {
    req.log = logger.child({
      method: req.method,
      path: req.originalUrl,
      requestId: readRequestId(req),
    });
    next();
  });

  app.get("/healthz", (_req, res) => {
    res.json({
      ok: true,
      service: "codex-netsuite-bridge",
      bridgeBasePath: config.bridgeBasePath,
      authConfigured:
        typeof config.bridgeBearerToken === "string" && config.bridgeBearerToken.trim().length > 0,
      defaultSessionCwd: config.defaultSessionCwd,
      defaultSessionConfigPath: config.defaultSessionConfigPath,
      bridgeStateDbPath: config.bridgeStateDbPath,
      runtime: bridgeRuntime.getHealth(),
    });
  });

  const router = express.Router();
  router.use(requireBridgeAuth(config));

  // Merge-safety anchor: these route bindings define the published bridge contract in
  // `mcp-standalone/README.md`; keep paths and runtime method wiring aligned.
  router.get("/sessions", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.listSessions(req.query ?? {});
    res.json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  router.get("/sessions/:sessionId", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.openSession(req.params.sessionId);
    res.json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  router.get("/sessions/:sessionId/events", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.pollSession(req.params.sessionId, req.query ?? {});
    res.json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  router.post("/sessions", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.createSession(req.body ?? {});
    res.status(201).json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  router.post("/sessions/:sessionId/messages", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.sendMessage(req.params.sessionId, req.body ?? {});
    res.status(202).json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  router.post("/sessions/:sessionId/title", asyncRoute(async (req, res) => {
    const response = await bridgeRuntime.renameSession(req.params.sessionId, req.body ?? {});
    res.json({
      requestId: readRequestId(req),
      ...response,
    });
  }));

  app.use(config.bridgeBasePath, router);

  app.use((error, req, res, next) => {
    if (res.headersSent) {
      next(error);
      return;
    }

    if (error instanceof SyntaxError && "body" in error) {
      sendBridgeError(req, res, 400, "BAD_REQUEST", "Request body is not valid JSON.");
      return;
    }

    if (typeof error?.status === "number" && typeof error?.code === "string") {
      sendBridgeError(req, res, error.status, error.code, error.message, error.details ?? null);
      return;
    }

    req.log?.error({ error }, "unhandled bridge error");
    sendBridgeError(req, res, 500, "INTERNAL_ERROR", "Standalone bridge runtime failed unexpectedly.");
  });

  app.use((req, res) => {
    sendBridgeError(req, res, 404, "NOT_FOUND", `No route matches ${req.method} ${req.path}.`);
  });

  return app;
}
