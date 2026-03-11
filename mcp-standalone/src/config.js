import "dotenv/config";
import { realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";

export const DEFAULT_BRIDGE_STATE_DB_PATH = join(
  homedir(),
  ".codex",
  "codex-netsuite-bridge",
  "bridge-state.sqlite",
);

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

function resolveOptionalFilePath(name, rawValue) {
  if (typeof rawValue !== "string" || rawValue.trim().length === 0) {
    return null;
  }

  const candidate = rawValue.trim();
  if (!isAbsolute(candidate)) {
    throw new Error(`${name} must be an absolute file path. Received: ${candidate}`);
  }

  let stats;
  try {
    stats = statSync(candidate);
  } catch {
    throw new Error(`${name} must point to an existing file. Received: ${candidate}`);
  }
  if (!stats.isFile()) {
    throw new Error(`${name} must point to a file. Received: ${candidate}`);
  }

  return realpathSync(candidate);
}

export function resolveBridgeStateDbPath(rawValue) {
  const candidate = typeof rawValue === "string" && rawValue.trim().length > 0
    ? rawValue.trim()
    : DEFAULT_BRIDGE_STATE_DB_PATH;
  if (!isAbsolute(candidate)) {
    throw new Error(`BRIDGE_STATE_DB_PATH must be an absolute file path. Received: ${candidate}`);
  }

  let fileStats;
  try {
    fileStats = statSync(candidate);
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw new Error(`BRIDGE_STATE_DB_PATH must resolve to a writable file path. Received: ${candidate}`);
    }
  }
  if (fileStats && !fileStats.isFile()) {
    throw new Error(`BRIDGE_STATE_DB_PATH must point to a file path. Received: ${candidate}`);
  }

  let parentStats;
  const parentDirectory = dirname(candidate);
  try {
    parentStats = statSync(parentDirectory);
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw new Error(`BRIDGE_STATE_DB_PATH parent path is invalid: ${parentDirectory}`);
    }
  }
  if (parentStats && !parentStats.isDirectory()) {
    throw new Error(`BRIDGE_STATE_DB_PATH parent path must be a directory: ${parentDirectory}`);
  }

  return candidate;
}

export function loadConfig() {
  return {
    port: parsePositiveInteger(process.env.PORT, 8787),
    bridgeBasePath: process.env.BRIDGE_BASE_PATH?.trim() || "/api/codex/v1",
    bridgeBearerToken: process.env.BRIDGE_BEARER_TOKEN?.trim() || null,
    bridgeDebugTranscript: parseBooleanEnv("BRIDGE_DEBUG_TRANSCRIPT", false),
    // Merge anchor: these defaults are consumed by runtime `session_create`
    // resolution and documented route semantics.
    defaultSessionCwd: resolveDirectoryPath(
      "BRIDGE_DEFAULT_SESSION_CWD",
      process.env.BRIDGE_DEFAULT_SESSION_CWD,
      "/home/lucas/work/avmb-plus",
    ),
    defaultSessionConfigPath: resolveOptionalFilePath(
      "BRIDGE_DEFAULT_SESSION_CONFIG_PATH",
      process.env.BRIDGE_DEFAULT_SESSION_CONFIG_PATH,
    ),
    bridgeStateDbPath: resolveBridgeStateDbPath(process.env.BRIDGE_STATE_DB_PATH),
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
