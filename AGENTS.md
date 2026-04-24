# Workspace Rules

## Custom Sync/Merge Rules (Mandatory)

During any sync/merge with `main` and/or `upstream`, these rules are mandatory:

1. These sync/merge instructions are specific to this workspace and must ALWAYS remain in `AGENTS.md` during future synchronizations.
2. Always resolve conflicts **MANUALLY** every time. Using `ours/theirs` automation is forbidden (including `-X ours`, `-X theirs`, `git checkout --ours`, `git checkout --theirs`, and equivalents).
3. In every conflict, find the best way to preserve the custom functionality we added while also reconciling significant upstream improvements.
4. When upstream and workspace-local code contain almost the same logic, compare both carefully and prefer the structurally cleaner upstream shape when it still supports the required local behavior; port only the necessary local contract deltas instead of blindly restoring the older local copy.
5. When a workspace-local customization keeps colliding with high-churn native files, prefer extracting that customization into a new local module and importing it from the native seam when that reduces future merge friction without adding compatibility shims, alias layers, or ownership confusion.
6. If a conflict or upstream delta is exclusively Windows-specific, do not spend merge effort preserving a separate workspace-local preference there just for this checkout; the operator runs Codex under WSL. Keep normal scrutiny on any change that can affect WSL, shared cross-platform code, serialization/contracts, or Linux-visible behavior.
7. `Merge-safety anchor:` markers are MANDATORY, not optional, on every touched workspace-local divergence file and every touched seam whose behavior, docs, tests, schema, serialization, cache, or operator surface must stay aligned with those customizations. Use the file's native comment syntax (`//`, `///`, `#`, `<!-- -->`, etc.); the required marker text is `Merge-safety anchor:`, not literal `//` everywhere. If a file cannot carry inline comments, add the nearest durable technical note that names the invariant being preserved. Missing merge-safety markers in touched customized or customization-adjacent seams are STOP-SHIP.
8. Existing `Merge anchor:` comments are legacy debt. Whenever you touch one of those files for customization-preserving work, normalize it to `Merge-safety anchor:` in the same change.
9. Remove from the workspace all CI/CD content under `.github` (workflows, actions, and any other pipeline artifacts).

<!-- Merge-safety anchor: AGENTS.md is the canonical source for the workspace-local customization inventory and merge-policy invariants; future sync work must update this section and keep scratchpads as redirects only. -->

## Codex CLI Customization Boundary

- Workspace-local customizations in this checkout exist only to support the shipped Codex CLI and its operator-facing surfaces, including the TUI and the runtime/tooling/prompt seams they require.
- Do not add, preserve, or reintroduce workspace-local divergence in files that do not materially affect Codex CLI behavior.
- If an upstream delta touches non-CLI surfaces, prefer leaving those files identical to upstream instead of carrying local edits there.
- If a non-CLI file must change only because it is a generated follower or contract artifact of a CLI-owned owner, keep it as a follower of that CLI-owned owner and do not let it become a second home for local behavior.

## Native Windows Execution Scope

- The operator runs Codex CLI under WSL, not as a native Windows binary.
- Unless the user explicitly asks for native Windows work, do not start implementation, review, or validation work that is isolated to native-Windows-only Codex CLI behavior.
- If a finding is confined to native-Windows-only seams and does not affect WSL-visible behavior, shared cross-platform owners, serialization/contracts, or Linux-visible behavior, record it and defer it instead of widening the current batch.
- If a shared owner change necessarily touches a Windows follower to keep the shipped CLI honest, keep that follower coherent, but do not turn that into a dedicated native-Windows remediation lane.

## Legacy / Feature-Gate Surface Discipline

- Do not rename an active shipped surface just because a gated or newer surface uses a different name. Public tool/function/command renames require explicit user intent or a separately locked migration plan; additive functionality belongs under the existing active name until that migration is actually adopted.
- Do not backport schema fields, target semantics, output shapes, or wording from a feature-gated / unreleased surface into the active legacy/default surface unless the migration itself is in scope and the feature is being intentionally enabled.
- When upstream renames a concept (for example `subagent` -> `multiagent`), do not keep both names alive as parallel canon just because local features were built on the older term. Pick one owner per active surface, migrate deliberately when needed, and avoid alias-soup in runtime, docs, prompts, configs, and tests.
- When legacy and gated-V2 surfaces coexist, lock canon per surface before editing: verify the active surface in `codex-rs/features/src/lib.rs`, `codex-rs/core/src/tools/spec.rs`, `codex-rs/core/src/tools/spec_tests.rs`, and the owning handler files, then keep runtime, tests, docs, prompts, and home overlays aligned to that chosen owner.
- If a workspace-local customization intentionally diverges from upstream legacy, document it here as local divergence instead of silently hybridizing legacy with V2 semantics.

## Workspace-local Customization Inventory (Source of Truth)

This section is the canonical cluster-level inventory of durable workspace-local divergence that must survive future syncs/merges with `upstream/main`. Re-derive it from the live diff against `upstream/main` whenever a cluster is added, removed, or materially re-scoped. Scratchpads may point here, but they must not duplicate the inventory body. This is a policy/inventory source of truth, not a claim that every legacy marker has already been normalized everywhere. The listed files are representative high-signal entrypoints, not an exhaustive manifest of every touched file in a cluster.
For commentless customization-adjacent seams such as JSON schema artifacts, the matching inventory bullet here is the nearest durable technical note required by the merge-safety policy.

