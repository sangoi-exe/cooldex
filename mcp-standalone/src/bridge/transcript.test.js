import assert from "node:assert/strict";
import test from "node:test";

import { formatTranscriptLine } from "./transcript.js";

test("formatTranscriptLine includes GMT-3 HH:mm:ss|DD-MM-YYYY timestamp and operator user id", () => {
  const line = formatTranscriptLine({
    channel: "SESSION",
    occurredAt: Date.UTC(2026, 2, 25, 12, 34, 56, 789),
    scope: {
      sessionId: "db3b040b",
      turnId: "-",
      itemId: "-",
      operatorUserId: "12345",
    },
    text: "thread/started -> idle",
  });

  assert.equal(
    line,
    "SESSION [09:34:56|25-03-2026] [NS:12345] [s:db3b040b t:- i:-] thread/started -> idle",
  );
});

test("formatTranscriptLine falls back to NS:- when user id is missing", () => {
  const line = formatTranscriptLine({
    channel: "USER",
    occurredAt: 0,
    scope: {
      sessionId: "db3b040b",
      turnId: "019d2531",
      itemId: "-",
      operatorUserId: null,
    },
    text: "teste",
  });

  assert.equal(
    line,
    "USER    [21:00:00|31-12-1969] [NS:-] [s:db3b040b t:019d2531 i:-] teste",
  );
});

test("formatTranscriptLine fails loud on invalid explicit timestamp", () => {
  assert.throws(
    () => formatTranscriptLine({
      channel: "ITEM",
      occurredAt: Number.NaN,
      scope: {
        sessionId: "db3b040b",
        turnId: "019d2531",
        itemId: "e6be78db",
        operatorUserId: "12345",
      },
      text: "userMessage completed",
    }),
    /Invalid transcript timestamp/,
  );
});
