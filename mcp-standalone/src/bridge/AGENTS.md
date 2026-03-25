# Bridge Runtime Notes

Last Review: 2026-03-24

## Purpose

- Own the durable bridge session model, transcript persistence, and the runtime-side HTTP contract consumed by operator surfaces.

## Key files

- `src/bridge/runtime.js` - runtime handlers, notification persistence, and request/response seams such as `session_create`, `session_rename`, `session_poll`, and `message_send`.
- `src/bridge/store.js` - persisted session/event store, cursor allocation, and append semantics.
- `README.md` - operator-facing bridge contract notes and transcript/persistence behavior that must stay aligned with `runtime.js`.

## Notes

- Browser-visible event contracts must fail loud on malformed payloads.
- `message_send` acceptance ids/cursor semantics are part of the operator reload/poll contract; do not change them casually.
- Session titles are bridge-owned durable state. Keep `session_rename` aligned with `store.updateSession(...)`, the HTTP route in `src/app.js`, and every operator consumer.
- If you add a new durable event type, keep `runtime.js`, `store.js`, and any operator-consumer docs aligned in the same change.