- `manage_context`: strict retrieve/apply flow, `/sanitize` integration, replacement-history materialization/rollback, and the canonical home-doc stack. Representative files: `codex-rs/core/src/tools/handlers/manage_context.rs`, `codex-rs/core/src/tasks/sanitize.rs`, `codex-rs/core/sanitize_prompt.md`, `~/.codex/manage_context.md`, `~/.codex/manage_context_cheatsheet.md`.
- `recall`: args-less recall, compact/debug output behavior, rollout/compaction coupling, fail-loud boundary handling, and the recall docs/config drift trap. Representative files: `codex-rs/core/src/tools/handlers/recall.rs`, `codex-rs/core/src/tools/spec.rs`, `codex-rs/core/src/session/mod.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/prompt_gc_rollout.rs`, `codex-rs/core/src/session/rollout_reconstruction.rs`, `docs/recall.md`, `docs/guide_reapply_recall.md`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/config.schema.json`, `codex-rs/core/tests/suite/compact.rs`, `codex-rs/core/tests/suite/compact_remote.rs`.
- `prompt_gc` / `PromptGcSidecar`: automatic prompt GC for phase-1 regular lead turns via a hidden summary-only child-session flow; activation keeps the `FunctionCallOutput` `Token qty > 200` fast path for intermediate checkpoints plus a bounded `FinalAnswer` selectable-burden fallback derived from the retrieve budget, requires metadata-backed prompt-gc rollout markers (legacy message-only markers are incompatible and disable future prompt-gc for that session), pairs tool outputs fail-loud on ambiguous `call_id` ownership, omits opaque encrypted-only reasoning from the hidden chunk manifest, and only refreshes TUI-private context-usage state after a real apply succeeds. Representative files: `codex-rs/core/src/prompt_gc_sidecar.rs`, `codex-rs/core/src/tools/handlers/prompt_gc.rs`, `codex-rs/core/prompt_gc_prompt.md`, `codex-rs/core/src/client_common.rs`, `codex-rs/core/src/client.rs`, `codex-rs/core/src/session/mod.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/prompt_gc_rollout.rs`, `codex-rs/rollout/src/recorder.rs`, `codex-rs/core/src/session/rollout_reconstruction.rs`, `codex-rs/core/src/tasks/mod.rs`, `codex-rs/core/src/tasks/regular.rs`, `codex-rs/tui/src/bottom_pane/mod.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/app.rs`.
- `post-compact prompt-top context restoration`: after local or remote compaction, whether manual `/compact` or pre-turn auto-compact, the next regular turn must rebuild canonical prompt-top context like a fresh session so `developer_instructions` and the root/path `AGENTS.md`-derived contextual user block stay ahead of compacted history; mid-turn compaction instead reinjects that same canonical initial context immediately before the last real user message or summary so the summary/compaction item stays last. Representative files: `codex-rs/core/src/compact.rs`, `codex-rs/core/src/compact_remote.rs`, `codex-rs/core/src/session/mod.rs`, `codex-rs/core/src/agents_md.rs`, `codex-rs/core/tests/suite/compact.rs`, `codex-rs/core/tests/suite/compact_remote.rs`.
- `resume transcript rendering`: stored-session resume now prefers rollout-backed reconstructed turn replay over the lossy `SessionConfigured.initial_messages` projection, with `[tui].resume_history` truncating at the last surviving visible `Context compacted` marker by default. In the unified TUI, once reconstructed turns are available they define the resume boundary even if the surviving suffix is currently non-renderable; fall back to legacy `initial_messages` only when reconstructed turns could not be loaded at all. Keep visible replay parity for completed unified-exec/review-finish/collab-wait/file-change surfaces, keep hook-prompt-only reconstructed history non-renderable until hook prompts get a real visible replay surface, and keep begin-only web/image tool history non-renderable so resume never fabricates completed-looking rows from incomplete evidence. Preserve rollback-aware truncation in shared turn reconstruction and keep the TUI replay/session adapters aligned with that contract. Representative files: `codex-rs/app-server-protocol/src/protocol/thread_history.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/config.schema.json`, `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/app_server_session.rs`, `codex-rs/tui/src/app/app_server_adapter.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/chatwidget/tests.rs`, `codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__resumed_turn_history_replays_original_rollout.snap`, `docs/config.md`.
- `final turn handoff raw debug dump`: `[tui].final_turn_handoff_debug` is a workspace-local operator debug surface that must stay owned by the core turn-finish path, not a TUI-only render side path. Keep the raw `last_agent_message` byte-preserving dump contract aligned across config/schema/docs/tests/runtime: when enabled and non-empty, write `${codex_home}/debug/<session_uuid>/turn-<turn_id>-final-handoff-raw.txt` from `Session::on_task_finished(...)`; on create/write failure, emit a warning for that turn and still complete it. Representative files: `codex-rs/config/src/types.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/config.schema.json`, `codex-rs/core/src/tasks/mod.rs`, `codex-rs/core/src/session/tests.rs`, `docs/config.md`.
- `/accounts`: multi-account ChatGPT management is a cross-file divergence cluster, not a single TUI command. Treat auth storage, TUI popup/cache flow, slash-command gating, remote app-server roster/set-active/lease-management surfaces, autoswitch refresh, and auth docs as one subsystem. Representative files: `codex-rs/login/src/auth/manager.rs`, `codex-rs/login/src/auth/storage.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/app-server-protocol/src/protocol/common.rs`, `codex-rs/app-server-protocol/src/protocol/v2.rs`, `codex-rs/app-server/src/codex_message_processor.rs`, `codex-rs/tui/src/app_server_session.rs`, `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/app_event.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/app-server/README.md`, `docs/authentication.md`, `docs/slash_commands.md`.
- `WS12 account-state coordination`: saved-account usage truth is moving under a dedicated SQLite owner so autoswitch, `/accounts`, and rate-limit reconciliation stop treating the legacy auth-store cache as live runtime truth. Keep the SQLite owner, auth-manager hydration/strip flow, and config-based `sqlite_home` wiring aligned during the staged cutover. Representative files: `codex-rs/account-state/src/lib.rs`, `codex-rs/login/src/auth/manager.rs`, `codex-rs/login/src/auth/storage.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/Cargo.toml`.
- sub-agent/runtime orchestration: custom spawn/profile plumbing, background-agent handling, parallel tool execution, collaboration/thread APIs, and child-agent prompt layering live outside upstream. Preserve `subagent_instructions_file` as the child base-instructions source, keep child spawn/resume config inheriting workspace AGENTS/project-doc context plus `Feature::ChildAgentsMd`, preserve AGENTS-derived `user_instructions` in forked child history, and allow child `developer_instructions` from lead/role config to stay active. Keep the active legacy collab surface contract separate from `MultiAgentV2`: do not rename shipped legacy tools or backport V2-only task-name/path semantics into legacy unless a deliberate V2 rollout is in scope. Keep the active built-in collab prompts aligned with that same legacy surface so embedded guidance does not keep telling the lead to call stale tool names. Representative files: `codex-rs/core/src/tools/handlers/multi_agents.rs`, `codex-rs/core/src/tools/handlers/multi_agents_v2.rs`, `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`, `codex-rs/core/src/tools/parallel.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/config/profile.rs`, `codex-rs/core/config.schema.json`, `codex-rs/core/src/agents_md.rs`, `codex-rs/core/src/state/session.rs`, `codex-rs/core/src/agent/control.rs`, `codex-rs/core/src/agent/builtins/awaiter.toml`, `codex-rs/core/templates/collab/experimental_prompt.md`, `codex-rs/core/src/rid.rs`, `codex-rs/app-server-protocol/src/protocol/v2.rs`, `codex-rs/app-server-protocol/schema/**`, `codex-rs/app-server/src/codex_message_processor.rs`, `codex-rs/state/src/runtime/threads.rs`, `AGENTS.md`.
- TUI debugging/custom operator surfaces: `/debug`, raw-response inspection, and context-window/operator-facing debugging affordances are local customizations and must stay aligned with runtime event/cache semantics. Representative files: `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/core/src/prompt_debug.rs`, `codex-rs/tui/src/app.rs`.
- `mcp-standalone` bridge/runtime customization: session cwd/config-path resolution, operator metadata plumbing, bridge defaults, and local runtime/auth expectations are workspace-local behavior. Representative files: `mcp-standalone/src/app.js`, `mcp-standalone/src/bridge/runtime.js`, `mcp-standalone/src/app-server/client.js`, `mcp-standalone/src/config.js`, `mcp-standalone/src/logger.js`, `mcp-standalone/README.md`.
<!-- Merge-safety anchor: local workspace keeps legacy Landlock enabled from /home/lucas/.codex/config.toml; preserve the propagation and runtime seams listed in the inventory entry below until bubblewrap has a safe writable-gitdir alternative. -->
- `legacy Landlock override`: the local operator config keeps `features.use_legacy_landlock = true` in `/home/lucas/.codex/config.toml` so sandboxed Git metadata writes remain available until upstream ships a safe writable-`gitdir` alternative. Preserve the propagation/honor seams across app-server, core sandbox plumbing, and Linux helper/runtime. Representative files: `/home/lucas/.codex/config.toml`, `codex-rs/app-server/src/codex_message_processor.rs`, `codex-rs/core/src/exec.rs`, `codex-rs/core/src/sandboxing/mod.rs`, `codex-rs/core/src/landlock.rs`, `codex-rs/linux-sandbox/src/linux_run_main.rs`, `codex-rs/cli/src/debug_sandbox.rs`.
- workspace sync policy and local instruction overlays: keep `.github/**` removed from this workspace, preserve the AGENTS-centric sync policy, and keep local config/instruction overlays aligned when those rules change. Representative files: `AGENTS.md`, `/home/lucas/.codex/config.toml`, diff against `upstream/main` for `.github/**`.

## `.sangoi` Repo Boundary

- The workspace `.sangoi/` checkout is its own Git repository and is intentionally ignored by the main workspace repo (`/.sangoi/` in `.gitignore`), so root `git status`/`git diff` do not capture `.sangoi` changes.
- When a task changes `.sangoi/**`, review and commit those changes from the `.sangoi` repo itself.
- Apply the same commit-discipline rule inside `.sangoi`: when a clean split is possible, keep code/config/script changes separate from docs/instructions/logs/reports changes instead of mixing them into one commit.

## Commit Attribution

<!-- Merge-safety anchor: main-repo commits in this workspace should preserve Codex co-author attribution when Codex materially participates in the change. -->

- For commits to the main repository `https://github.com/sangoi-exe/cooldex/`, include `Co-authored-by: Codex <codex@openai.com>` in the commit message unless the user explicitly asks not to.

## Workspace Test Safety

- Do not delegate Cargo validation to sub-agents in this workspace. That includes `cargo check`, `cargo test --no-run`, and `cargo test`.
- Run every Cargo validation rung through `./scripts/cargo-guard.sh` from the workspace root; do not run raw `cargo check`, `cargo test --no-run`, or `cargo test` directly in this workspace.
- Cargo validation precedence is strict: exhaust the lighter/faster checks first, escalate only when they are green, and do not skip ahead to a heavier step when a lighter one can still answer the same question.
- Batch clearly same-class mechanical fallout before escalating to a heavier validation rung; rerun only when the batch is ready or fresh diagnostics are needed.
- `./scripts/cargo-guard.sh` asks Cargo itself for the effective `target_directory`/`build_directory` under the exact wrapper context, caps guarded Cargo parallelism at 4 jobs by default, rejects explicit `-j/--jobs` requests above that cap, enforces the binary 5 GiB free-space floor, and runs `cargo clean` only when the lowest-free-space filesystem across those directories is below that floor before or after a guarded build-like command; failure/interruption alone must not trigger cleanup.
- Lightweight compile-first default: start with the relevant fast checks, preferring `./scripts/cargo-guard.sh cargo check -p <project>` first and escalating to `--tests` only when test targets, fixtures, macros, or integration surfaces are in play.
- Only after the relevant lightweight checks are green, run `./scripts/cargo-guard.sh cargo test -p <project> --no-run` for test-target build/link coverage.
- Only after `./scripts/cargo-guard.sh cargo test -p <project> --no-run` is green, run real `./scripts/cargo-guard.sh cargo test -p <project>` for runtime/behavior validation when behavior actually needs to be proven.
- Successful exit alone is not green: compiler warnings in the selected target set are blockers even when `cargo check` / `cargo test --no-run` exit `0`.
- Target selection must match the shipped surface. If you are validating an installable binary or other top-level deliverable, include that final target (for example `./scripts/cargo-guard.sh cargo check -p codex-cli --bin codex`) instead of validating only a narrower dependency crate. When plain rustc warnings in transitive local crates must also be blocking for that shipped surface, use `just check-strict ...` on the same exact target instead of assuming `just clippy-strict ...` alone proves the full binary path.

# Rust/codex-rs

In the codex-rs folder where the rust code lives:

- Crate names are prefixed with `codex-`. For example, the `core` folder's crate is named `codex-core`
- When using format! and you can inline variables into {}, always do that.
- Install any commands the repo relies on (for example `just`, `rg`, or `cargo-insta`) if they aren't already available before running instructions here.
- Never add or modify any code related to `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` or `CODEX_SANDBOX_ENV_VAR`.
  - You operate in a sandbox where `CODEX_SANDBOX_NETWORK_DISABLED=1` will be set whenever you use the `shell` tool. Any existing code that uses `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` was authored with this fact in mind. It is often used to early exit out of tests that the author knew you would not be able to run given your sandbox limitations.
  - Similarly, when you spawn a process using Seatbelt (`/usr/bin/sandbox-exec`), `CODEX_SANDBOX=seatbelt` will be set on the child process. Integration tests that want to run Seatbelt themselves cannot be run under Seatbelt, so checks for `CODEX_SANDBOX=seatbelt` are also often used to early exit out of tests, as appropriate.
- Always collapse if statements per https://rust-lang.github.io/rust-clippy/master/index.html#collapsible_if
- Always inline format! args when possible per https://rust-lang.github.io/rust-clippy/master/index.html#uninlined_format_args
- Use method references over closures when possible per https://rust-lang.github.io/rust-clippy/master/index.html#redundant_closure_for_method_calls
- Avoid bool or ambiguous `Option` parameters that force callers to write hard-to-read code such as `foo(false)` or `bar(None)`. Prefer enums, named methods, newtypes, or other idiomatic Rust API shapes when they keep the callsite self-documenting.
- When you cannot make that API change and still need a small positional-literal callsite in Rust, follow the `argument_comment_lint` convention:
  - Use an exact `/*param_name*/` comment before opaque literal arguments such as `None`, booleans, and numeric literals when passing them by position.
  - Do not add these comments for string or char literals unless the comment adds real clarity; those literals are intentionally exempt from the lint.
  - The parameter name in the comment must exactly match the callee signature.
  - You can run `just argument-comment-lint` to run the lint check locally. This is powered by Bazel, so running it the first time can be slow if Bazel is not warmed up, though incremental invocations should take <15s. Most of the time, it is best to update the PR and let CI take responsibility for checking this (or run it asynchronously in the background after submitting the PR). Note CI checks all three platforms, which the local run does not.
- When possible, make `match` statements exhaustive and avoid wildcard arms.
- Newly added traits should include doc comments that explain their role and how implementations are expected to use them.
- When writing tests, prefer comparing the equality of entire objects over fields one by one.
- When making a change that adds or changes an API, ensure that the documentation in the `docs/` folder is up to date if applicable.
- Prefer private modules and explicitly exported public crate API.
- If you change `ConfigToml` or nested config types, run `just write-config-schema` to update `codex-rs/core/config.schema.json`.
- When working with MCP tool calls, prefer using `codex-rs/codex-mcp/src/mcp_connection_manager.rs` to handle mutation of tools and tool calls. Aim to minimize the footprint of changes and leverage existing abstractions rather than plumbing code through multiple levels of function calls.
- If you change Rust dependencies (`Cargo.toml` or `Cargo.lock`), run `just bazel-lock-update` from the
  repo root to refresh `MODULE.bazel.lock`, and include that lockfile update in the same change.
- After dependency changes, run `just bazel-lock-check` from the repo root so lockfile drift is caught
  locally before CI.
- Bazel does not automatically make source-tree files available to compile-time Rust file access. If
  you add `include_str!`, `include_bytes!`, `sqlx::migrate!`, or similar build-time file or
  directory reads, update the crate's `BUILD.bazel` (`compile_data`, `build_script_data`, or test
  data) or Bazel may fail even when Cargo passes.
- Do not create small helper methods that are referenced only once.
- Avoid large modules:
  - Prefer adding new modules instead of growing existing ones.
  - Target Rust modules under 500 LoC, excluding tests.
  - If a file exceeds roughly 800 LoC, add new functionality in a new module instead of extending
    the existing file unless there is a strong documented reason not to.
  - This rule applies especially to high-touch files that already attract unrelated changes, such
    as `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/bottom_pane/chat_composer.rs`,
    `codex-rs/tui/src/bottom_pane/footer.rs`, `codex-rs/tui/src/chatwidget.rs`,
    `codex-rs/tui/src/bottom_pane/mod.rs`, and similarly central orchestration modules.
  - When extracting code from a large module, move the related tests and module/type docs toward
    the new implementation so the invariants stay close to the code that owns them.
  - Avoid adding new standalone methods to `codex-rs/tui/src/chatwidget.rs` unless the change is
    trivial; prefer new modules/files and keep `chatwidget.rs` focused on orchestration.
- When running Rust commands (e.g. `just fix` or `cargo test`) be patient with the command and never try to kill them using the PID. Rust lock can make the execution slow, this is expected.

Run `just fmt` (in `codex-rs` directory) automatically after you have finished making Rust code changes; do not ask for approval to run it.

For Rust validation in `codex-rs`, use this light-first ladder:

0. Before each rung below, invoke it via `./scripts/cargo-guard.sh cargo ...`; the wrapper asks Cargo for the effective `target_directory`/`build_directory`, caps guarded Cargo parallelism at 4 jobs unless you explicitly request a lower count, rejects explicit higher `-j/--jobs` requests, checks the 5 GiB floor across those directories, and runs `cargo clean` only when low free space violates that guardrail.
1. Run the relevant quick/light Cargo checks first, with `./scripts/cargo-guard.sh cargo check -p <project>` as the default starting point.
2. Escalate to `./scripts/cargo-guard.sh cargo check -p <project> --tests` only when test targets, fixtures, macros, or integration surfaces are part of the touched scope.
3. Only if the relevant `cargo check` rung(s) are green, run `./scripts/cargo-guard.sh cargo test -p <project> --no-run`.
4. Only if `--no-run` is green, run `./scripts/cargo-guard.sh cargo test -p <project>` when runtime/behavior validation is actually needed.
5. Ask the user before running a complete suite such as workspace-wide `cargo test` / `just test`.
6. When warnings must be blocking for the selected target set, run `just clippy-strict ...` after the compile ladder. Add `--tests` only when test targets are intentionally in scope. If the deliverable is a shipped binary or another top-level target whose local dependencies must also be warning-clean under plain rustc, also run `just check-strict ...` on that same exact surface (for example `just check-strict -p codex-cli --bin codex`).

Before finalizing a large change to `codex-rs`, run `just fix -p <project>` (from the workspace root or inside `codex-rs`; the recipe routes through `./scripts/cargo-guard.sh`) to fix any linter issues in the code. Prefer scoping with `-p` to avoid slow workspace‑wide Clippy builds; only run `just fix` without `-p` if you changed shared crates. Do not re-run tests after running `fix` or `fmt`.

## The `codex-core` crate

Over time, the `codex-core` crate (defined in `codex-rs/core/`) has become bloated because it is the largest crate, so it is often easier to add something new to `codex-core` rather than refactor out the library code you need so your new code neither takes a dependency on, nor contributes to the size of, `codex-core`.

To that end: **resist adding code to codex-core**!

Particularly when introducing a new concept/feature/API, before adding to `codex-core`, consider whether:

- There is an existing crate other than `codex-core` that is an appropriate place for your new code to live.
- It is time to introduce a new crate to the Cargo workspace for your new functionality. Refactor existing code as necessary to make this happen.

Likewise, when reviewing code, do not hesitate to push back on PRs that would unnecessarily add code to `codex-core`.

## TUI style conventions

See `codex-rs/tui/styles.md`.

## TUI code conventions

- Use concise styling helpers from ratatui’s Stylize trait.
  - Basic spans: use "text".into()
  - Styled spans: use "text".red(), "text".green(), "text".magenta(), "text".dim(), etc.
  - Prefer these over constructing styles with `Span::styled` and `Style` directly.
  - Example: patch summary file lines
    - Desired: vec!["  └ ".into(), "M".red(), " ".dim(), "tui/src/app.rs".dim()]

### TUI Styling (ratatui)

- Prefer Stylize helpers: use "text".dim(), .bold(), .cyan(), .italic(), .underlined() instead of manual Style where possible.
- Prefer simple conversions: use "text".into() for spans and vec![…].into() for lines; when inference is ambiguous (e.g., Paragraph::new/Cell::from), use Line::from(spans) or Span::from(text).
- Computed styles: if the Style is computed at runtime, using `Span::styled` is OK (`Span::from(text).set_style(style)` is also acceptable).
- Avoid hardcoded white: do not use `.white()`; prefer the default foreground (no color).
- Chaining: combine helpers by chaining for readability (e.g., url.cyan().underlined()).
- Single items: prefer "text".into(); use Line::from(text) or Span::from(text) only when the target type isn’t obvious from context, or when using .into() would require extra type annotations.
- Building lines: use vec![…].into() to construct a Line when the target type is obvious and no extra type annotations are needed; otherwise use Line::from(vec![…]).
- Avoid churn: don’t refactor between equivalent forms (Span::styled ↔ set_style, Line::from ↔ .into()) without a clear readability or functional gain; follow file‑local conventions and do not introduce type annotations solely to satisfy .into().
- Compactness: prefer the form that stays on one line after rustfmt; if only one of Line::from(vec![…]) or vec![…].into() avoids wrapping, choose that. If both wrap, pick the one with fewer wrapped lines.

### Text wrapping

- Always use textwrap::wrap to wrap plain strings.
- If you have a ratatui Line and you want to wrap it, use the helpers in tui/src/wrapping.rs, e.g. word_wrap_lines / word_wrap_line.
- If you need to indent wrapped lines, use the initial_indent / subsequent_indent options from RtOptions if you can, rather than writing custom logic.
- If you have a list of lines and you need to prefix them all with some prefix (optionally different on the first vs subsequent lines), use the `prefix_lines` helper from line_utils.

## Tests

### Snapshot tests

This repo uses snapshot tests (via `insta`), especially in `codex-rs/tui`, to validate rendered output.

**Requirement:** any change that affects user-visible UI (including adding new UI) must include
corresponding `insta` snapshot coverage (add a new snapshot test if one doesn't exist yet, or
update the existing snapshot). Review and accept snapshot updates as part of the PR so UI impact
is easy to review and future diffs stay visual.

When UI or text output changes intentionally, update the snapshots as follows:

- Follow the light-first validation ladder for `codex-tui`; only after the lighter checks are green, run the real runtime step that generates or updates snapshots:
  - `./scripts/cargo-guard.sh cargo test -p codex-tui`
- Check what’s pending:
  - `cargo insta pending-snapshots -p codex-tui`
- Review changes by reading the generated `*.snap.new` files directly in the repo, or preview a specific file:
  - `cargo insta show -p codex-tui path/to/file.snap.new`
- Only if you intend to accept all new snapshots in this crate, run:
  - `cargo insta accept -p codex-tui`

If you don’t have the tool:

- `cargo install cargo-insta`

### Test assertions

- Tests should use pretty_assertions::assert_eq for clearer diffs. Import this at the top of the test module if it isn't already.
- Prefer deep equals comparisons whenever possible. Perform `assert_eq!()` on entire objects, rather than individual fields.
- Avoid mutating process environment in tests; prefer passing environment-derived flags or dependencies from above.

### Spawning workspace binaries in tests (Cargo vs Bazel)

- Prefer `codex_utils_cargo_bin::cargo_bin("...")` over `assert_cmd::Command::cargo_bin(...)` or `escargot` when tests need to spawn first-party binaries.
  - Under Bazel, binaries and resources may live under runfiles; use `codex_utils_cargo_bin::cargo_bin` to resolve absolute paths that remain stable after `chdir`.
- When locating fixture files or test resources under Bazel, avoid `env!("CARGO_MANIFEST_DIR")`. Prefer `codex_utils_cargo_bin::find_resource!` so paths resolve correctly under both Cargo and Bazel runfiles.

### Integration tests (core)

- Prefer the utilities in `core_test_support::responses` when writing end-to-end Codex tests.

- All `mount_sse*` helpers return a `ResponseMock`; hold onto it so you can assert against outbound `/responses` POST bodies.
- Use `ResponseMock::single_request()` when a test should only issue one POST, or `ResponseMock::requests()` to inspect every captured `ResponsesRequest`.
- `ResponsesRequest` exposes helpers (`body_json`, `input`, `function_call_output`, `custom_tool_call_output`, `call_output`, `header`, `path`, `query_param`) so assertions can target structured payloads instead of manual JSON digging.
- Build SSE payloads with the provided `ev_*` constructors and the `sse(...)`.
- Prefer `wait_for_event` over `wait_for_event_with_timeout`.
- Prefer `mount_sse_once` over `mount_sse_once_match` or `mount_sse_sequence`

- Typical pattern:

  ```rust
  let mock = responses::mount_sse_once(&server, responses::sse(vec![
      responses::ev_response_created("resp-1"),
      responses::ev_function_call(call_id, "shell", &serde_json::to_string(&args)?),
      responses::ev_completed("resp-1"),
  ])).await;

  codex.submit(Op::UserTurn { ... }).await?;

  // Assert request body if needed.
  let request = mock.single_request();
  // assert using request.function_call_output(call_id) or request.json_body() or other helpers.
  ```

## App-server API Development Best Practices

These guidelines apply to app-server protocol work in `codex-rs`, especially:

- `app-server-protocol/src/protocol/common.rs`
- `app-server-protocol/src/protocol/v2.rs`
- `app-server/README.md`

### Core Rules

- All active API development should happen in app-server v2. Do not add new API surface area to v1.
- Follow payload naming consistently:
  `*Params` for request payloads, `*Response` for responses, and `*Notification` for notifications.
- Expose RPC methods as `<resource>/<method>` and keep `<resource>` singular (for example, `thread/read`, `app/list`).
- Always expose fields as camelCase on the wire with `#[serde(rename_all = "camelCase")]` unless a tagged union or explicit compatibility requirement needs a targeted rename.
- Exception: config RPC payloads are expected to use snake_case to mirror config.toml keys (see the config read/write/list APIs in `app-server-protocol/src/protocol/v2.rs`).
- Always set `#[ts(export_to = "v2/")]` on v2 request/response/notification types so generated TypeScript lands in the correct namespace.
- Never use `#[serde(skip_serializing_if = "Option::is_none")]` for v2 API payload fields.
  Exception: client->server requests that intentionally have no params may use:
  `params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>`.
