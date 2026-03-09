import "dotenv/config";
import { realpathSync, statSync } from "node:fs";
import { isAbsolute } from "node:path";

function parsePositiveInteger(rawValue, fallback) {
  const parsed = Number.parseInt(rawValue ?? "", 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }
  return parsed;
}

function parseBooleanEnv(name, fallback) {
  const rawValue = process.env[name];
  if (rawValue === undefined) {
    return fallback;
  }

  const normalized = rawValue.trim().toLowerCase();
  if (normalized.length === 0) {
    return fallback;
  }
  if (["1", "true", "yes", "on"].includes(normalized)) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(normalized)) {
    return false;
  }

  throw new Error(`Invalid boolean value for ${name}: ${rawValue}`);
}

function resolveDirectoryPath(name, rawValue, fallback) {
  const candidate = typeof rawValue === "string" && rawValue.trim().length > 0
    ? rawValue.trim()
    : fallback;
  if (!isAbsolute(candidate)) {
    throw new Error(`${name} must be an absolute directory path. Received: ${candidate}`);
  }

  let stats;
  try {
    stats = statSync(candidate);
  } catch (error) {
    throw new Error(`${name} must point to an existing directory. Received: ${candidate}`);
  }
  if (!stats.isDirectory()) {
    throw new Error(`${name} must point to a directory. Received: ${candidate}`);
  }

  return realpathSync(candidate);
}

export function loadConfig() {
  return {
    port: parsePositiveInteger(process.env.PORT, 8787),
    bridgeBasePath: process.env.BRIDGE_BASE_PATH?.trim() || "/api/codex/v1",
    bridgeBearerToken: process.env.BRIDGE_BEARER_TOKEN?.trim() || null,
    bridgeDebugTranscript: parseBooleanEnv("BRIDGE_DEBUG_TRANSCRIPT", false),
    defaultSessionCwd: resolveDirectoryPath(
      "BRIDGE_DEFAULT_SESSION_CWD",
      process.env.BRIDGE_DEFAULT_SESSION_CWD,
      "/home/lucas/work/avmb-plus",
    ),
    codexCommand: process.env.CODEX_COMMAND?.trim() || "codex",
    codexArgs: ["app-server", "--listen", "stdio://"],
    appServerStartupTimeoutMs: parsePositiveInteger(process.env.APP_SERVER_STARTUP_TIMEOUT_MS, 15000),
    appServerRequestTimeoutMs: parsePositiveInteger(process.env.APP_SERVER_REQUEST_TIMEOUT_MS, 30000),
    appServerShutdownTimeoutMs: parsePositiveInteger(process.env.APP_SERVER_SHUTDOWN_TIMEOUT_MS, 5000),
    clientInfo: {
      name: "codex_netsuite_bridge",
      title: "Codex NetSuite Bridge",
      version: "0.1.0",
    },
  };
}
