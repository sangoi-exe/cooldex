import { randomUUID } from "node:crypto";

function createAssociationError(message, details) {
  const error = new Error(message);
  error.code = "SESSION_ASSOCIATION_FAILED";
  error.details = details;
  return error;
}

function toSessionSummary(session) {
  return {
    sessionId: session.sessionId,
    title: session.title,
    cwd: session.cwd,
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
    touchedRecords: [...session.touchedRecords],
  };
}

export function createBridgeStore() {
  const sessionsById = new Map();
  const sessionIdsByThreadId = new Map();
  let lastTimestamp = 0;

  function nextTimestamp() {
    const current = Date.now();
    lastTimestamp = current > lastTimestamp ? current : lastTimestamp + 1;
    return lastTimestamp;
  }

  function getSessionById(sessionId) {
    return sessionsById.get(sessionId) ?? null;
  }

  function getSessionByThreadId(threadId) {
    const sessionId = sessionIdsByThreadId.get(threadId);
    if (!sessionId) {
      return null;
    }
    return sessionsById.get(sessionId) ?? null;
  }

  function createSession({ title, threadId, cwd }) {
    if (sessionIdsByThreadId.has(threadId)) {
      throw new Error(`threadId ${threadId} is already mapped to a session.`);
    }

    const createdAt = nextTimestamp();
    const sessionId = randomUUID();
    const session = {
      sessionId,
      threadId,
      title,
      cwd,
      status: "idle",
      createdAt,
      updatedAt: createdAt,
      lastMessagePreview: null,
      cursor: null,
      nextCursorIndex: 0,
      events: [],
      pendingApprovals: [],
      touchedRecords: [],
    };

    sessionsById.set(sessionId, session);
    sessionIdsByThreadId.set(threadId, sessionId);

    return session;
  }

  function updateSession(session, updates) {
    Object.assign(session, updates, {
      updatedAt: nextTimestamp(),
    });
    return session;
  }

  function appendEvent(session, entry) {
    session.nextCursorIndex += 1;
    const cursor = String(session.nextCursorIndex);
    const event = {
      eventId: randomUUID(),
      cursor,
      sessionId: session.sessionId,
      occurredAt: entry.occurredAt ?? nextTimestamp(),
      type: entry.type,
      turnId: entry.turnId ?? null,
      itemId: entry.itemId ?? null,
      payload: entry.payload ?? {},
    };

    session.cursor = cursor;
    session.events.push(event);
    session.updatedAt = nextTimestamp();
    return event;
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

    session.pendingApprovals.push(approval);
    session.updatedAt = nextTimestamp();
    return approval;
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

    const before = session.pendingApprovals.length;
    session.pendingApprovals = session.pendingApprovals.filter((approval) => approval.requestId !== requestId);
    if (session.pendingApprovals.length !== before) {
      session.updatedAt = nextTimestamp();
    }
    return session;
  }

  function listSessions({ offset, limit }) {
    const ordered = Array.from(sessionsById.values()).sort((left, right) => {
      if (left.updatedAt !== right.updatedAt) {
        return right.updatedAt - left.updatedAt;
      }
      if (left.createdAt !== right.createdAt) {
        return right.createdAt - left.createdAt;
      }
      return left.sessionId.localeCompare(right.sessionId);
    });

    const effectiveLimit = limit ?? ordered.length;
    const page = ordered.slice(offset, offset + effectiveLimit).map(toSessionSummary);
    const nextOffset = offset + page.length;
    return {
      sessions: page,
      nextCursor: nextOffset < ordered.length ? String(nextOffset) : null,
    };
  }

  function readSessionEvents(session, { afterCursor, limit }) {
    let startIndex = 0;
    if (afterCursor !== null) {
      const cursorIndex = session.events.findIndex((event) => event.cursor === afterCursor);
      if (cursorIndex === -1) {
        return {
          cursorFound: false,
          events: [],
          nextCursor: session.cursor,
          hasMore: false,
        };
      }
      startIndex = cursorIndex + 1;
    }

    const effectiveLimit = limit ?? session.events.length;
    const events = session.events.slice(startIndex, startIndex + effectiveLimit);
    const nextCursor = events.length > 0 ? events[events.length - 1].cursor : afterCursor;
    return {
      cursorFound: true,
      events: [...events],
      nextCursor,
      hasMore: startIndex + events.length < session.events.length,
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