- Keep Rust and TS wire renames aligned. If a field or variant uses `#[serde(rename = "...")]`, add matching `#[ts(rename = "...")]`.
- For discriminated unions, use explicit tagging in both serializers:
  `#[serde(tag = "type", ...)]` and `#[ts(tag = "type", ...)]`.
- Prefer plain `String` IDs at the API boundary (do UUID parsing/conversion internally if needed).
- Timestamps should be integer Unix seconds (`i64`) and named `*_at` (for example, `created_at`, `updated_at`, `resets_at`).
- For experimental API surface area:
  use `#[experimental("method/or/field")]`, derive `ExperimentalApi` when field-level gating is needed, and use `inspect_params: true` in `common.rs` when only some fields of a method are experimental.

### Client->server request payloads (`*Params`)

- Every optional field must be annotated with `#[ts(optional = nullable)]`. Do not use `#[ts(optional = nullable)]` outside client->server request payloads (`*Params`).
- Optional collection fields (for example `Vec`, `HashMap`) must use `Option<...>` + `#[ts(optional = nullable)]`. Do not use `#[serde(default)]` to model optional collections, and do not use `skip_serializing_if` on v2 payload fields.
- When you want omission to mean `false` for boolean fields, use `#[serde(default, skip_serializing_if = "std::ops::Not::not")] pub field: bool` over `Option<bool>`.
- For new list methods, implement cursor pagination by default:
  request fields `pub cursor: Option<String>` and `pub limit: Option<u32>`,
  response fields `pub data: Vec<...>` and `pub next_cursor: Option<String>`.

### Development Workflow

- Update docs/examples when API behavior changes (at minimum `app-server/README.md`).
- Regenerate schema fixtures when API shapes change:
  `just write-app-server-schema`
  (and `just write-app-server-schema --experimental` when experimental API fixtures are affected).
- Validate `codex-app-server-protocol` with the light-first ladder; run the real `./scripts/cargo-guard.sh cargo test -p codex-app-server-protocol` step only after the lighter checks are green and runtime behavior still needs to be proven.
- Avoid boilerplate tests that only assert experimental field markers for individual
  request fields in `common.rs`; rely on schema generation/tests and behavioral coverage instead.

## Codex CLI Atlas (`upstream/main` baseline)

Last reviewed against `upstream/main` commit `43a69c50eb` on `2026-04-20`.

<!-- Merge-safety anchor: the atlas below is the prompt-resident upstream-main owner map for the shipped Rust Codex CLI; update it whenever main moves a hot path, owner file, or shipped entrypoint. -->

### How to use this atlas

- The workspace-local customization inventory above is the override layer for this checkout.
- The atlas below describes the official `upstream/main` topology of the shipped Rust Codex CLI.
- When local customization and upstream atlas disagree, use the customization inventory first, then use this atlas to understand the upstream base you are diverging from.
- Optimize for owner-first navigation: open the governing seam before opening followers, tests, generated schema, or wide UI files.
- Treat this atlas as the default "open this first" map so a fresh session does not need broad grep just to rediscover the runtime shape.
- Follow one hot path top-down before widening sideways. Most wrong reads here come from opening followers in parallel before the governing seam is locked.
- If the current `HEAD` / `upstream/main` no longer matches the review commit above, re-check the high-churn hotspot files listed below before trusting fine-grained owner details.
- When you land in a folder that already has its own `AGENTS.md`, read that local sub-atlas immediately before editing inside that folder.
- Treat this atlas as a first-pass routing map, not a license to guess gated names, stale symbols, or follower behavior without checking the live owner file.

### Existing local sub-atlases

- `codex-rs/tui/src/bottom_pane/AGENTS.md`
  - Read this once a TUI issue is isolated below `bottom_pane/**`; it owns local doc-sync rules for the composer/paste-burst state-machine cluster.
- `codex-rs/thread-store/src/remote/AGENTS.md`
  - Read this once a task actually lands in `thread-store/src/remote/**`; it owns the checked-in protobuf/regeneration contract for the remote thread-store surface.

### High-churn hotspots and fanout seams

- `codex-rs/cli/src/main.rs`
  - Wide dispatch fanout across almost every shipped command family. Lock the target surface before editing helpers beneath it.
- `codex-rs/login/src/auth/manager.rs`
  - Wide auth fanout: login status, refresh, logout/revoke, external bearer flows, and account metadata all converge here.
- `codex-rs/core/src/session/mod.rs`
  - High-leverage orchestration seam: session startup, initial prompt assembly, service wiring, and turn bootstrap. Small changes here radiate widely.
- `codex-rs/core/src/session/turn.rs`
  - High-leverage sampling/tool loop seam. Changes here often affect prompt shape, tool flow, rollout persistence, and visible runtime events together.
- `codex-rs/models-manager/src/manager.rs`
  - Model catalog/default-selection seam. Small changes here alter available models, defaults, collaboration presets, refresh behavior, and auth-backed `/models` requests together.
- `codex-rs/core/src/tools/spec.rs`
  - Tool-catalog seam with broad runtime/doc/test fallout. Verify handler, exposure, and feature gating together before editing.
- `codex-rs/app-server/src/codex_message_processor.rs`
  - Large RPC fanout seam. Prefer the narrow request handler plus adjacent helpers over broad edits across unrelated methods.
- `codex-rs/app-server/src/config_api.rs`
  - Config RPC fanout seam. Changes here affect read/write semantics, requirements, runtime feature enablement, and user-config reload propagation together.
- `codex-rs/tui/src/app.rs`
  - Governing UI/event-loop seam. Expect broad render/state fallout; isolate the owning submodule or adapter when possible.
- `codex-rs/tui/src/chatwidget.rs`
  - Large visible-surface seam. Changes can alter transcript rendering, slash behavior, overlays, and snapshots together.

### Top-level repo map

- `AGENTS.md`
  - Workspace canon for merge policy, local divergence inventory, Cargo/test rules, and the atlas below.
- `README.md`
  - Mixed fork-level framing plus upstream README. Useful for repo context, but weaker than `codex-rs/README.md` for the Rust runtime skeleton.
- `codex-rs/`
  - The actual Rust workspace and the primary owner surface for shipped CLI behavior.
- `docs/`
  - User/developer docs for config, installation, getting started, and feature usage. Update when behavior or contract changes.
- `justfile`
  - Canonical human-facing command entrypoints. Treat this as the first build/test command map.
- `scripts/cargo-guard.sh`
  - Mandatory Cargo wrapper for this workspace. Cargo validation rules in this AGENTS file assume this wrapper, not raw `cargo`; it also hard-caps guarded Cargo parallelism at 4 jobs unless a lower explicit job count is requested.
- `codex-cli/`
  - Packaging/distribution wrapper for the native CLI artifacts. Adjacent shipped surface, not the Rust runtime owner.
- `BUILD.bazel`, `MODULE.bazel`, `codex-rs/cli/BUILD.bazel`
  - Bazel packaging/build surfaces. Important when build-time file inclusion or release packaging changes.
- `.sangoi/`
  - Separate ignored repo for plans/logs/scratchpads. Not part of the Rust CLI runtime owner tree.
- `mcp-standalone/`, `mcp-sangoi-ia/`, `sdk/`, `node_modules/`
  - Not first-stop surfaces for the shipped Rust CLI unless the task explicitly expands into those domains.
- `codex-rs/tui/src/bottom_pane/AGENTS.md`, `codex-rs/thread-store/src/remote/AGENTS.md`
  - Existing folder-level sub-atlases. The root atlas stays the global map; these files take over once work is isolated into those folders.

### Shipped binaries and entrypoints

- `codex`
  - Owner: `codex-rs/cli/src/main.rs`
  - Role: multitool dispatcher for interactive TUI, headless exec/review, MCP server, app-server tooling, sandbox helpers, feature toggles, login/logout, resume/fork, and debug commands.
  - Command-family split inside `cli/src/main.rs` matters:
    - default/no subcommand plus `resume` / `fork` re-enter the interactive TUI bootstrap path
    - `exec` / `review` route into `codex-rs/exec`
    - `login` / `logout` / `mcp` / `plugin` / `features` stay in CLI management surfaces
    - `sandbox` / `apply` / `exec-server` / `responses*` / `stdio-to-uds` are helper/internal surfaces, not alternate interactive runtimes
- `codex exec`
  - Owner: `codex-rs/exec/src/lib.rs`, fronted by `codex-rs/cli/src/main.rs`
  - Role: headless non-interactive turn/review driver that still talks to the runtime through an in-process app-server client.
- `codex review`
  - Owner: same path as `exec`; review is a headless exec mode, not a separate runtime.
- `codex login` / `codex logout`
  - Owner: `codex-rs/cli/src/login.rs` front door over `codex-rs/login`
  - Role: operator-facing auth entrypoints for the main Codex login surface.
- `codex mcp`
  - Owner: `codex-rs/cli/src/mcp_cmd.rs`
  - Role: operator-facing MCP server config/OAuth management front door; runtime MCP connections and OAuth helpers live deeper than this CLI surface.
