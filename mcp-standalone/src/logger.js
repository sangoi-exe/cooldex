import pino from "pino";

// Merge anchor: explicit truncation markers are part of bridge transcript and
// persisted-tool-output semantics; do not silently clip without a marker.
const DEFAULT_TRUNCATION_SUFFIX = " …[truncated]";

export function truncateLogText(value, maxChars, suffix = DEFAULT_TRUNCATION_SUFFIX) {
  if (typeof value !== "string") {
    return value;
  }
  if (!Number.isFinite(maxChars) || maxChars <= 0 || value.length <= maxChars) {
    return value;
  }

  if (suffix.length >= maxChars) {
    return value.slice(0, maxChars);
  }

  return `${value.slice(0, maxChars - suffix.length)}${suffix}`;
}

export function createLogger() {
  return pino({
    level: process.env.LOG_LEVEL?.trim() || "info",
    base: undefined,
  });
}
