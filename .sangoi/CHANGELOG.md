2025-12-20
- Feature: Added `manage_context` tool for smart context management (list/status + include/exclude/delete + replace for tool outputs/reasoning + pinned notes).
- Feature: Extended `manage_context` with a non-interactive v2 `mode=retrieve|apply` contract (snapshot_id anti-drift, atomic batched ops, real dry-run simulation).
- Core: When `manage_context` is available, Codex appends built-in tool usage instructions to the system prompt (same pattern as `apply_patch`).
- UX: `manage_context status` / `retrieve` now include a bounded “what’s taking space” breakdown (approx bytes by category + top included items) to guide pruning decisions.
- Core: Persisted context overlays (replacements + notes) in rollout via `RolloutItem::ContextOverlay` and replayed them on resume.
- Fix: Pruning tool calls now also prunes their corresponding outputs; prompt construction additionally normalizes call/output pairs to avoid orphan-output session errors.
- Docs: Added `.sangoi/reference/manage_context.md` and a task log for the implementation.

2025-10-23
- Docs: Added an English version of the prune implementation map (`.sangoi/prune-implementation-map.en.md`) so non-Portuguese contributors can align on rollout behavior.

2025-10-18
- Fix: Prevent UTF-8 boundary panic when opening `/prune` (session preview builder now truncates safely). Affects non-ASCII content.
 - Fix: Uniform, grapheme-safe caps for all prune previews to avoid broken glyphs and layout jitter.
 - Fix: Deduplicate indices during destructive prune to prevent shifted deletions.
 - UX: After applying advanced prune, UI now refreshes context and reopens the advanced list; local toggles are cleared to avoid desync.
- UX: Show `[!]` warning on risky deletions (when next included item is an assistant message).
 - UX: After confirming advanced prune, return focus to the chat composer (no auto-reopen of Advanced).

2025-10-20
- Fix: Context prune persistence now uses stable history RIDs; resume respects deletions and inclusion toggles instead of resurrecting items.
- Core: Rollout snapshots emit `included_ids`/`deleted_ids` alongside indices with graceful fallback for legacy files.
- Tests: Added replay coverage for prune-by-ID and index-only rollouts to lock behavior.
- Fix: Manual prune (`/prune` categorias) também grava snapshots `ContextInclusion`, então os toggles não evaporam ao retomar a conversa.
- UX: `Esc` em qualquer submenu de `/prune` fecha o fluxo; o menu manual busca snapshot e atualiza as porcentagens sem depender do advanced.