- `codex mcp-server`
  - Owner: `codex-rs/mcp-server` crate, dispatched from `codex-rs/cli/src/main.rs`
  - Role: run Codex as an MCP server for other clients.
- `codex app-server`
  - Owner: `codex-rs/app-server/src/main.rs` and `codex-rs/app-server/src/lib.rs`
  - Role: start the app-server transport/runtime directly.
- `codex cloud`
  - Owner: `codex-rs/cloud-tasks`
  - Role: cloud-task browsing/apply surface; shipped command family, but not a first-stop owner for the core local-runtime path.
- `codex-linux-sandbox`
  - Owner: `codex-rs/linux-sandbox/src/lib.rs` and `linux_run_main.rs`
  - Role: Linux helper binary invoked via argv0/helper-path dispatch. It is a helper/runtime surface, not the policy owner.
- `apply_patch`
  - Owner: `codex-rs/apply-patch`
  - Role: internal helper binary plus parser/standalone patch-application contract used by the runtime/tool surface.
- Packaging wrappers and adjacent utility bins
  - `codex-cli/bin/codex.js`, `codex-rs/app-server-test-client`, `codex-rs/file-search`, `codex-rs/responses-api-proxy`, `codex-rs/stdio-to-uds`
  - Use only when the task explicitly touches packaging, test harnesses, or those utilities.

### Runtime mental model

- The shipped `codex` binary is a dispatcher, not the runtime owner.
- Interactive CLI does not talk to `codex-core` directly anymore.
  - `cli` -> `tui` -> app-server client -> app-server request handlers -> `ThreadManager` -> `session/*` runtime -> model/tools -> events back through app-server -> TUI state/render.
- Headless exec/review also does not jump straight to core.
  - `cli` -> `exec` -> in-process app-server client -> app-server handlers -> core runtime.
- The shared embedded transport path is split in two layers:
  - `codex-rs/app-server-client/src/lib.rs` owns the worker-task client facade used by `tui` and `exec`
  - `codex-rs/app-server/src/in_process.rs` hosts the actual app-server runtime over in-memory channels
- `codex-core` owns thread/session/task/tool execution, but its current hot path lives under `core/src/session/*`, not in the old monolithic `core/src/codex.rs` layout from older local branches.
- `codex-state` is the SQLite service for thread metadata, logs, memories, agent jobs, backfill, and remote-control/runtime state. JSONL rollout files remain the canonical transcript/history source.
- `codex-login` owns auth/login persistence and refresh logic. MCP OAuth in `cli/src/mcp_cmd.rs` is a separate auth surface.
- `codex-sandboxing` owns policy selection and request transforms. `codex-linux-sandbox` only enforces the Linux helper/runtime path chosen by that policy layer.
- One-screen owner stack for most CLI bugs:
  - command dispatch: `codex-rs/cli/src/main.rs`
  - surface bootstrap: `codex-rs/tui/src/lib.rs` or `codex-rs/exec/src/lib.rs`
  - embedded transport client: `codex-rs/app-server-client/src/lib.rs`
  - app-server request owner: `codex-rs/app-server/src/codex_message_processor.rs`
  - core runtime owner: `codex-rs/core/src/session/mod.rs` plus `session/turn.rs`
  - transcript truth / metadata: `codex-rs/thread-store`, `codex-rs/rollout`, `codex-rs/state`
  - side owners that often decide behavior before the turn starts: `codex-rs/core/src/config_loader/mod.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/agents_md.rs`, `codex-rs/login`, `codex-rs/sandboxing`

### End-to-end hot paths

#### 1. Interactive TUI turn

- `codex-rs/cli/src/main.rs`
  - Parses `MultitoolCli`.
  - No subcommand -> `run_interactive_tui(...)`.
- `codex-rs/tui/src/lib.rs`
  - Loads config and environment state.
  - Normalizes sandbox/approval/profile/provider inputs.
  - Chooses embedded vs remote app-server target.
  - Starts app-server client/session bootstrap.
- `codex-rs/app-server-client/src/lib.rs`
  - Shared `tui` / `exec` client facade over embedded or remote app-server transports.
  - Owns typed request helpers, event consumption, shutdown, and the transitional `legacy_core` namespace still used by the TUI.
- `codex-rs/app-server/src/in_process.rs`
  - Runs the full app-server semantics over in-memory channels for embedded callers.
  - This is the real in-process runtime host; the client crate above is the caller-facing wrapper.
- `codex-rs/tui/src/app.rs`
  - Owns active UI state machine.
  - Routes `AppEvent::CodexOp` into thread submission.
  - Attaches/replays thread/session history and renders the active widget tree.
- `codex-rs/tui/src/app_server_session.rs`
  - Builds typed app-server RPC requests like `thread/start`, `thread/resume`, `turn/start`, `turn/steer`, `turn/interrupt`, `thread/read`, `thread/list`.
- `codex-rs/app-server/src/message_processor.rs`
  - Top-level JSON-RPC dispatch hub that wires config/auth/fs/thread surfaces together.
- `codex-rs/app-server/src/codex_message_processor.rs`
  - Owns actual request handlers for thread lifecycle and turn operations.
  - Translates app-server payloads into core ops and thread actions.
- `codex-rs/core/src/thread_manager.rs`
  - Creates/resumes/forks threads and owns the live thread registry.
  - Calls `session::Codex::spawn(...)` and waits for session bootstrap.
- `codex-rs/core/src/session/mod.rs`
  - Defines `Codex`, `CodexSpawnArgs`, spawn/submit APIs, and the submission loop entry.
- `codex-rs/core/src/session/session.rs`
  - Owns `Session`, `SessionConfiguration`, and session-scoped runtime wiring.
- `codex-rs/core/src/session/turn.rs`
  - Owns `run_turn(...)`: prompt assembly, plugin/skill injection, model sampling, tool execution, and event emission.
- `codex-rs/core/src/tasks/regular.rs`
  - Default user-turn task wrapper around `run_turn(...)`.
- Events flow back out through app-server notifications and are consumed by:
  - `codex-rs/tui/src/app/app_server_adapter.rs`
  - `codex-rs/tui/src/chatwidget.rs`
  - `codex-rs/tui/src/history_cell.rs`

#### 2. Headless exec/review

- `codex-rs/cli/src/main.rs`
  - `Exec` and `Review` subcommands dispatch to `codex_exec::run_main(...)`.
- `codex-rs/exec/src/cli.rs`
  - CLI parsing for headless mode.
- `codex-rs/exec/src/lib.rs`
  - Starts an in-process app-server client.
  - Starts/resumes threads and turns through typed app-server requests.
  - Renders human output or JSONL output.
- `codex-rs/app-server-client/src/lib.rs`
  - `InProcessAppServerClient` is the embedded transport client used by `exec`.
- After that point the path joins the same app-server -> thread manager -> session runtime flow as the TUI path.

#### 3. Login/auth flow

- `codex-rs/cli/src/login.rs`
  - Operator-facing login/logout/status front door.
- `codex-rs/login/src/lib.rs`
  - Export map for auth flows and managers.
- `codex-rs/login/src/auth/mod.rs`
  - Auth runtime module root.
- `codex-rs/login/src/auth/manager.rs`
  - `AuthManager` owner for auth state, refresh, logout, external bearer flows, agent identity auth, and runtime auth decisions.
- `codex-rs/login/src/auth/storage.rs`
  - Auth persistence owner: file/keyring/auto/ephemeral backends and `auth.json` structure.
- `codex-rs/login/src/device_code_auth.rs`
  - Device-code login flow.
- `codex-rs/login/src/server.rs`
  - Local login web server/browser callback flow.
- Separate auth surface:
  - `codex-rs/cli/src/mcp_cmd.rs` handles MCP OAuth login/logout for MCP servers and does not own normal Codex CLI auth state.

#### 4. Sandbox/exec flow

- `codex-rs/core/src/exec.rs`
  - Builds `ExecRequest` and calls `SandboxManager::transform(...)`.
  - Bridges runtime intent to platform sandbox execution.
- `codex-rs/sandboxing/src/manager.rs`
  - Owner of platform sandbox selection and transform contract.
- `codex-rs/sandboxing/src/policy_transforms.rs`
  - Merges/adds effective permission profiles before execution.
- `codex-rs/linux-sandbox/src/linux_run_main.rs`
  - Linux helper runtime path for bubblewrap/seccomp/legacy landlock enforcement.
- `codex-rs/core/src/tools/handlers/unified_exec.rs`
  - Built-in `exec_command` / `write_stdin` tool handler layer.
- `codex-rs/core/src/unified_exec/process_manager.rs`
  - Long-lived process/session runtime for unified exec.
- `codex-rs/arg0/src/lib.rs`
  - Helper-path and argv0 dispatch owner. Important whenever a helper executable or alias seems missing.

#### 5. Resume/fork/history flow

- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - Canonical rollout -> `Turn` reconstruction for app-server surfaces.
- `codex-rs/app-server/src/codex_message_processor.rs`
  - `thread/read`, `thread/list`, `thread/resume`, `thread/fork` request handlers.
- `codex-rs/tui/src/app_server_session.rs`
  - TUI request/response mapping for those thread operations.
- `codex-rs/tui/src/app.rs`
  - Resume/fork bootstrap and thread/session attach logic.
- `codex-rs/tui/src/chatwidget.rs`
  - Final replay/render consumer for resumed turns.
- `codex-rs/core/src/rollout.rs`
  - Rollout recorder/helpers.
- `codex-rs/state/src/runtime/threads.rs`
  - SQLite summary/index of thread metadata and spawn edges. Useful for lookup and UI summaries, not transcript truth.

#### 6. Prompt, AGENTS, and skill injection flow

- `codex-rs/core/src/agents_md.rs`
  - Loads the preferred home doc (`~/.codex/AGENTS.override.md` before `~/.codex/AGENTS.md`), then walks project root -> cwd taking the first existing project doc per directory from `AGENTS.override.md`, `AGENTS.md`, and configured project-doc fallback filenames into model-visible user instructions.
- `codex-rs/core/src/skills.rs`
  - Projects config into skill load input, resolves env-var dependencies, and tracks implicit skill invocation side effects.
- `codex-rs/core/src/apps/render.rs`
  - Builds the prompt-visible Apps/Connectors section from accessible connectors.
- `codex-rs/core/src/session/mod.rs`
  - Assembly owner that gathers AGENTS/user instructions, apps, skills, plugins, and contextual-user fragments into the initial prompt items for a turn.
- `codex-rs/core/src/session/turn.rs`
  - Consumes the assembled initial context, wraps it into the final `Prompt`, and runs the sampling/tool loop.

#### 7. Config -> AGENTS -> initial prompt assembly

- `codex-rs/tui/src/lib.rs` or `codex-rs/exec/src/lib.rs`
  - Surface bootstrap chooses cwd/overrides/provider/auth assumptions before core starts.
- `codex-rs/core/src/config_loader/mod.rs`
  - Discovers trusted config layers and project-root semantics.
- `codex-rs/core/src/config/mod.rs`
  - Projects the layer stack into effective runtime `Config`.
- `codex-rs/core/src/agents_md.rs`
  - Loads global plus project AGENTS/project-doc instructions.
- `codex-rs/core/src/session/mod.rs`
  - `build_initial_context(...)` assembles developer/contextual-user prompt items from config, AGENTS, apps, skills, plugins, and environment context.
- `codex-rs/core/src/session/turn.rs`
  - Wraps that context into the final model `Prompt` and starts sampling.

#### 8. External MCP/OAuth/tool exposure path

- `codex-rs/cli/src/mcp_cmd.rs`
  - Operator-facing config/OAuth management entrypoint for external MCP servers.
- `codex-rs/rmcp-client/src/lib.rs`
  - OAuth helpers, stdio/server launchers, and `RmcpClient` transport layer for external MCP servers.
- `codex-rs/codex-mcp/src/mcp_connection_manager.rs`
  - Live MCP connection/runtime owner that manages one `RmcpClient` per configured server.
- `codex-rs/core/src/mcp.rs`
  - Config/auth/plugin projection for effective MCP server maps and tool provenance.
- `codex-rs/core/src/mcp_tool_exposure.rs`
  - Governs how MCP tools are filtered/exposed into the runtime-visible tool surface.
- `codex-rs/core/src/tools/spec.rs`
  - Final model-visible tool-catalog assembly point that includes MCP tool exposure.

#### 9. Compaction / history reconstruction flow

- `codex-rs/core/src/tasks/compact.rs`
  - Chooses the compaction implementation for an explicit compact turn and records whether the task ran locally or remotely.
- `codex-rs/core/src/compact.rs`
  - Local/manual/auto compaction owner: builds compact prompts, summarizes history, decides whether initial context must be reinjected, and persists `CompactedItem` replacement history.
- `codex-rs/core/src/compact_remote.rs`
  - Remote compaction owner: processes model-provided compacted history and preserves the mid-turn rule that initial context must be reinserted before the last real user or summary item.
- `codex-rs/core/src/session/mod.rs`
  - `replace_compacted_history(...)`, `build_initial_context(...)`, and `record_context_updates_and_set_reference_context_item(...)` own how compaction resets or re-establishes the runtime context baseline.
- `codex-rs/core/src/session/rollout_reconstruction.rs`
  - Rebuilds history/reference-context state from rollout items after compaction and rollback-sensitive replay.
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - Final visible transcript reconstruction owner for `thread/read` and resume surfaces after compaction has already rewritten history.

#### 10. Config read/write / reload flow

- `codex-rs/app-server/src/config_api.rs`
  - App-server config RPC owner for `read`, `config_requirements_read`, `write_value`, `batch_write`, runtime feature enablement, and per-thread user-config reload propagation.
- `codex-rs/app-server-protocol/src/protocol/v2.rs`
  - Wire payload owner for config RPC params/responses and experimental feature enablement surfaces.
- `codex-rs/core/src/config_loader/mod.rs`
  - Layer discovery, trusted-root/project-root semantics, and cloud requirements loading.
