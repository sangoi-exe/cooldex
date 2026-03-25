# Bridge Runtime Notes

Last Review: 2026-03-25

## Purpose

- Own the durable bridge session model, transcript persistence, and the runtime-side HTTP contract consumed by operator surfaces.

## Key files

<!-- Merge-safety anchor: transcript-format helpers, runtime wiring, docs, and tests must stay aligned because the stderr transcript is an operator-facing local contract. -->

- `src/bridge/runtime.js` - runtime handlers, notification persistence, and request/response seams such as `session_create`, `session_rename`, `session_poll`, and `message_send`.
- `src/bridge/store.js` - persisted session/event store, cursor allocation, and append semantics.
- `src/bridge/transcript.js` - transcript prefix formatter for terminal-only debug output, including timestamp and operator user-id tags.
- `src/bridge/transcript.test.js` - focused transcript-prefix contract coverage for the formatter helper.
- `README.md` - operator-facing bridge contract notes and transcript/persistence behavior that must stay aligned with `runtime.js`.

## Notes

- Browser-visible event contracts must fail loud on malformed payloads.
- `message_send` acceptance ids/cursor semantics are part of the operator reload/poll contract; do not change them casually.
- Session titles are bridge-owned durable state. Keep `session_rename` aligned with `store.updateSession(...)`, the HTTP route in `src/app.js`, and every operator consumer.
- Keep transcript prefix changes aligned across `runtime.js`, `transcript.js`, `transcript.test.js`, and `README.md`; the stderr format is local-only but still operator-facing.
- If you add a new durable event type, keep `runtime.js`, `store.js`, and any operator-consumer docs aligned in the same change.
