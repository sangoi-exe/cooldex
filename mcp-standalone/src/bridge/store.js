import { randomUUID } from "node:crypto";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

import Database from "better-sqlite3";

import { resolveBridgeStateDbPath } from "../config.js";

// Merge anchor: these aliases define the persisted session shape consumed by
// runtime/session APIs; keep names aligned with runtime + README contracts.
const SESSION_SELECT_COLUMNS = [
  "session_id AS sessionId",
  "thread_id AS threadId",
  "title",
  "operator_json AS operatorJson",
  "cwd",
  "config_path AS configPath",
  "status",
  "created_at AS createdAt",
  "updated_at AS updatedAt",
  "last_message_preview AS lastMessagePreview",
  "cursor",
  "next_cursor_index AS nextCursorIndex",
].join(", ");

function createAssociationError(message, details) {
  const error = new Error(message);
  error.code = "SESSION_ASSOCIATION_FAILED";
  error.details = details;
  return error;
}

function isPlainObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function assertNonEmptyString(value, fieldName) {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${fieldName} must be a non-empty string.`);
  }
  return value;
}

function assertNullableString(value, fieldName) {
  if (value === null) {
    return null;
  }
  if (typeof value !== "string") {
    throw new Error(`${fieldName} must be a string or null.`);
  }
  return value;
}

function assertNullableNonEmptyString(value, fieldName) {
  if (value === null) {
    return null;
  }
  return assertNonEmptyString(value, fieldName);
}

function assertInteger(value, fieldName, minimum = null) {
  if (!Number.isInteger(value)) {
    throw new Error(`${fieldName} must be an integer.`);
  }
  if (minimum !== null && value < minimum) {
    throw new Error(`${fieldName} must be greater than or equal to ${minimum}.`);
  }
  return value;
}

function parseJsonObject(rawValue, fieldName, { allowNull = false } = {}) {
  if (rawValue === null) {
    if (allowNull) {
      return null;
    }
    throw new Error(`Persisted ${fieldName} is null but null is not allowed.`);
  }
  if (typeof rawValue !== "string") {
    throw new Error(`Persisted ${fieldName} must be a JSON string.`);
  }

  let parsed;
  try {
    parsed = JSON.parse(rawValue);
  } catch (error) {
    throw new Error(`Failed to parse persisted ${fieldName}: ${error.message}`);
  }

  if (parsed === null) {
    if (allowNull) {
      return null;
    }
    throw new Error(`Persisted ${fieldName} is null but null is not allowed.`);
  }
  if (!isPlainObject(parsed)) {
    throw new Error(`Persisted ${fieldName} must decode to a JSON object.`);
  }
  return parsed;
}

function stringifyJsonObject(value, fieldName, { allowNull = false } = {}) {
  if (value === null) {
    if (allowNull) {
      return null;
    }
    throw new Error(`${fieldName} cannot be null.`);
  }
  if (!isPlainObject(value)) {
    throw new Error(`${fieldName} must be a JSON object.`);
  }

  let serialized;
  try {
    serialized = JSON.stringify(value);
  } catch (error) {
    throw new Error(`Failed to serialize ${fieldName}: ${error.message}`);
  }
  if (typeof serialized !== "string") {
    throw new Error(`Failed to serialize ${fieldName}: JSON.stringify returned a non-string value.`);
  }
  return serialized;
}

function readCount(rawValue, fieldName) {
  if (!Number.isInteger(rawValue) || rawValue < 0) {
    throw new Error(`Persisted ${fieldName} must be a non-negative integer.`);
  }
  return rawValue;
}

function cloneSessionOperator(operator) {
  if (operator === null || operator === undefined) {
    return null;
  }
  if (!isPlainObject(operator)) {
    throw new Error("session operator metadata must be an object or null.");
  }
  return { ...operator };
}

function normalizeSessionForStorage(session) {
  return {
    sessionId: assertNonEmptyString(session.sessionId, "session.sessionId"),
    threadId: assertNonEmptyString(session.threadId, "session.threadId"),
    title: assertNullableString(session.title, "session.title"),
    operator: cloneSessionOperator(session.operator),
    cwd: assertNonEmptyString(session.cwd, "session.cwd"),
    configPath: assertNullableNonEmptyString(session.configPath, "session.configPath"),
    status: assertNonEmptyString(session.status, "session.status"),
    createdAt: assertInteger(session.createdAt, "session.createdAt", 0),
    updatedAt: assertInteger(session.updatedAt, "session.updatedAt", 0),
    lastMessagePreview: assertNullableString(session.lastMessagePreview, "session.lastMessagePreview"),
    cursor: assertNullableNonEmptyString(session.cursor, "session.cursor"),
    nextCursorIndex: assertInteger(session.nextCursorIndex, "session.nextCursorIndex", 0),
  };
}

function normalizeEventInput(entry, nextTimestamp, observeTimestamp) {
  if (!isPlainObject(entry)) {
    throw new Error("event entry must be an object.");
  }

  const type = assertNonEmptyString(entry.type, "event.type");
  const turnId = entry.turnId === undefined ? null : assertNullableNonEmptyString(entry.turnId, "event.turnId");
  const itemId = entry.itemId === undefined ? null : assertNullableNonEmptyString(entry.itemId, "event.itemId");

  const payload = entry.payload === undefined ? {} : entry.payload;
  if (!isPlainObject(payload)) {
    throw new Error("event.payload must be an object.");
  }

  let occurredAt;
  if (entry.occurredAt === undefined || entry.occurredAt === null) {
    occurredAt = nextTimestamp();
  } else {
    occurredAt = assertInteger(entry.occurredAt, "event.occurredAt", 0);
    observeTimestamp(occurredAt);
  }

  return {
    type,
    turnId,
    itemId,
    payload,
    occurredAt,
  };
}

function normalizeApprovalInput(approval, nextTimestamp, observeTimestamp) {
  if (!isPlainObject(approval)) {
    throw new Error("approval payload must be an object.");
  }
  const requestId = assertNonEmptyString(approval.requestId, "approval.requestId");

  const createdAt = Number.isInteger(approval.createdAt)
    ? assertInteger(approval.createdAt, "approval.createdAt", 0)
    : nextTimestamp();
  observeTimestamp(createdAt);

  return {
    requestId,
    approval: { ...approval },
    createdAt,
  };
}

function toSessionSummary(session) {
  if (!Array.isArray(session.pendingApprovals)) {
    throw new Error("session.pendingApprovals must be an array.");
  }

  return {
    sessionId: session.sessionId,
    title: session.title,
    operator: cloneSessionOperator(session.operator),
    cwd: session.cwd,
    configPath: session.configPath,
    status: session.status,
    createdAt: session.createdAt,
    updatedAt: session.updatedAt,
    hasPendingApprovals: session.pendingApprovals.length > 0,
    lastMessagePreview: session.lastMessagePreview,
  };
}

function toSnapshot(session) {
  return {
    cursor: session.cursor,
    events: [...session.events],
    pendingApprovals: [...session.pendingApprovals],
    touchedRecords: [],
  };
}

export function createBridgeStore(options = {}) {
  if (!isPlainObject(options)) {
    throw new Error("createBridgeStore options must be an object.");
  }

  const bridgeStateDbPath = resolveBridgeStateDbPath(options.bridgeStateDbPath ?? process.env.BRIDGE_STATE_DB_PATH);
  mkdirSync(dirname(bridgeStateDbPath), { recursive: true });

  const database = new Database(bridgeStateDbPath);
  database.pragma("journal_mode = WAL");
  database.pragma("busy_timeout = 5000");
  database.pragma("foreign_keys = ON");

  database.exec(`
    CREATE TABLE IF NOT EXISTS sessions (
      session_id TEXT PRIMARY KEY,
      thread_id TEXT NOT NULL UNIQUE,
      title TEXT,
      operator_json TEXT,
      cwd TEXT NOT NULL,
      config_path TEXT,
      status TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      last_message_preview TEXT,
      cursor TEXT,
      next_cursor_index INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS session_events (
      event_id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL,
      cursor TEXT NOT NULL,
      cursor_index INTEGER NOT NULL,
      occurred_at INTEGER NOT NULL,
      type TEXT NOT NULL,
      turn_id TEXT,
      item_id TEXT,
      payload_json TEXT NOT NULL,
      FOREIGN KEY (session_id) REFERENCES sessions (session_id) ON DELETE CASCADE,
      UNIQUE (session_id, cursor),
      UNIQUE (session_id, cursor_index)
    );

    CREATE TABLE IF NOT EXISTS pending_approvals (
      approval_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
      session_id TEXT NOT NULL,
      request_id TEXT NOT NULL,
      approval_json TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      FOREIGN KEY (session_id) REFERENCES sessions (session_id) ON DELETE CASCADE
    );

    CREATE INDEX IF NOT EXISTS session_events_session_cursor_index_idx
      ON session_events (session_id, cursor_index);

    CREATE INDEX IF NOT EXISTS pending_approvals_session_row_idx
      ON pending_approvals (session_id, approval_row_id);
  `);

  const selectSessionByIdStatement = database.prepare(
    `SELECT ${SESSION_SELECT_COLUMNS} FROM sessions WHERE session_id = ?`,
  );
  const selectSessionByThreadIdStatement = database.prepare(
    `SELECT ${SESSION_SELECT_COLUMNS} FROM sessions WHERE thread_id = ?`,
  );
  const selectSessionCountStatement = database.prepare("SELECT COUNT(*) AS count FROM sessions");
  const selectRecoverableSessionCountStatement = database.prepare(
    `SELECT COUNT(*) AS count FROM sessions WHERE status IN ('running', 'waitingOnApproval', 'waitingOnUserInput')`,
  );
  const selectPendingApprovalsCountStatement = database.prepare("SELECT COUNT(*) AS count FROM pending_approvals");
  const selectPendingApprovalsCountBySessionIdStatement = database.prepare(
    "SELECT COUNT(*) AS count FROM pending_approvals WHERE session_id = ?",
  );
  const listSessionsPageStatement = database.prepare(
    `SELECT ${SESSION_SELECT_COLUMNS}
     FROM sessions
     ORDER BY updated_at DESC, created_at DESC, session_id ASC
     LIMIT ? OFFSET ?`,
  );

  const insertSessionStatement = database.prepare(`
    INSERT INTO sessions (
      session_id,
      thread_id,
      title,
      operator_json,
      cwd,
      config_path,
      status,
      created_at,
      updated_at,
      last_message_preview,
      cursor,
      next_cursor_index
    ) VALUES (
      @sessionId,
      @threadId,
      @title,
      @operatorJson,
      @cwd,
      @configPath,
      @status,
      @createdAt,
      @updatedAt,
      @lastMessagePreview,
      @cursor,
      @nextCursorIndex
    )
  `);

  const updateSessionStatement = database.prepare(`
    UPDATE sessions
    SET title = @title,
        operator_json = @operatorJson,
        cwd = @cwd,
        config_path = @configPath,
        status = @status,
        updated_at = @updatedAt,
        last_message_preview = @lastMessagePreview,
        cursor = @cursor,
        next_cursor_index = @nextCursorIndex
    WHERE session_id = @sessionId
  `);

  const updateSessionCursorStatement = database.prepare(`
    UPDATE sessions
    SET cursor = @cursor,
        next_cursor_index = @nextCursorIndex,
        updated_at = @updatedAt
    WHERE session_id = @sessionId
  `);

  const touchSessionStatement = database.prepare(
    "UPDATE sessions SET updated_at = ? WHERE session_id = ?",
  );

  const interruptRecoverableSessionsStatement = database.prepare(`
    UPDATE sessions
    SET status = 'interrupted', updated_at = ?
    WHERE status IN ('running', 'waitingOnApproval', 'waitingOnUserInput')
  `);

  const selectEventsBySessionIdStatement = database.prepare(`
    SELECT
      event_id AS eventId,
      session_id AS sessionId,
      cursor,
      cursor_index AS cursorIndex,
      occurred_at AS occurredAt,
      type,
      turn_id AS turnId,
      item_id AS itemId,
      payload_json AS payloadJson
    FROM session_events
    WHERE session_id = ?
    ORDER BY cursor_index ASC
  `);

  const insertEventStatement = database.prepare(`
    INSERT INTO session_events (
      event_id,
      session_id,
      cursor,
      cursor_index,
      occurred_at,
      type,
      turn_id,
      item_id,
      payload_json
    ) VALUES (
      @eventId,
      @sessionId,
      @cursor,
      @cursorIndex,
      @occurredAt,
      @type,
      @turnId,
      @itemId,
      @payloadJson
    )
  `);

  const selectPendingApprovalsBySessionIdStatement = database.prepare(`
    SELECT
      approval_row_id AS approvalRowId,
      approval_json AS approvalJson
    FROM pending_approvals
    WHERE session_id = ?
    ORDER BY approval_row_id ASC
  `);

  const insertPendingApprovalStatement = database.prepare(`
    INSERT INTO pending_approvals (
      session_id,
      request_id,
      approval_json,
      created_at
    ) VALUES (
      @sessionId,
      @requestId,
      @approvalJson,
      @createdAt
    )
  `);

  const deletePendingApprovalsByRequestIdStatement = database.prepare(
    "DELETE FROM pending_approvals WHERE session_id = ? AND request_id = ?",
  );
  const clearPendingApprovalsStatement = database.prepare("DELETE FROM pending_approvals");

  const selectMaxTimestampStatement = database.prepare(`
    SELECT MAX(value) AS maxValue
    FROM (
      SELECT MAX(created_at) AS value FROM sessions
      UNION ALL
      SELECT MAX(updated_at) AS value FROM sessions
      UNION ALL
      SELECT MAX(occurred_at) AS value FROM session_events
      UNION ALL
      SELECT MAX(created_at) AS value FROM pending_approvals
    )
  `);

  function readInitialLastTimestamp() {
    const row = selectMaxTimestampStatement.get();
    if (!row || row.maxValue === null) {
      return 0;
    }
    return assertInteger(row.maxValue, "persisted timestamp", 0);
  }

  let lastTimestamp = readInitialLastTimestamp();

  function observeTimestamp(candidate) {
    if (Number.isInteger(candidate) && candidate > lastTimestamp) {
      lastTimestamp = candidate;
    }
  }

  function nextTimestamp() {
    const current = Date.now();
    lastTimestamp = current > lastTimestamp ? current : lastTimestamp + 1;
    return lastTimestamp;
  }

  function hydrateSessionBase(row) {
    if (!row || typeof row !== "object") {
      throw new Error("Malformed persisted session row.");
    }

    return {
      sessionId: assertNonEmptyString(row.sessionId, "sessions.session_id"),
      threadId: assertNonEmptyString(row.threadId, "sessions.thread_id"),
      title: assertNullableString(row.title, "sessions.title"),
      operator: cloneSessionOperator(parseJsonObject(row.operatorJson, "sessions.operator_json", { allowNull: true })),
      cwd: assertNonEmptyString(row.cwd, "sessions.cwd"),
      configPath: assertNullableNonEmptyString(row.configPath, "sessions.config_path"),
      status: assertNonEmptyString(row.status, "sessions.status"),
      createdAt: assertInteger(row.createdAt, "sessions.created_at", 0),
      updatedAt: assertInteger(row.updatedAt, "sessions.updated_at", 0),
      lastMessagePreview: assertNullableString(row.lastMessagePreview, "sessions.last_message_preview"),
      cursor: assertNullableNonEmptyString(row.cursor, "sessions.cursor"),
      nextCursorIndex: assertInteger(row.nextCursorIndex, "sessions.next_cursor_index", 0),
    };
  }

  function hydrateEvent(row) {
    if (!row || typeof row !== "object") {
      throw new Error("Malformed persisted event row.");
    }

    return {
      eventId: assertNonEmptyString(row.eventId, "session_events.event_id"),
      cursor: assertNonEmptyString(row.cursor, "session_events.cursor"),
      sessionId: assertNonEmptyString(row.sessionId, "session_events.session_id"),
      occurredAt: assertInteger(row.occurredAt, "session_events.occurred_at", 0),
      type: assertNonEmptyString(row.type, "session_events.type"),
      turnId: assertNullableNonEmptyString(row.turnId, "session_events.turn_id"),
      itemId: assertNullableNonEmptyString(row.itemId, "session_events.item_id"),
      payload: parseJsonObject(row.payloadJson, "session_events.payload_json"),
      cursorIndex: assertInteger(row.cursorIndex, "session_events.cursor_index", 1),
    };
  }

  function hydratePendingApproval(row) {
    if (!row || typeof row !== "object") {
      throw new Error("Malformed persisted pending approval row.");
    }

    const approval = parseJsonObject(row.approvalJson, "pending_approvals.approval_json");
    assertNonEmptyString(approval.requestId, "pending_approvals.approval_json.requestId");
    assertInteger(row.approvalRowId, "pending_approvals.approval_row_id", 1);
    return approval;
  }

  function readEventsForSession(sessionId) {
    const rows = selectEventsBySessionIdStatement.all(sessionId);
    return rows.map((row) => {
      const hydratedEvent = hydrateEvent(row);
      return {
        eventId: hydratedEvent.eventId,
        cursor: hydratedEvent.cursor,
        sessionId: hydratedEvent.sessionId,
        occurredAt: hydratedEvent.occurredAt,
        type: hydratedEvent.type,
        turnId: hydratedEvent.turnId,
        itemId: hydratedEvent.itemId,
        payload: hydratedEvent.payload,
      };
    });
  }

  function readPendingApprovalsForSession(sessionId) {
    const rows = selectPendingApprovalsBySessionIdStatement.all(sessionId);
    return rows.map(hydratePendingApproval);
  }

  function loadSessionById(sessionId) {
    const row = selectSessionByIdStatement.get(sessionId);
    if (!row) {
      return null;
    }

    const base = hydrateSessionBase(row);
    return {
      ...base,
      events: readEventsForSession(base.sessionId),
      pendingApprovals: readPendingApprovalsForSession(base.sessionId),
      touchedRecords: [],
    };
  }

  function loadSessionByThreadId(threadId) {
    const row = selectSessionByThreadIdStatement.get(threadId);
    if (!row) {
      return null;
    }

    const base = hydrateSessionBase(row);
    return {
      ...base,
      events: readEventsForSession(base.sessionId),
      pendingApprovals: readPendingApprovalsForSession(base.sessionId),
      touchedRecords: [],
    };
  }

  function syncSessionReference(target, source) {
    if (!target || typeof target !== "object") {
      return source;
    }

    for (const key of Object.keys(target)) {
      delete target[key];
    }
    Object.assign(target, source);
    return target;
  }

  const appendEventTransaction = database.transaction((eventRecord, sessionId, updatedAt) => {
    insertEventStatement.run({
      eventId: eventRecord.eventId,
      sessionId,
      cursor: eventRecord.cursor,
      cursorIndex: eventRecord.cursorIndex,
      occurredAt: eventRecord.occurredAt,
      type: eventRecord.type,
      turnId: eventRecord.turnId,
      itemId: eventRecord.itemId,
      payloadJson: stringifyJsonObject(eventRecord.payload, "session_events.payload_json"),
    });
    updateSessionCursorStatement.run({
      sessionId,
      cursor: eventRecord.cursor,
      nextCursorIndex: eventRecord.cursorIndex,
      updatedAt,
    });
  });

  const addPendingApprovalTransaction = database.transaction((sessionId, requestId, approval, createdAt, updatedAt) => {
    insertPendingApprovalStatement.run({
      sessionId,
      requestId,
      approvalJson: stringifyJsonObject(approval, "pending_approvals.approval_json"),
      createdAt,
    });
    touchSessionStatement.run(updatedAt, sessionId);
  });

  const recoverStartupStateTransaction = database.transaction((recoverableSessionCount, pendingApprovalCount, recoveryTimestamp) => {
    if (recoverableSessionCount > 0) {
      interruptRecoverableSessionsStatement.run(recoveryTimestamp);
    }
    if (pendingApprovalCount > 0) {
      clearPendingApprovalsStatement.run();
    }
  });

  const recoverableSessionCount = readCount(
    selectRecoverableSessionCountStatement.get()?.count ?? 0,
    "recoverable sessions count",
  );
  const pendingApprovalCount = readCount(selectPendingApprovalsCountStatement.get()?.count ?? 0, "pending approvals count");
  if (recoverableSessionCount > 0 || pendingApprovalCount > 0) {
    const recoveryTimestamp = recoverableSessionCount > 0 ? nextTimestamp() : null;
    recoverStartupStateTransaction(recoverableSessionCount, pendingApprovalCount, recoveryTimestamp);
  }

  function getSessionById(sessionId) {
    assertNonEmptyString(sessionId, "sessionId");
    return loadSessionById(sessionId);
  }

  function getSessionByThreadId(threadId) {
    assertNonEmptyString(threadId, "threadId");
    return loadSessionByThreadId(threadId);
  }

  function createSession({ title, operator, threadId, cwd, configPath }) {
    const normalizedThreadId = assertNonEmptyString(threadId, "threadId");
    if (getSessionByThreadId(normalizedThreadId)) {
      throw new Error(`threadId ${normalizedThreadId} is already mapped to a session.`);
    }

    const createdAt = nextTimestamp();
    const sessionId = randomUUID();

    const normalizedSession = normalizeSessionForStorage({
      sessionId,
      threadId: normalizedThreadId,
      title: assertNullableString(title, "title"),
      operator: cloneSessionOperator(operator),
      cwd: assertNonEmptyString(cwd, "cwd"),
      configPath: assertNullableNonEmptyString(configPath, "configPath"),
      status: "idle",
      createdAt,
      updatedAt: createdAt,
      lastMessagePreview: null,
      cursor: null,
      nextCursorIndex: 0,
    });

    try {
      insertSessionStatement.run({
        ...normalizedSession,
        operatorJson: stringifyJsonObject(normalizedSession.operator, "sessions.operator_json", { allowNull: true }),
      });
    } catch (error) {
      if (error?.code === "SQLITE_CONSTRAINT_UNIQUE") {
        throw new Error(`threadId ${normalizedThreadId} is already mapped to a session.`);
      }
      throw error;
    }

    const createdSession = loadSessionById(sessionId);
    if (!createdSession) {
      throw new Error(`Failed to read session ${sessionId} immediately after insert.`);
    }
    return createdSession;
  }

  function updateSession(session, updates) {
    if (!isPlainObject(session)) {
      throw new Error("updateSession requires a session object.");
    }
    if (!isPlainObject(updates)) {
      throw new Error("updateSession updates must be an object.");
    }

    const existingSession = loadSessionById(assertNonEmptyString(session.sessionId, "session.sessionId"));
    if (!existingSession) {
      throw new Error(`Cannot update unknown sessionId ${session.sessionId}.`);
    }

    // Merge anchor: this allowlist is the fail-loud contract for mutable session
    // fields across runtime handlers and persisted storage.
    const supportedUpdateFields = new Set([
      "title",
      "operator",
      "cwd",
      "configPath",
      "status",
      "lastMessagePreview",
      "cursor",
      "nextCursorIndex",
    ]);
    for (const fieldName of Object.keys(updates)) {
      if (!supportedUpdateFields.has(fieldName)) {
        throw new Error(`Unsupported session update field: ${fieldName}`);
      }
    }

    const mergedSession = normalizeSessionForStorage({
      ...existingSession,
      ...updates,
      operator: Object.hasOwn(updates, "operator") ? cloneSessionOperator(updates.operator) : existingSession.operator,
      updatedAt: nextTimestamp(),
    });

    updateSessionStatement.run({
      ...mergedSession,
      operatorJson: stringifyJsonObject(mergedSession.operator, "sessions.operator_json", { allowNull: true }),
    });

    const refreshedSession = loadSessionById(mergedSession.sessionId);
    if (!refreshedSession) {
      throw new Error(`Failed to reload session ${mergedSession.sessionId} after update.`);
    }
    return syncSessionReference(session, refreshedSession);
  }

  function appendEvent(session, entry) {
    if (!isPlainObject(session)) {
      throw new Error("appendEvent requires a session object.");
    }

    const existingSession = loadSessionById(assertNonEmptyString(session.sessionId, "session.sessionId"));
    if (!existingSession) {
      throw new Error(`Cannot append event to unknown sessionId ${session.sessionId}.`);
    }

    const normalizedEntry = normalizeEventInput(entry, nextTimestamp, observeTimestamp);
    const nextCursorIndex = existingSession.nextCursorIndex + 1;
    assertInteger(nextCursorIndex, "next cursor index", 1);

    const event = {
      eventId: randomUUID(),
      cursor: String(nextCursorIndex),
      cursorIndex: nextCursorIndex,
      sessionId: existingSession.sessionId,
      occurredAt: normalizedEntry.occurredAt,
      type: normalizedEntry.type,
      turnId: normalizedEntry.turnId,
      itemId: normalizedEntry.itemId,
      payload: normalizedEntry.payload,
    };

    const updatedAt = nextTimestamp();
    appendEventTransaction(event, existingSession.sessionId, updatedAt);

    const refreshedSession = loadSessionById(existingSession.sessionId);
    if (!refreshedSession) {
      throw new Error(`Failed to reload session ${existingSession.sessionId} after appending event.`);
    }
    syncSessionReference(session, refreshedSession);

    return {
      eventId: event.eventId,
      cursor: event.cursor,
      sessionId: event.sessionId,
      occurredAt: event.occurredAt,
      type: event.type,
      turnId: event.turnId,
      itemId: event.itemId,
      payload: event.payload,
    };
  }

  function appendThreadEvent(threadId, entry) {
    const session = getSessionByThreadId(threadId);
    if (!session) {
      throw createAssociationError(`No session mapping exists for threadId ${threadId}.`, {
        threadId,
        eventType: entry?.type ?? null,
      });
    }
    return appendEvent(session, entry);
  }

  function addPendingApproval(threadId, approval) {
    const session = getSessionByThreadId(threadId);
    if (!session) {
      throw createAssociationError(`No session mapping exists for approval threadId ${threadId}.`, {
        threadId,
        requestId: approval?.requestId ?? null,
      });
    }

    const normalizedApproval = normalizeApprovalInput(approval, nextTimestamp, observeTimestamp);
    const updatedAt = nextTimestamp();
    addPendingApprovalTransaction(
      session.sessionId,
      normalizedApproval.requestId,
      normalizedApproval.approval,
      normalizedApproval.createdAt,
      updatedAt,
    );

    const refreshedSession = loadSessionById(session.sessionId);
    if (!refreshedSession) {
      throw new Error(`Failed to reload session ${session.sessionId} after adding pending approval.`);
    }
    syncSessionReference(session, refreshedSession);

    return normalizedApproval.approval;
  }

  function resolvePendingApproval(threadId, requestId) {
    const session = getSessionByThreadId(threadId);
    if (!session) {
      throw createAssociationError(
        `No session mapping exists while resolving approval for threadId ${threadId}.`,
        {
          threadId,
          requestId,
        },
      );
    }

    if (typeof requestId !== "string") {
      throw new Error("resolvePendingApproval requestId must be a string.");
    }

    const deleted = deletePendingApprovalsByRequestIdStatement.run(session.sessionId, requestId);
    if (deleted.changes > 0) {
      touchSessionStatement.run(nextTimestamp(), session.sessionId);
    }

    const refreshedSession = loadSessionById(session.sessionId);
    if (!refreshedSession) {
      throw new Error(`Failed to reload session ${session.sessionId} after resolving pending approval.`);
    }
    return syncSessionReference(session, refreshedSession);
  }

  function listSessions({ offset, limit }) {
    const normalizedOffset = assertInteger(offset, "listSessions offset", 0);
    const totalSessionCount = readCount(selectSessionCountStatement.get()?.count ?? 0, "sessions count");
    const effectiveLimit = limit === undefined || limit === null
      ? totalSessionCount
      : assertInteger(limit, "listSessions limit", 0);

    const rows = listSessionsPageStatement.all(effectiveLimit, normalizedOffset);
    const sessions = rows.map((row) => {
      const baseSession = hydrateSessionBase(row);
      const pendingApprovalCount = readCount(
        selectPendingApprovalsCountBySessionIdStatement.get(baseSession.sessionId)?.count ?? 0,
        `pending approvals count for session ${baseSession.sessionId}`,
      );
      return {
        sessionId: baseSession.sessionId,
        title: baseSession.title,
        operator: cloneSessionOperator(baseSession.operator),
        cwd: baseSession.cwd,
        configPath: baseSession.configPath,
        status: baseSession.status,
        createdAt: baseSession.createdAt,
        updatedAt: baseSession.updatedAt,
        hasPendingApprovals: pendingApprovalCount > 0,
        lastMessagePreview: baseSession.lastMessagePreview,
      };
    });

    const nextOffset = normalizedOffset + sessions.length;
    return {
      sessions,
      nextCursor: nextOffset < totalSessionCount ? String(nextOffset) : null,
    };
  }

  function readSessionEvents(session, { afterCursor, limit }) {
    if (!isPlainObject(session)) {
      throw new Error("readSessionEvents requires a session object.");
    }

    const persistedSession = loadSessionById(assertNonEmptyString(session.sessionId, "session.sessionId"));
    if (!persistedSession) {
      throw new Error(`Cannot read events for unknown sessionId ${session.sessionId}.`);
    }
    syncSessionReference(session, persistedSession);

    const normalizedAfterCursor = afterCursor === undefined ? null : afterCursor;
    if (normalizedAfterCursor !== null && typeof normalizedAfterCursor !== "string") {
      throw new Error("readSessionEvents afterCursor must be a string or null.");
    }

    const effectiveLimit = limit === undefined || limit === null
      ? persistedSession.events.length
      : assertInteger(limit, "readSessionEvents limit", 0);

    let startIndex = 0;
    if (normalizedAfterCursor !== null) {
      const cursorIndex = persistedSession.events.findIndex((event) => event.cursor === normalizedAfterCursor);
      if (cursorIndex === -1) {
        return {
          cursorFound: false,
          events: [],
          nextCursor: persistedSession.cursor,
          hasMore: false,
        };
      }
      startIndex = cursorIndex + 1;
    }

    const events = persistedSession.events.slice(startIndex, startIndex + effectiveLimit);
    const nextCursor = events.length > 0 ? events[events.length - 1].cursor : normalizedAfterCursor;

    return {
      cursorFound: true,
      events: [...events],
      nextCursor,
      hasMore: startIndex + events.length < persistedSession.events.length,
    };
  }

  return {
    createSession,
    getSessionById,
    getSessionByThreadId,
    listSessions,
    readSessionEvents,
    updateSession,
    appendEvent,
    appendThreadEvent,
    addPendingApproval,
    resolvePendingApproval,
    toSessionSummary,
    toSnapshot,
  };
}