- `codex-rs/core/src/config/mod.rs`
  - Effective runtime `Config` projection and `ConfigService` behavior used beneath the RPC layer.
- `codex-rs/config/src/lib.rs`
  - Raw config-schema/types crate. Open this when the question is about the on-disk contract rather than effective runtime behavior.

#### 11. Model/provider/catalog selection flow

- `codex-rs/models-manager/src/manager.rs`
  - Catalog/default-selection owner: bundled models, `/models` refresh, cache/etag handling, and collaboration-mode presets.
- `codex-rs/model-provider-info/src/lib.rs`
  - Provider capability/config registry: built-in providers, user overrides, retry config, auth metadata, and provider feature flags.
- `codex-rs/model-provider/src/auth.rs`
  - Provider-scoped auth routing. Decides when provider-specific command-backed auth replaces or wraps the base `AuthManager`.
- `codex-rs/codex-api/src/provider.rs`
  - Concrete API endpoint/retry/header owner used to talk to the selected provider.
- `codex-rs/core/src/client.rs`
  - Session-scoped model client: binds provider info, auth, websocket-vs-HTTP transport, and per-turn model/reasoning settings into actual Responses API requests.

#### 12. Remote-control / collab transport flow

- `codex-rs/app-server/src/transport/remote_control/mod.rs`
  - Remote-control websocket bootstrap that wires auth, state runtime, and transport events for app-server remote control.
- `codex-rs/app-server/src/transport/remote_control/protocol.rs`
  - Remote-control envelope/protocol owner.
- `codex-rs/state/src/runtime/remote_control.rs`
  - Persistent enrollment storage keyed by websocket target, account, and app-server client name.
- `codex-rs/core/src/tools/handlers/multi_agents.rs`
  - Legacy collab tool surface.
- `codex-rs/core/src/agent/control.rs`
  - Live spawn/wait/parent-child runtime ownership beneath the tool handlers.

### Crate map by layer

#### Entry surfaces

- `codex-rs/cli`
  - Shipped `codex` multitool binary and shared top-level flag/subcommand dispatch.
- `codex-rs/tui`
  - Fullscreen interactive UI, app-server client bootstrap, and rendered operator surface.
- `codex-rs/exec`
  - Headless execution/review entrypoint.
- `codex-rs/app-server`
  - Embedded/remote transport runtime bridging clients into core threads.
- `codex-rs/app-server-protocol`
  - Typed RPC contracts, schema export, and thread history reconstruction.
- `codex-rs/app-server-client`
  - Embedded/remote app-server client used by TUI and exec.
  - Owns the caller-facing worker-task facade (`request_typed`, `next_event`, shutdown) and exports the transitional `legacy_core` bridge still referenced by TUI.

#### Core runtime and contracts

- `codex-rs/core`
  - Main session/thread/task/tool runtime.
- `codex-rs/protocol`
  - Core `Op`, `Event`, config, item, approval, and wire/domain types used inside runtime.
- `codex-rs/config`
  - Low-level config TOML/schema/types and config-layer helpers.
- `codex-rs/features`
  - Feature registry and feature metadata. Use this to verify gating; do not invent feature canon elsewhere.
- `codex-rs/state`
  - SQLite service for thread metadata/logs plus first-class runtime state such as memories, agent jobs, backfill, and remote-control/runtime records.
- `codex-rs/thread-store`
  - Durable thread/session persistence boundary; local reads compose rollout files with optional SQLite metadata from `codex-state`.
- `codex-rs/rollout`
  - Rollout file formats and persistence helpers.
- `codex-rs/codex-api`
  - Responses/realtime HTTP+websocket client and API error mapping underneath higher-level model/back-end clients.

#### Auth, providers, and model stack

- `codex-rs/login`
  - Login flows, auth persistence, refresh, agent identity auth.
- `codex-rs/codex-client`
  - High-level Codex service client helpers.
- `codex-rs/backend-client`
  - Lower-level backend API client work.
- `codex-rs/chatgpt`
  - ChatGPT-specific client/integration helpers.
- `codex-rs/model-provider-info`
  - Provider metadata and capability registry.
- `codex-rs/model-provider`
  - Provider transport/auth resolution layer, including provider-specific auth paths that can bypass normal `auth.json` assumptions.
- `codex-rs/models-manager`
  - Provider/model catalog management, refresh, collaboration-mode presets.
- `codex-rs/ollama`, `codex-rs/lmstudio`
  - OSS/local-provider integrations.
- `codex-rs/network-proxy`
  - Managed network proxy support and audit metadata.
- `codex-rs/secrets`
  - Secret storage and redaction helpers. Relevant when auth/plugin/MCP/app config starts depending on stored secrets instead of plain config entries.

#### Tools, skills, apps, and plugins

- `codex-rs/codex-mcp`
  - MCP connection management and runtime integration.
- `codex-rs/rmcp-client`
  - External MCP transport/OAuth layer (`RmcpClient`, streamable HTTP auth discovery, stdio launchers) used beneath `codex-mcp` and `codex mcp`.
- `codex-rs/mcp-server`
  - Codex-as-MCP-server binary implementation.
- `codex-rs/connectors`
  - Connector/app discovery and connector metadata helpers.
- `codex-rs/plugin`
  - Plugin packaging/runtime surfaces.
- `codex-rs/core-plugins`
  - Plugin discovery/install/runtime-ready projection used by `codex-core`.
- `codex-rs/tools`
  - Shared tool-schema / tool-registry-plan crate used by `core/src/tools/spec.rs` and related handler registration paths.
- `codex-rs/instructions`
  - Instruction fragment assets and canon helpers.
- `codex-rs/skills`
  - Skill loading/config helpers.
- `codex-rs/core-skills`
  - Bundled core skill surfaces.
- `codex-rs/hooks`
  - Hook config schema and hook execution contract.
- `codex-rs/apply-patch`
  - Patch grammar/parser plus standalone `apply_patch` helper executable contract.
- `codex-rs/code-mode`
  - `exec` / `wait` tool description and runtime support for code-mode surfaces.
- `codex-rs/collaboration-mode-templates`
  - Collaboration-mode template assets.

#### Sandbox, process, and OS integration

- `codex-rs/sandboxing`
  - Platform-independent sandbox policy owner.
- `codex-rs/linux-sandbox`
  - Linux helper/runtime binary for sandbox enforcement.
- `codex-rs/exec-server`
  - Execution/filesystem/environment services used by app-server and runtime.
- `codex-rs/execpolicy`, `codex-rs/execpolicy-legacy`
  - Exec policy parsing/compat surfaces.
- `codex-rs/process-hardening`
  - Host process hardening helpers.
- `codex-rs/shell-command`, `codex-rs/shell-escalation`, `codex-rs/terminal-detection`
  - Shell/runtime support crates.
- `codex-rs/keyring-store`
  - Keyring storage support used by auth persistence.
- `codex-rs/arg0`
  - argv0-based helper dispatch and helper-path derivation.
- `codex-rs/install-context`
  - Detects standalone/npm/bun/brew install context and bundled resource lookup (for example bundled `rg`); open this for packaging/resource-resolution surprises.
- `codex-rs/realtime-webrtc`
  - Native realtime WebRTC helper. Not a first stop unless the bug crosses macOS realtime voice/WebRTC behavior.

#### Peripheral but still relevant

- `codex-rs/analytics`, `codex-rs/feedback`, `codex-rs/otel`
  - Telemetry, feedback, and observability surfaces.
- `codex-rs/file-search`
  - File-search utility surface.
- `codex-rs/cloud-requirements`
  - Cloud-hosted config requirements loader used during config bootstrap.
- `codex-rs/cloud-tasks`, `codex-rs/cloud-tasks-client`
  - `codex cloud` command family and its backend client. Relevant only when the task explicitly touches cloud-task browsing/apply behavior.
- `codex-rs/codex-backend-openapi-models`
  - Generated backend model follower crate. Open only after a backend-client/API owner already points there.
- `codex-rs/response-debug-context`
  - Extracts request/auth/debug headers into operator-facing diagnostics.
- `codex-rs/responses-api-proxy`, `codex-rs/stdio-to-uds`, `codex-rs/app-server-test-client`, `codex-rs/debug-client`
  - Test/support/bridge utilities.
- `codex-rs/utils/**`
  - Small focused support crates. Open only when a path explicitly crosses them.

### Primary owner files inside `codex-rs/core`

#### Runtime root

- `codex-rs/core/src/lib.rs`
  - Crate skeleton/export map. Open this first to confirm the current owner split before assuming older file names.
- `codex-rs/core/src/thread_manager.rs`
  - Global thread registry, spawn/resume/fork, watcher setup, and thread bootstrap around `session::Codex::spawn(...)`.
- `codex-rs/core/src/codex_thread.rs`
  - Handle object presented to callers; wraps submit/next_event/config snapshot/state DB access for one live thread.

#### Session runtime split on `upstream/main`

- `codex-rs/core/src/session/mod.rs`
  - Public core runtime entrypoint for current main.
  - Defines `Codex`, spawn args/return types, submit APIs, event receiver path, and top-level session module wiring.
- `codex-rs/core/src/session/session.rs`
  - `Session`, `SessionConfiguration`, `SessionSettingsUpdate` and the long-lived session owner.
  - Open this when the question is "what belongs to the session as a whole?".
- `codex-rs/core/src/session/handlers.rs`
  - Submission loop and op dispatch glue.
  - Open this when tracing how incoming `Op`s are handled.
- `codex-rs/core/src/session/turn.rs`
  - `run_turn(...)` and the real model/tool execution loop.
  - Open this for sampling, prompt construction, plugin/skill injection, tool execution, or live event emission questions.
- `codex-rs/core/src/session/turn_context.rs`
  - Per-turn snapshot/context owner. Open this when the question is about model, sandbox, cwd, telemetry, or per-turn state captured before execution.
- `codex-rs/core/src/session/review.rs`
  - Review-mode session helpers.
- `codex-rs/core/src/session/mcp.rs`
  - Session-side MCP helpers.
- `codex-rs/core/src/session/agent_task_lifecycle.rs`
  - Agent-task lifecycle helpers tightly coupled to session runtime.
- `codex-rs/core/src/session/rollout_reconstruction.rs`
  - Runtime-side rollout reconstruction helpers.

#### Task system

- `codex-rs/core/src/tasks/mod.rs`
  - Generic task abstraction, task spawn/abort/finish lifecycle, session task context.
- `codex-rs/core/src/tasks/regular.rs`
  - Default user-turn task wrapper around `session::turn::run_turn(...)`.
- `codex-rs/core/src/tasks/review.rs`
  - Review-mode task.
- `codex-rs/core/src/tasks/compact.rs`
  - Explicit compact task owner.
- `codex-rs/core/src/tasks/undo.rs`
  - Undo task owner.
- `codex-rs/core/src/tasks/ghost_snapshot.rs`
  - Ghost snapshot task owner.
- `codex-rs/core/src/tasks/user_shell.rs`
  - User shell command task owner.

#### Mutable runtime state partitioning

- `codex-rs/core/src/state/service.rs`
  - `SessionServices`: long-lived shared managers/services for a session/thread.
- `codex-rs/core/src/state/session.rs`
  - `SessionState`: mutable session-scoped history/context/persistent-in-turn state.
- `codex-rs/core/src/state/turn.rs`
  - `TurnState`, `ActiveTurn`, pending approvals/user-input/dynamic-tools queues, task ownership.
- `codex-rs/core/src/state/mod.rs`
  - Map of the state partition.

#### Config and environment

- `codex-rs/core/src/config/mod.rs`
  - Effective runtime config owner.
  - This is where layered config becomes the concrete runtime `Config` used by CLI/app-server/core.
- `codex-rs/core/src/config_loader/mod.rs`
  - Config layer-stack loader and trust-aware project-config resolution.
  - Open this before blaming `Config` when the problem is really layer order or project-root discovery.
- `codex-rs/config/src/lib.rs`
  - Low-level config schema/types crate.
  - Use when the question is raw TOML/types/schema, not effective runtime projection.
- `codex-rs/features/src/lib.rs`
  - Feature registry. Use to verify gating, default stage, or deprecation/removal.

#### Instructions, skills, project docs, and apps

- `codex-rs/core/src/agents_md.rs`
  - `AgentsMdManager` owner for global + project `AGENTS.md` discovery, project-root -> cwd concatenation, and model-visible instruction assembly.
- `codex-rs/core/src/skills.rs`
  - Thin owner around `codex-core-skills` integration points: load input projection, dependency prompting, and implicit-skill telemetry hooks used by `session::turn`.
- `codex-rs/core/src/instructions/mod.rs`
  - Thin re-export of the bundled instruction canon from `codex-instructions`. Open this only when tracing where the instruction assets come from.
- `codex-rs/core/src/apps/render.rs`
  - Prompt fragment owner for the Apps/Connectors section rendered into model-visible instructions.

#### Model and prompt path

- `codex-rs/core/src/client.rs`
  - Transport owner split between session-scoped `ModelClient` and per-turn `ModelClientSession`.
- `codex-rs/core/src/client_common.rs`
  - Shared prompt/request-response plumbing.
- `codex-rs/core/src/compact.rs`
  - Local compaction and replacement-history construction.
- `codex-rs/core/src/compact_remote.rs`
  - Remote compaction and post-compaction replacement-history processing.
- `codex-rs/core/src/context_manager/*`
  - Context/history normalization and update helpers.
- `codex-rs/core/src/contextual_user_message.rs`
  - Contextual user message helpers and markers.
- `codex-rs/core/src/prompt_debug.rs`
  - Prompt-debug rendering helpers and prompt-input materialization for debugging/operator surfaces.
- `codex-rs/core/src/realtime_context.rs`, `realtime_conversation.rs`, `realtime_prompt.rs`
  - Realtime voice/realtime conversation support.

#### Tools and execution

