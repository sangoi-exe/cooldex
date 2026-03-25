// Merge-safety anchor: transcript prefix formatting defines the operator-visible
// stderr contract shared with runtime wiring, tests, and README notes.

const GMT_MINUS_3_OFFSET_MS = 3 * 60 * 60 * 1000;

function formatTranscriptTimestamp(occurredAt = Date.now()) {
  const timestamp = new Date(occurredAt);
  if (Number.isNaN(timestamp.valueOf())) {
    throw new Error(`Invalid transcript timestamp: ${occurredAt}`);
  }
  const shiftedTimestamp = new Date(timestamp.valueOf() - GMT_MINUS_3_OFFSET_MS);
  const hours = String(shiftedTimestamp.getUTCHours()).padStart(2, "0");
  const minutes = String(shiftedTimestamp.getUTCMinutes()).padStart(2, "0");
  const seconds = String(shiftedTimestamp.getUTCSeconds()).padStart(2, "0");
  const day = String(shiftedTimestamp.getUTCDate()).padStart(2, "0");
  const month = String(shiftedTimestamp.getUTCMonth() + 1).padStart(2, "0");
  const year = String(shiftedTimestamp.getUTCFullYear()).padStart(4, "0");
  return `${hours}:${minutes}:${seconds}|${day}-${month}-${year}`;
}

function formatTranscriptUserTag(operatorUserId) {
  return typeof operatorUserId === "string" && operatorUserId.length > 0
    ? `NS:${operatorUserId}`
    : "NS:-";
}

export function formatTranscriptLine({ channel, occurredAt = Date.now(), scope, text }) {
  return `${channel.padEnd(7)} [${formatTranscriptTimestamp(occurredAt)}] `
    + `[${formatTranscriptUserTag(scope.operatorUserId)}] `
    + `[s:${scope.sessionId} t:${scope.turnId} i:${scope.itemId}] ${text}`;
}