- `codex-rs/core/src/tools/spec.rs`
  - Model-visible tool catalog and handler registration. Always open this before assuming a tool exists or how it is exposed.
- `codex-rs/core/src/tools/router.rs`
  - Response-item -> tool-call parsing and dispatch prep.
- `codex-rs/core/src/tools/registry.rs`
  - Tool handler registry and execution lookup.
- `codex-rs/core/src/tools/orchestrator.rs`
  - Sandbox/approval/escalation orchestration for tool execution.
- `codex-rs/core/src/tools/context.rs`
  - Tool-call context tracking.
- `codex-rs/core/src/tools/parallel.rs`
  - Parallel tool execution coordination.
- `codex-rs/core/src/tools/handlers/*`
  - Per-tool handlers. Important builtins on current main include:
    - `shell.rs`
    - `unified_exec.rs`
    - `apply_patch.rs`
    - `multi_agents.rs`
    - `multi_agents_v2.rs`
    - `plan.rs`
    - `request_permissions.rs`
    - `request_user_input.rs`
    - `mcp.rs`
    - `mcp_resource.rs`
    - `tool_search.rs`
    - `tool_suggest.rs`
    - `view_image.rs`
    - `js_repl.rs`
    - `agent_jobs.rs`
- `codex-rs/core/src/tools/runtimes/*`
  - Runtime helpers behind tool handlers.
- `codex-rs/core/src/unified_exec/*`
  - Unified exec runtime internals; `process_manager.rs` is the main owner for long-lived terminal/process state.
- `codex-rs/core/src/exec.rs`
  - Core exec request builder and bridge into sandbox execution.
- `codex-rs/core/src/sandboxing/mod.rs`
  - Core-facing exec/sandbox request types and conversions.

#### Collaboration, MCP, plugins, memories, guardian

- `codex-rs/core/src/agent/control.rs`
  - Multi-agent control-plane owner within a root thread tree.
- `codex-rs/core/src/agent/*`
  - Agent registry/mailbox/status/role plumbing.
- `codex-rs/core/src/agent_identity/*`
  - Agent identity and background task registration helpers.
- `codex-rs/core/src/codex_delegate.rs`
  - Parent-child delegation bridge and inter-agent event/approval plumbing.
- `codex-rs/core/src/mcp.rs`, `mcp_tool_call.rs`, `mcp_tool_exposure.rs`, `mcp_openai_file.rs`
  - MCP config/auth/plugin projection plus runtime integration helpers.
  - `core/src/mcp.rs` owns `McpManager`'s configured/effective server views and tool provenance helpers, but live session connections are still initialized elsewhere during session startup.
- `codex-rs/core/src/plugins/*`
  - Plugin/install/marketplace/discovery surfaces.
- `codex-rs/core/src/connectors.rs`
  - Connector helpers.
- `codex-rs/core/src/memories/*`
  - Memory tool phases, storage, citations, and prompt fragments.
- `codex-rs/core/src/guardian/*`
  - Guardian approval/review support.
- `codex-rs/core/src/agents_md.rs`
  - AGENTS.md manager and AGENTS/local AGENTS file name owners on current main.

### TUI, app-server, and protocol owner map

#### TUI

- `codex-rs/tui/src/lib.rs`
  - Interactive startup root.
  - Loads config, checks auth restrictions, starts embedded/remote app-server, sets up logging, handles resume/fork picker bootstrap.
- `codex-rs/tui/src/app.rs`
  - Governing UI state machine.
  - Owns app event loop, active-thread submission, thread/session attach, draw/render scheduling, overlays, and high-level routing.
- `codex-rs/tui/src/chatwidget.rs`
  - Main visible transcript/render surface.
  - Owns transcript, overlays, many operator-visible state changes, live notification consumption, and replay application.
- `codex-rs/tui/src/tui.rs`
  - Terminal draw/flush owner. Open this when corruption, flicker, synchronized-update, or screen invalidation bugs reproduce only in a live terminal.
- `codex-rs/tui/src/app_server_session.rs`
  - Typed TUI <-> app-server RPC boundary.
- `codex-rs/tui/src/app/app_server_adapter.rs`
  - Hybrid adapter between app-server protocol notifications and older TUI event model. Use this when the payload looks right but the UI mutates wrong.
- `codex-rs/tui/src/app/app_server_requests.rs`
  - Correlates incoming server requests with TUI-side approvals, request-user-input answers, and MCP elicitation responses.
- `codex-rs/tui/src/app/loaded_threads.rs`
  - Pure spawn-tree walk used to discover loaded subagent descendants for the active primary thread.
- `codex-rs/tui/src/resume_picker.rs`
  - Resume/fork picker UI.
- `codex-rs/tui/src/slash_command.rs`
  - Built-in slash-command canon.
- `codex-rs/tui/src/bottom_pane/mod.rs`
  - Composer/modal/footer state owner. Start here for slash popup state, composer state retention, or modal-driven redraw bugs.
- `codex-rs/tui/src/bottom_pane/AGENTS.md`
  - Local sub-atlas for bottom-pane state-machine work. Read it before editing below `bottom_pane/**`.
- `codex-rs/tui/src/history_cell.rs`
  - Transcript/history cell rendering.
- `codex-rs/tui/src/status/*`
  - Status card/account/rate-limit widgets.
- `codex-rs/tui/src/bottom_pane/*`
  - Composer/footer/modal/render followers. Do not start here unless the bug is already isolated below `App`/`ChatWidget`.

#### App-server

- `codex-rs/app-server/src/lib.rs`
  - App-server architecture root.
  - Owns transport bootstrap, processor loop vs outbound router split, and server startup.
- `codex-rs/app-server/src/message_processor.rs`
  - JSON-RPC request/notification hub that wires config/auth/fs/thread/plugin/runtime services.
- `codex-rs/app-server/src/in_process.rs`
  - In-process runtime host used by embedded `tui` / `exec` callers. Preserves app-server semantics without a process boundary.
- `codex-rs/app-server/src/codex_message_processor.rs`
  - Main server-side handler owner for thread lifecycle, turn operations, auth/account RPCs, config RPCs, FS RPCs, plugins, apps, and related helpers.
- `codex-rs/app-server/src/dynamic_tools.rs`
  - Bridges client-side dynamic-tool responses back into core `Op::DynamicToolResponse`.
- `codex-rs/app-server/src/outgoing_message.rs`
  - Outbound notification fanout and message send helpers. Open this when events exist but never reach clients.
- `codex-rs/app-server/src/thread_state.rs`
  - Running-thread state + current-turn snapshot/rejoin owner.
- `codex-rs/app-server/src/thread_status.rs`
  - Derived thread status owner.
- `codex-rs/app-server/src/config_api.rs`
  - Config RPC owner: read/write/batch-write semantics, requirements reads, experimental feature enablement, and user-config reload propagation.
- `codex-rs/app-server/src/fs_api.rs`
  - FS RPC owner.
- `codex-rs/app-server/src/transport/remote_control/mod.rs`
  - Remote-control transport bootstrap. Start here for websocket remote-control enable/disable/enrollment behavior instead of normal thread RPCs.
- `codex-rs/app-server/src/transport/*`
  - Transport-specific wiring. Do not start here unless the problem is connection-level rather than runtime/state-level.

#### App-server protocol

- `codex-rs/app-server-protocol/src/protocol/common.rs`
  - Shared request enum and RPC method naming.
- `codex-rs/app-server-protocol/src/protocol/v2.rs`
  - Active app-server API payloads. Treat v2 as the owner for new API work.
- `codex-rs/app-server-protocol/src/protocol/v1.rs`
  - Legacy API surface only.
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - Rollout reconstruction into visible `Turn` history plus history truncation helpers.
- `codex-rs/app-server-protocol/src/export.rs`, `schema_fixtures.rs`, `schema/**`
  - Schema/generation followers. Use only after the owning Rust payloads are locked.

### Auth, sandbox, and persistence owner map

- `codex-rs/login/src/lib.rs`
  - Auth export map.
- `codex-rs/login/src/auth/manager.rs`
  - `AuthManager` owner for login status, refresh, logout, external bearer flows, auth mode decisions, and runtime auth state.
- `codex-rs/login/src/auth/storage.rs`
  - Persistence backend owner: `auth.json`, keyring-backed storage, delete/save/load behavior, auth file locking.
- `codex-rs/login/src/auth/external_bearer.rs`
  - Provider command-backed bearer auth owner: caches command output and refresh intervals for provider-scoped external auth.
- `codex-rs/login/src/auth/revoke.rs`
  - Token-revocation path used by logout-with-revoke.
- `codex-rs/login/src/device_code_auth.rs`
  - Device-code flow.
- `codex-rs/login/src/server.rs`
  - Local browser login server flow.
- `codex-rs/login/src/pkce.rs`
  - PKCE verifier/challenge generation for browser login.
- `codex-rs/login/src/token_data.rs`
  - Parsed token payload/JWT-claim owner for account id, plan type, FedRAMP routing, and expiration metadata loaded from `auth.json`.
- `codex-rs/login/src/agent_identity.rs`
  - Agent-identity/background-task auth helpers.
- `codex-rs/rmcp-client/src/lib.rs`
  - External MCP OAuth/token helpers and transport auth discovery. Open this for `codex mcp login/logout` behavior instead of stopping in `cli/src/mcp_cmd.rs`.
- `codex-rs/secrets/src/lib.rs`
  - Secret storage/redaction contract when auth or plugin/MCP flows start depending on stored secret material.
- `codex-rs/core/src/exec.rs`
  - Core exec request builder.
- `codex-rs/core/src/sandboxing/mod.rs`
  - Core adapter layer that converts transformed sandbox requests into executable requests/env markers.
- `codex-rs/sandboxing/src/manager.rs`
  - Policy owner for selecting/translating sandbox backend.
- `codex-rs/sandboxing/src/policy_transforms.rs`
  - Permission-profile merge and transform helpers.
- `codex-rs/core/src/tools/runtimes/shell/unix_escalation.rs`
  - Shell approval/escalation runtime: user approval prompts, Guardian handoff, sandbox transform call, and denial/result mapping.
- `codex-rs/core/src/guardian/review.rs`
  - Guardian auto-review routing and approval gate helpers.
- `codex-rs/linux-sandbox/src/linux_run_main.rs`
  - Linux helper runtime path.
- `codex-rs/state/src/lib.rs`
  - Crate contract: SQLite-backed service for thread metadata/logs plus first-class runtime state such as memories, agent jobs, backfill, and remote-control records.
- `codex-rs/state/src/runtime.rs`
  - `StateRuntime` owner for DB init, migrations, and runtime handles.
- `codex-rs/state/src/runtime/threads.rs`
  - Thread metadata, dynamic tools summary, and spawn-edge persistence.
- `codex-rs/state/src/runtime/agent_jobs.rs`, `memories.rs`, `remote_control.rs`
  - State followers for queued agent-job persistence, memory storage, and remote-control/runtime coordination surfaces.
- `codex-rs/state/src/log_db.rs`
  - Logs DB surface.
- `codex-rs/thread-store/src/store.rs`
  - `ThreadStore` trait and persistence contract for list/read/write/fork-style thread access.
- `codex-rs/thread-store/src/local/read_thread.rs`
  - Local read path that consults SQLite metadata when present and falls back to rollout-backed reconstruction.
- `codex-rs/model-provider/src/auth.rs`
  - Provider-scoped auth override seam. Open this when a non-default provider ignores normal login assumptions.

### Hot function and type index

#### CLI, packaging, and startup dispatch

- `codex-cli/bin/codex.js`
  - Resolves the platform package, locates the vendored native `codex` binary, and `spawn`s it.
  - Packaging wrapper only; do not start here for runtime logic.
- `codex-rs/cli/src/main.rs`
  - `main()`
    - Parses `MultitoolCli`, chooses the subcommand path, and routes `Review` through `codex_exec::run_main(...)`.
  - `run_interactive_tui(...)`
    - Default no-subcommand interactive front door.
- `codex-rs/cli/src/mcp_cmd.rs`
  - `run(...)` / `perform_oauth_login_retry_without_scopes(...)`
    - Front door for MCP config/OAuth management. Do not confuse this with the live MCP runtime owner.
- `codex-rs/cli/src/login.rs`
  - Operator-facing login/status/logout CLI front door for the main Codex auth surface.
- `codex-rs/arg0/src/lib.rs`
  - argv0 helper dispatch for helper binaries such as `codex-linux-sandbox`, `apply_patch`, and exec-server related helpers.
- `codex-rs/exec/src/lib.rs`
  - `run_main(...)`
    - Headless startup root.
    - Loads effective config, enforces login restrictions, starts an in-process app-server client, sends `thread/start` or `thread/resume`, then `turn/start` or `review/start`.
  - `thread_start_params_from_config(...)`
    - Projects effective config into app-server `ThreadStartParams`.
  - `thread_resume_params_from_config(...)`
    - Projects effective config plus thread id into app-server `ThreadResumeParams`.
- `codex-rs/app-server-client/src/lib.rs`
  - `InProcessAppServerClient::start(...)`
    - Starts the caller-facing worker task over the embedded app-server runtime.
  - `request_typed(...)`, `next_event(...)`, `shutdown(...)`
    - Shared typed request/event/shutdown surface used by both `tui` and `exec`.

#### Core thread/session/task runtime

- `codex-rs/core/src/thread_manager.rs`
  - `spawn_thread(...)` / `spawn_thread_with_source(...)`
    - Assemble runtime managers (`ModelsManager`, skills, plugins, MCP, state) and call `session::Codex::spawn(...)`.
  - `finalize_thread_spawn(...)`
    - Waits for the first `SessionConfigured` event, then inserts the live thread into the manager map.
  - `send_op(...)`
    - Forwards a runtime `Op` into the target live thread.
- `codex-rs/core/src/codex_thread.rs`
  - `submit(...)`, `next_event(...)`, `steer_input(...)`
    - Public handle methods for one live thread.
  - This file is a conduit wrapper, not the owner of session behavior.
- `codex-rs/core/src/session/mod.rs`
  - `Codex`
    - Current main runtime entry handle.
  - `Codex::spawn(...)`
    - Builds the session/task/event channels and returns the live runtime handle used by `ThreadManager`.
  - `build_initial_context(...)`
    - Assembles AGENTS/user instructions, apps, skills, plugins, and contextual-user fragments into the initial prompt items before sampling starts.
- `codex-rs/core/src/session/session.rs`
  - `Session`
    - Long-lived session owner.
  - `SessionConfiguration`
    - Session-scoped immutable-ish config snapshot.
  - `Session::new(...)`
    - Assembles `SessionServices`, creates the `ModelClient`, initializes rollout/state/MCP connection management, and emits `SessionConfigured`.
- `codex-rs/core/src/session/handlers.rs`
  - Central `Op` dispatcher for a live session.
  - `Op::UserInput` and `Op::UserTurn` arrive here first in core, become a `SessionSettingsUpdate`, create turn context, and then spawn the appropriate task.
- `codex-rs/core/src/tasks/mod.rs`
  - `Session::spawn_task(...)` / `start_task(...)`
    - Own active-turn task lifecycle, cancellation, finish bookkeeping, and completion emission.
- `codex-rs/core/src/tasks/regular.rs`
  - `RegularTask`
    - Default user-turn task wrapper that enters `session::turn::run_turn(...)`.
- `codex-rs/core/src/tasks/compact.rs`
  - `CompactTask`
    - `SessionTask` owner that routes an explicit compact turn to local vs remote compaction.
- `codex-rs/core/src/session/turn_context.rs`
  - `TurnContext`
    - Per-turn snapshot of cwd, sandbox/approval mode, model/provider, visible tools, telemetry, and related runtime state captured before sampling starts.
- `codex-rs/core/src/session/turn.rs`
  - `run_turn(...)`
    - Real turn engine.
    - Consumes the initial context from `session/mod.rs`, builds the final `Prompt` and visible tool surface, opens the model stream, handles streamed tool calls and assistant output, checks compaction/continuation conditions, and emits events back to callers.
- `codex-rs/core/src/compact.rs`
  - `run_compact_task(...)` / `run_inline_auto_compact_task(...)`
    - Local compaction entrypoints for explicit and auto-compact turns.
  - `insert_initial_context_before_last_real_user_or_summary(...)`
    - Canonical insertion rule for replacement history after compaction.
- `codex-rs/core/src/compact_remote.rs`
  - `run_remote_compact_task(...)` / `run_inline_remote_auto_compact_task(...)`
    - Remote compaction entrypoints.
  - `process_compacted_history(...)`
    - Filters remote compact output and reapplies canonical initial context placement.
- `codex-rs/core/src/session/rollout_reconstruction.rs`
  - `reconstruct_history_from_rollout(...)`
    - Rebuilds compacted history and reference-context state from rollout items.
- `codex-rs/core/src/client.rs`
  - `ModelClient`
    - Session-scoped model/backend client.
  - `ModelClientSession`
    - Turn-scoped sampling view over the shared client.
  - `stream(...)`
    - Chooses the transport path (for example websocket vs HTTP Responses flow) and yields model events/tool-call items.
- `codex-rs/core/src/config/mod.rs`
  - `ConfigBuilder::build(...)`
    - Produces the effective runtime `Config` consumed by CLI/app-server/core.
- `codex-rs/core/src/config_loader/mod.rs`
  - `load_config_layers_state(...)`
    - Discovers config layers, enforces trust/project-root rules, and explains which layer won before `ConfigBuilder` projects it.
- `codex-rs/core/src/session/mod.rs`
  - `build_initial_context(...)`
    - First-open seam for “why is this developer/contextual-user prompt fragment present or missing?” questions.
- `codex-rs/core/src/agents_md.rs`
  - `AgentsMdManager::user_instructions(...)` / `instruction_sources(...)`
    - Build the model-visible AGENTS/project-doc stack and track which files supplied it.
- `codex-rs/models-manager/src/manager.rs`
  - `list_models(...)` / `get_default_model(...)` / `refresh_if_new_etag(...)`
    - Catalog/default-model ownership for `/models` refresh, cache, and collaboration-mode presets.
- `codex-rs/core/src/skills.rs`
  - `skills_load_input_from_config(...)`
    - Converts effective config plus skill roots into the load request consumed by `codex-core-skills`.
  - `resolve_skill_dependencies_for_turn(...)`
    - Prompts for missing skill env vars and stores them session-locally.
  - `maybe_emit_implicit_skill_invocation(...)`
    - Records implicit skill-use side effects/telemetry for command-driven turns.
- `codex-rs/core/src/tools/router.rs`
  - Parses model response items into tool-call intents and prepares dispatch.
- `codex-rs/core/src/tools/registry.rs`
  - Owns tool registration and execution lookup.
  - `build(...)`
    - Returns the model-visible tool specs and the executable registry together.
- `codex-rs/core/src/tools/orchestrator.rs`
  - Executes tools under approval/sandbox/escalation/retry rules.
- `codex-rs/core/src/mcp.rs`
  - `McpManager::configured_servers(...)`, `effective_servers(...)`, `tool_plugin_provenance(...)`
    - Config/auth/plugin projection for MCP server maps and tool provenance.
  - Do not confuse this with the live session-side MCP connection manager built during `Session::new(...)`.

#### TUI, app-server, and protocol

- `codex-rs/tui/src/lib.rs`
  - `run_main(...)`
    - Interactive startup root: config bootstrap, auth checks, embedded-vs-remote app-server choice, and initial session bootstrap.
- `codex-rs/tui/src/app.rs`
  - `App::run(...)`
    - Main event loop that multiplexes terminal input, app events, active-thread events, and app-server notifications.
  - `handle_tui_event(...)`
    - Draw/update bridge into `ChatWidget`.
  - `try_submit_active_thread_op_via_app_server(...)`
    - Active-thread submission path for app-server-backed sessions.
  - `backfill_loaded_subagent_threads(...)`
    - Rehydrates already-running child threads into the UI model.
- `codex-rs/tui/src/chatwidget.rs`
  - `handle_key_event(...)`
    - High-traffic operator input handler.
  - `submit_op(...)`
    - Sends ops upward toward direct/app-server-backed execution.
  - `as_renderable(...)` / `render(...)`
    - Final transcript/layout composition for the visible surface.
- `codex-rs/tui/src/app/app_server_adapter.rs`
  - `handle_app_server_event(...)`
    - Hybrid bridge from app-server notifications/requests into older TUI-local state/event paths.
- `codex-rs/tui/src/app_server_session.rs`
  - Typed wrappers for `thread/start`, `thread/resume`, `thread/list`, `thread/read`, `thread/fork`, and `turn/start`.
  - This is the client-side owner for request shape before the server sees it.
- `codex-rs/tui/src/app/app_server_requests.rs`
  - `PendingAppServerRequests::note_server_request(...)` / `take_resolution(...)`
    - Map app-server request ids to TUI-side approval or user-input resolutions.
- `codex-rs/tui/src/app/loaded_threads.rs`
  - `find_loaded_subagent_threads_for_primary(...)`
    - Rebuilds the loaded subagent cache from a flat loaded-thread list using spawn edges.
- `codex-rs/tui/src/slash_command.rs`
  - Built-in command canon and availability checks.
- `codex-rs/tui/src/bottom_pane/slash_commands.rs`
  - Filters visible built-ins for the slash popup/composer state.
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs`
  - Executes slash commands after they are chosen.
- `codex-rs/tui/src/resume_picker.rs`
  - Resume/fork picker that pages `thread/list` results and drives follow-up `thread/read`.
- `codex-rs/tui/src/tui.rs`
  - Terminal draw pipeline and synchronized-update handling.
  - Open this when a live terminal misrenders while replay/VT100 snapshots look correct.
- `codex-rs/app-server/src/lib.rs`
  - `run_main(...)` / `run_main_with_transport(...)`
    - App-server startup roots for embedded or explicit transport use.
- `codex-rs/app-server/src/in_process.rs`
  - `start(...)` / `InProcessClientHandle`
    - Embedded runtime host beneath `codex-app-server-client`.
- `codex-rs/app-server/src/message_processor.rs`
  - Connection/session/message root that wires transports to runtime-facing processors.
- `codex-rs/app-server/src/codex_message_processor.rs`
  - `process_request(...)`
    - Routes `thread/start`, `thread/resume`, `thread/list`, `thread/read`, `turn/start`, and adjacent RPCs.
  - `thread_start(...)`
    - Creates a new thread and sends `SessionConfigured`-derived data back to the caller.
  - `thread_list(...)`
    - Builds picker rows from thread-store data plus runtime overlays.
  - `thread_read(...)`
    - Returns sparse thread/turn state plus reconstructed history when requested.
  - `thread_resume(...)`
    - Reattaches to an existing session/history using precedence `history > path > thread_id`.
  - `turn_start(...)`
    - Translates app-server turn params into core `Op::UserInput`/turn execution.
- `codex-rs/app-server/src/dynamic_tools.rs`
  - `on_call_response(...)`
    - Converts app-server dynamic-tool results back into `Op::DynamicToolResponse`.
- `codex-rs/app-server/src/config_api.rs`
  - `read(...)` / `write_value(...)` / `batch_write(...)`
    - App-server owner for config RPC semantics.
  - `load_latest_config(...)`
    - Rebuilds effective runtime config under the current loader/runtime feature overrides.
- `codex-rs/app-server/src/transport/remote_control/mod.rs`
  - `start_remote_control_with_options(...)`
    - Boots the remote-control websocket transport with auth, state runtime, and enable/disable wiring.
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - `ThreadHistoryBuilder`
    - Reconstructs visible turn history from rollout-backed items; this is the owner of resumed/read transcript richness, not the TUI renderer.
- `codex-rs/app-server-protocol/src/protocol/common.rs`
  - Owns request/notification enums and RPC method naming.
- `codex-rs/app-server-protocol/src/protocol/v2.rs`
  - Owns active wire payloads.
  - `Thread` and `Turn` payloads are intentionally sparse in many responses/notifications; do not assume full transcript richness everywhere.

#### Auth, sandbox, exec, and persistence

- `codex-rs/login/src/auth/manager.rs`
  - `AuthManager`
    - Canonical runtime auth owner.
  - `shared_from_config(...)`
    - Builds the shared auth manager from effective config.
  - `get_token(...)`
    - Runtime token resolution path.
  - `logout_with_revoke(...)`
    - Logout/revoke path.
  - Refresh helpers in this file also own external/provider-backed bearer refresh behavior.
- `codex-rs/login/src/auth/storage.rs`
  - `AuthDotJson`
    - Durable auth payload shape.
  - `AuthStorageBackend`
    - File/keyring/auto/ephemeral backend choice.
- `codex-rs/login/src/auth/external_bearer.rs`
  - `BearerTokenRefresher`
    - Provider command-backed bearer auth with cached token refresh behavior.
- `codex-rs/login/src/auth/revoke.rs`
  - `revoke_auth_tokens(...)`
    - Best-effort token revocation path behind logout-with-revoke.
- `codex-rs/login/src/server.rs`
  - `run_login_server(...)`
    - Browser callback login server.
  - `exchange_code_for_tokens(...)`
    - Auth-code exchange.
  - `persist_tokens_async(...)`
    - Token persistence after successful login.
  - `ensure_workspace_allowed(...)`
    - Workspace/account gating during login.
- `codex-rs/login/src/pkce.rs`
  - `generate_pkce(...)`
    - PKCE verifier/challenge generation for browser login.
- `codex-rs/login/src/token_data.rs`
  - `parse_chatgpt_jwt_claims(...)` / `parse_jwt_expiration(...)`
    - JWT-derived account/workspace/plan/expiration parsing used by runtime auth decisions.
- `codex-rs/cli/src/login.rs`
  - Operator-facing login/status/logout UX wrapper.
- `codex-rs/models-manager/src/manager.rs`
  - `list_models(...)` / `get_default_model(...)` / `refresh_if_new_etag(...)`
    - Catalog/default-model and refresh ownership for `/models`-backed availability.
- `codex-rs/state/src/runtime/remote_control.rs`
  - `get_remote_control_enrollment(...)` / `upsert_remote_control_enrollment(...)`
    - Persistent remote-control enrollment ownership by websocket target/account/client name.
- `codex-rs/sandboxing/src/manager.rs`
  - `SandboxType`
    - Selected backend enum.
  - `SandboxTransformRequest`
    - Policy/input contract before backend rewrite.
  - `SandboxExecRequest`
    - Executable command/env contract after transformation.
- `codex-rs/core/src/sandboxing/mod.rs`
  - `ExecRequest::from_sandbox_exec_request(...)`
    - Converts the policy-layer result into the exec-layer request actually launched.
  - `execute_env(...)`
    - Adds sandbox environment markers for child execution.
- `codex-rs/core/src/tools/runtimes/shell/unix_escalation.rs`
  - Approval/escalation runtime for shell actions.
  - Owns user prompt routing, optional Guardian review, sandbox transform calls, and denial/result mapping.
- `codex-rs/core/src/guardian/review.rs`
  - `routes_approval_to_guardian(...)`
    - Decides when approval flows should be handled by Guardian rather than only by the operator.
- `codex-rs/linux-sandbox/src/linux_run_main.rs`
  - Linux helper entrypoint.
  - Despite legacy naming, current main defaults to a bubblewrap + seccomp/no_new_privs pipeline, with legacy Landlock fallback.
- `codex-rs/state/src/runtime.rs`
  - `StateRuntime::init(...)`
    - Initializes the SQLite metadata/log DB runtime and related handles.
- `codex-rs/state/src/runtime/agent_jobs.rs`, `memories.rs`, `remote_control.rs`
  - State owners for queued agent jobs, persistent memories, and remote-control/runtime coordination.
- `codex-rs/thread-store/src/store.rs`
  - `ThreadStore`
    - Durable list/read/write contract for thread/session persistence.
- `codex-rs/thread-store/src/local/read_thread.rs`
  - Local read path prefers SQLite metadata when present, then reconstructs from rollout files.
- `codex-rs/model-provider/src/auth.rs`
  - `auth_manager_for_provider(...)`
    - Provider-specific auth override seam.
  - `resolve_provider_auth(...)`
    - Turns provider auth decisions into the auth shape actually used by requests.
- `codex-rs/rmcp-client/src/lib.rs`
  - `perform_oauth_login(...)`, `supports_oauth_login(...)`, `RmcpClient`
    - External MCP OAuth + transport seam used by `codex mcp` and live external MCP connections.
- `codex-rs/install-context/src/lib.rs`
  - `InstallContext::current(...)`, `rg_command(...)`
    - Packaging/resource-lookup seam for standalone/npm/bun/brew installs and bundled helper lookup.

### Open-this-first by task type

- CLI flag, subcommand, or startup behavior
  - `codex-rs/cli/src/main.rs`
  - `codex-rs/tui/src/lib.rs` or `codex-rs/exec/src/lib.rs` depending on path
  - target crate manifest/lib only after dispatch is clear
- Interactive submit or visible runtime bug
  - `codex-rs/tui/src/app.rs`
  - `codex-rs/tui/src/app_server_session.rs`
  - `codex-rs/tui/src/app/app_server_requests.rs` if approvals or request-user-input responses go missing
  - `codex-rs/tui/src/app/loaded_threads.rs` if the problem is loaded-subagent discovery
  - `codex-rs/app-server/src/codex_message_processor.rs`
  - `codex-rs/core/src/thread_manager.rs`
  - `codex-rs/core/src/session/turn.rs`
  - `codex-rs/tui/src/bottom_pane/AGENTS.md` once the bug is isolated below `bottom_pane/**`
- Resume/fork/history bug
  - `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - `codex-rs/app-server/src/codex_message_processor.rs`
  - `codex-rs/tui/src/app.rs`
  - `codex-rs/tui/src/chatwidget.rs`
  - `codex-rs/thread-store/src/local/read_thread.rs` for persistence lookup/source selection
  - `codex-rs/state/src/runtime/threads.rs` only for metadata/index questions
- Tool exposure or tool execution bug
  - `codex-rs/core/src/tools/spec.rs`
  - `codex-rs/core/src/tools/router.rs`
  - `codex-rs/core/src/tools/registry.rs`
  - `codex-rs/core/src/skills.rs` if skill-triggered tool visibility or dependency prompting is involved
  - specific handler under `tools/handlers/`
  - `codex-rs/core/src/tools/orchestrator.rs`
- Prompt/instructions/skills/apps bug
  - `codex-rs/core/src/agents_md.rs`
  - `codex-rs/core/src/skills.rs`
  - `codex-rs/core/src/apps/render.rs`
  - `codex-rs/core/src/session/mod.rs`
  - `codex-rs/core/src/session/turn.rs`
- External MCP/OAuth/server-launch bug
  - `codex-rs/cli/src/mcp_cmd.rs`
  - `codex-rs/rmcp-client/src/lib.rs`
  - `codex-rs/codex-mcp/src/mcp_connection_manager.rs`
  - `codex-rs/core/src/mcp.rs`
  - `codex-rs/core/src/mcp_tool_exposure.rs`
  - `codex-rs/core/src/tools/spec.rs`
- Packaging/install-context/resource-lookup bug
  - `codex-rs/install-context/src/lib.rs`
  - `codex-rs/tui/src/update_action.rs` if the visible symptom is update UX
  - `codex-rs/arg0/src/lib.rs` if helper-path dispatch is involved
- Embedded app-server transport bug
  - `codex-rs/app-server-client/src/lib.rs`
  - `codex-rs/app-server/src/in_process.rs`
  - `codex-rs/app-server/src/message_processor.rs`
- Exec/terminal/runtime process bug
  - `codex-rs/exec/src/lib.rs`
  - `codex-rs/core/src/tools/handlers/unified_exec.rs`
  - `codex-rs/core/src/unified_exec/process_manager.rs`
  - `codex-rs/core/src/exec.rs`
- Sandbox bug
  - `codex-rs/core/src/exec.rs`
  - `codex-rs/core/src/sandboxing/mod.rs`
  - `codex-rs/sandboxing/src/manager.rs`
  - `codex-rs/core/src/tools/runtimes/shell/unix_escalation.rs`
  - `codex-rs/linux-sandbox/src/linux_run_main.rs`
  - `codex-rs/arg0/src/lib.rs`
- Auth/account bug
  - `codex-rs/login/src/auth/manager.rs`
  - `codex-rs/login/src/auth/storage.rs`
  - `codex-rs/cli/src/login.rs`
  - `codex-rs/login/src/server.rs` for browser callback / token exchange bugs
  - `codex-rs/login/src/device_code_auth.rs` for device-code flow bugs
  - `codex-rs/login/src/auth/external_bearer.rs` for provider command-backed auth bugs
  - `codex-rs/login/src/token_data.rs` when account/workspace/plan claims look wrong
  - `codex-rs/login/src/auth/revoke.rs` for logout-with-revoke behavior
  - `codex-rs/model-provider/src/auth.rs` if the behavior differs by provider
  - TUI `/accounts` surface only after the auth owner is locked
- Compact/history reconstruction bug
  - `codex-rs/core/src/tasks/compact.rs`
  - `codex-rs/core/src/compact.rs`
  - `codex-rs/core/src/compact_remote.rs`
  - `codex-rs/core/src/session/mod.rs`
  - `codex-rs/core/src/session/rollout_reconstruction.rs`
  - `codex-rs/app-server-protocol/src/protocol/thread_history.rs` only after the runtime-side history rewrite path is understood
- Config/feature bug
  - `codex-rs/app-server/src/config_api.rs` for config RPC semantics or reload propagation
  - `codex-rs/app-server-protocol/src/protocol/v2.rs` for config wire shapes
  - `codex-rs/core/src/config_loader/mod.rs`
  - `codex-rs/core/src/config/mod.rs`
  - `codex-rs/config/src/lib.rs`
  - `codex-rs/features/src/lib.rs`
- Model/provider/catalog bug
  - `codex-rs/models-manager/src/manager.rs`
  - `codex-rs/model-provider-info/src/lib.rs`
  - `codex-rs/model-provider/src/auth.rs`
  - `codex-rs/codex-api/src/provider.rs`
  - `codex-rs/core/src/client.rs`
- App-server protocol/API bug
  - `codex-rs/app-server-protocol/src/protocol/v2.rs`
  - `codex-rs/app-server-protocol/src/protocol/common.rs`
  - `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
  - `codex-rs/app-server/src/codex_message_processor.rs`
  - client caller in `tui/src/app_server_session.rs` or `exec/src/lib.rs`
- Remote-control/runtime coordination bug
  - `codex-rs/app-server/src/transport/remote_control/mod.rs`
  - `codex-rs/app-server/src/transport/remote_control/protocol.rs`
  - `codex-rs/state/src/runtime/remote_control.rs`
  - `codex-rs/core/src/agent/control.rs` if the symptom is collab/runtime coordination rather than websocket transport
- Multi-agent/collab bug
  - `codex-rs/core/src/tools/handlers/multi_agents.rs`
  - `codex-rs/core/src/agent/control.rs`
  - `codex-rs/core/src/codex_delegate.rs`
  - `codex-rs/core/src/thread_manager.rs`
  - `codex-rs/features/src/lib.rs` only if feature gating matters

### Do-not-guess boundaries

- Do not confuse `codex-cli/` packaging with `codex-rs/cli` runtime ownership.
- Do not treat `codex review` as a separate runtime. It is `codex exec` with review mode.
- Do not jump from `cli` directly to `core` for interactive behavior; current main routes interactive behavior through the TUI and app-server.
- Do not assume the TUI/app-server boundary is cleanly migrated. `tui/src/app/app_server_adapter.rs` is explicitly a hybrid bridge and `tui` still reaches some compatibility surfaces through `legacy_core`.
- Do not treat `codex-rs/app-server-client` as a thin alias. It owns worker-task buffering, event backpressure behavior, and the transitional `legacy_core` namespace that TUI still imports.
- Do not assume the old monolithic `core/src/codex.rs` owner still exists on current main. The runtime is split under `core/src/session/*`.
- Do not treat `CodexThread` as the session owner. It is a wrapper over the real `session/*` runtime.
- Do not treat `codex-tools` as the live runtime owner of a tool. It owns shared tool schemas/registry-plan types; the runtime exposure/handler owner is still `core/src/tools/spec.rs` plus the matching handler/runtime files.
- Do not confuse app-server `TurnStartParams` / `Thread*Response` payloads with core `Op` payloads.
- Do not assume app-server `Thread` / `Turn` payloads are full transcripts. Many APIs intentionally return sparse thread/turn payloads.
- Do not debug bad resumed/read history in `ChatWidget` first. Transcript richness for `thread/read` and `thread/resume` is owned by `app-server-protocol/src/protocol/thread_history.rs` plus the app-server reconstruction path.
- Do not forget `thread/resume` precedence: `history > path > thread_id`.
- Do not assume `thread/list` rows come from one source. They are composed from thread-store summaries, title lookup, and runtime status overlays.
- Do not confuse `codex-rs/config` raw schema/types with `core/src/config/mod.rs` effective runtime config.
- Do not debug config read/write or reload propagation only in `core/src/config_loader/mod.rs`; the app-server RPC owner is `app-server/src/config_api.rs`.
- Do not debug config-layer precedence in `ConfigBuilder` first; layer ordering and trust/project-root rules live in `core/src/config_loader/mod.rs`.
- Do not treat `core/src/agents_md.rs` as just filename constants. It assembles the actual model-visible AGENTS/project-doc stack from codex-home plus project-root -> cwd docs.
- Do not debug prompt-fragment assembly only in `session/turn.rs`; `core/src/session/mod.rs::build_initial_context(...)` assembles AGENTS/apps/skills/plugins/user-instructions before `turn.rs` wraps and samples them.
- Do not debug compaction only in `tasks/compact.rs`; real history rewrite spans `compact.rs`, `compact_remote.rs`, `session/mod.rs`, and `session/rollout_reconstruction.rs`.
- Do not debug login/logout only in `cli/src/login.rs`; browser/device/external-bearer/revoke/token-claim ownership is split across the `codex-login` crate.
- Do not debug `codex mcp login/logout` only in `cli/src/mcp_cmd.rs`; OAuth discovery/login transport and stdio launcher details live in `codex-rmcp-client`, while live server connections live in `codex-mcp`.
- Do not treat `core/src/mcp.rs` as the live MCP runtime owner. It owns configured/effective server projection and tool provenance, while live MCP connection management is initialized during `Session::new(...)`.
- Do not treat model selection as a `core/src/client.rs`-only question; catalog/default selection lives in `codex-models-manager`, while provider capabilities and auth rules live in `codex-model-provider-info` and `codex-model-provider`.
- Do not collapse tool ownership into one file. `tools/router.rs`, `tools/registry.rs`, and `tools/orchestrator.rs` own different parts of the tool path.
- Do not treat `codex-state` SQLite data as the canonical transcript owner. Rollout files remain canonical.
- Do not treat `codex-state` as threads-only. The durable thread/session boundary is `codex-rs/thread-store`, but memories, agent jobs, and remote-control/backfill state still live under `codex-state`.
- Do not treat remote-control enrollment as generic thread metadata. Live websocket transport lives under `app-server/src/transport/remote_control/*`, while persisted enrollment state lives under `state/src/runtime/remote_control.rs`.
- Do not treat `codex-linux-sandbox` as sandbox policy owner. `codex-sandboxing` chooses/rewrites the request first.
- Do not trust legacy naming in the Linux helper. Current main defaults to bubblewrap-first behavior even though some types still say `Landlock`.
- Do not treat `cli/src/mcp_cmd.rs` OAuth as the same auth surface as `codex-login`.
- Do not assume all bearer auth comes from local login storage. Provider-specific external bearer flows can come from `codex-rs/model-provider/src/auth.rs`.
- Do not treat install-context/resource lookup as packaging trivia only. `codex-install-context` can decide bundled helper lookup such as `rg`, and that affects runtime/operator behavior.
- Do not open `tui/src/bottom_pane/**`, protocol schema fixtures, or large snapshot suites before the owner files above.
- Do not enter `tui/src/bottom_pane/**` or `thread-store/src/remote/**` without reading the local `AGENTS.md` once the task is isolated there.
- Do not start in `docs/exec.md` for runtime ownership; it is only an external-doc pointer.
- Do not guess a feature name or stage from docs/comments. Verify in `codex-rs/features/src/lib.rs`.
- Do not add new runtime concepts to `codex-core` if an existing smaller crate or a new crate is the better owner.

### Minimal file-open order for a fresh session

1. `AGENTS.md`
2. `codex-rs/README.md`
3. `codex-rs/cli/src/main.rs`
4. One of:
   - `codex-rs/tui/src/lib.rs` for interactive work
   - `codex-rs/exec/src/lib.rs` for headless work
   - `codex-rs/app-server/src/lib.rs` for app-server work
5. If the path is embedded TUI/exec, `codex-rs/app-server-client/src/lib.rs`
6. `codex-rs/core/src/lib.rs`
7. `codex-rs/core/src/thread_manager.rs`
8. `codex-rs/core/src/session/mod.rs`
9. `codex-rs/core/src/session/session.rs`
10. `codex-rs/core/src/session/turn.rs`
11. `codex-rs/core/src/tools/spec.rs`
12. If the issue is isolated into `tui/src/bottom_pane/**` or `thread-store/src/remote/**`, read that folder's local `AGENTS.md` before widening.
13. Only then widen into handlers, TUI followers, tests, or schema followers.
