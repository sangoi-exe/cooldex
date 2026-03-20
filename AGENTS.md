# Workspace Rules

## Custom Sync/Merge Rules (Mandatory)

During any sync/merge with `main` and/or `upstream`, these rules are mandatory:

1. These sync/merge instructions are specific to this workspace and must ALWAYS remain in `AGENTS.md` during future synchronizations.
2. Always resolve conflicts **MANUALLY** every time. Using `ours/theirs` automation is forbidden (including `-X ours`, `-X theirs`, `git checkout --ours`, `git checkout --theirs`, and equivalents).
3. In every conflict, find the best way to preserve the custom functionality we added while also reconciling significant upstream improvements.
4. When upstream and workspace-local code contain almost the same logic, compare both carefully and prefer the structurally cleaner upstream shape when it still supports the required local behavior; port only the necessary local contract deltas instead of blindly restoring the older local copy.
5. When a workspace-local customization keeps colliding with high-churn native files, prefer extracting that customization into a new local module and importing it from the native seam when that reduces future merge friction without adding compatibility shims, alias layers, or ownership confusion.
6. `Merge-safety anchor:` markers are MANDATORY, not optional, on every touched workspace-local divergence file and every touched seam whose behavior, docs, tests, schema, serialization, cache, or operator surface must stay aligned with those customizations. Use the file's native comment syntax (`//`, `///`, `#`, `<!-- -->`, etc.); the required marker text is `Merge-safety anchor:`, not literal `//` everywhere. If a file cannot carry inline comments, add the nearest durable technical note that names the invariant being preserved. Missing merge-safety markers in touched customized or customization-adjacent seams are STOP-SHIP.
7. Existing `Merge anchor:` comments are legacy debt. Whenever you touch one of those files for customization-preserving work, normalize it to `Merge-safety anchor:` in the same change.
8. Remove from the workspace all CI/CD content under `.github` (workflows, actions, and any other pipeline artifacts).

<!-- Merge-safety anchor: AGENTS.md is the canonical source for the workspace-local customization inventory and merge-policy invariants; future sync work must update this section and keep scratchpads as redirects only. -->

## Workspace-local Customization Inventory (Source of Truth)

This section is the canonical cluster-level inventory of durable workspace-local divergence that must survive future syncs/merges with `upstream/main`. Re-derive it from the live diff against `upstream/main` whenever a cluster is added, removed, or materially re-scoped. Scratchpads may point here, but they must not duplicate the inventory body. This is a policy/inventory source of truth, not a claim that every legacy marker has already been normalized everywhere. The listed files are representative high-signal entrypoints, not an exhaustive manifest of every touched file in a cluster.

- `manage_context`: strict retrieve/apply flow, `/sanitize` integration, replacement-history materialization/rollback, and the canonical home-doc stack. Representative files: `codex-rs/core/src/tools/handlers/manage_context.rs`, `codex-rs/core/src/tasks/sanitize.rs`, `codex-rs/core/sanitize_prompt.md`, `~/.codex/manage_context.md`, `~/.codex/manage_context_cheatsheet.md`.
- `recall`: args-less recall, compact/debug output behavior, rollout/compaction coupling, fail-loud boundary handling, and the recall docs/config drift trap. Representative files: `codex-rs/core/src/tools/handlers/recall.rs`, `codex-rs/core/src/codex.rs`, `docs/recall.md`, `docs/guide_reapply_recall.md`, `retrieve`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/config.schema.json`, `codex-rs/core/tests/suite/compact.rs`, `codex-rs/core/tests/suite/compact_remote.rs`.
- `prompt_gc` / `PromptGcSidecar`: automatic active-turn prompt GC, hidden `prompt_gc` tool routing, private runtime activity state, replacement-history persistence/reconstruction, configurable built-in prompt, and the TUI prompt-GC indicator. Representative files: `codex-rs/core/src/prompt_gc_sidecar.rs`, `codex-rs/core/src/tools/handlers/prompt_gc.rs`, `codex-rs/core/prompt_gc_prompt.md`, `codex-rs/core/src/client.rs`, `codex-rs/core/src/codex.rs`, `codex-rs/core/src/rollout/recorder.rs`, `codex-rs/core/src/codex/rollout_reconstruction.rs`, `codex-rs/core/src/tasks/mod.rs`, `codex-rs/core/src/tools/registry.rs`, `codex-rs/tui/src/bottom_pane/mod.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/app.rs`.
- `/accounts`: multi-account ChatGPT management is a cross-file divergence cluster, not a single TUI command. Treat auth storage, TUI popup/cache flow, slash-command gating, app-server account surfaces, and auth docs as one subsystem. Representative files: `codex-rs/core/src/auth.rs`, `codex-rs/core/src/auth/storage.rs`, `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/app_event.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/app-server/README.md`, `docs/authentication.md`, `docs/slash_commands.md`.
- sub-agent/runtime orchestration: custom spawn/profile plumbing, background-agent handling, parallel tool execution, collaboration/thread APIs, and child-agent isolation live outside upstream. Preserve `subagent_instructions_file` as the child-only base-instructions source and keep child spawn/resume config from inheriting lead `developer_instructions`, AGENTS/project-doc-derived `user_instructions`, and `Feature::ChildAgentsMd`. Representative files: `codex-rs/core/src/tools/handlers/multi_agents.rs`, `codex-rs/core/src/tools/parallel.rs`, `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/config/profile.rs`, `codex-rs/core/config.schema.json`, `codex-rs/core/src/project_doc.rs`, `codex-rs/core/src/state/session.rs`, `codex-rs/core/src/rid.rs`, `codex-rs/app-server-protocol/src/protocol/v2.rs`, `codex-rs/app-server-protocol/schema/**`, `codex-rs/app-server/src/codex_message_processor.rs`, `codex-rs/state/src/runtime/threads.rs`, `AGENTS.md`.
- TUI debugging/custom operator surfaces: `/debug`, raw-response inspection, and context-window/operator-facing debugging affordances are local customizations and must stay aligned with runtime event/cache semantics. Representative files: `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/core/src/codex.rs`, `codex-rs/tui/src/app.rs`.
- `mcp-standalone` bridge/runtime customization: session cwd/config-path resolution, operator metadata plumbing, bridge defaults, and local runtime/auth expectations are workspace-local behavior. Representative files: `mcp-standalone/src/app.js`, `mcp-standalone/src/bridge/runtime.js`, `mcp-standalone/src/app-server/client.js`, `mcp-standalone/src/config.js`, `mcp-standalone/src/logger.js`, `mcp-standalone/README.md`.
<!-- Merge-safety anchor: local workspace keeps legacy Landlock enabled from /home/lucas/.codex/config.toml; preserve the propagation and runtime seams listed in the inventory entry below until bubblewrap has a safe writable-gitdir alternative. -->
- `legacy Landlock override`: the local operator config keeps `features.use_legacy_landlock = true` in `/home/lucas/.codex/config.toml` so sandboxed Git metadata writes remain available until upstream ships a safe writable-`gitdir` alternative. Preserve the propagation/honor seams across app-server, core sandbox plumbing, and Linux helper/runtime. Representative files: `/home/lucas/.codex/config.toml`, `codex-rs/app-server/src/codex_message_processor.rs`, `codex-rs/core/src/exec.rs`, `codex-rs/core/src/sandboxing/mod.rs`, `codex-rs/core/src/landlock.rs`, `codex-rs/linux-sandbox/src/linux_run_main.rs`, `codex-rs/cli/src/debug_sandbox.rs`.
- workspace sync policy and local instruction overlays: keep `.github/**` removed from this workspace, preserve the AGENTS-centric sync policy, and keep local config/instruction overlays aligned when those rules change. Representative files: `AGENTS.md`, `/home/lucas/.codex/config.toml`, diff against `upstream/main` for `.github/**`.

## `.sangoi` Repo Boundary

- The workspace `.sangoi/` checkout is its own Git repository and is intentionally ignored by the main workspace repo (`/.sangoi/` in `.gitignore`), so root `git status`/`git diff` do not capture `.sangoi` changes.
- When a task changes `.sangoi/**`, review and commit those changes from the `.sangoi` repo itself.
- Apply the same commit-discipline rule inside `.sangoi`: when a clean split is possible, keep code/config/script changes separate from docs/instructions/logs/reports changes instead of mixing them into one commit.

## Workspace Test Safety

- Do not delegate Cargo validation to sub-agents in this workspace. That includes `cargo check`, `cargo test --no-run`, and `cargo test`.
- Run every Cargo validation rung through `./scripts/cargo-guard.sh` from the workspace root; do not run raw `cargo check`, `cargo test --no-run`, or `cargo test` directly in this workspace.
- Cargo validation precedence is strict: exhaust the lighter/faster checks first, escalate only when they are green, and do not skip ahead to a heavier step when a lighter one can still answer the same question.
- Batch clearly same-class mechanical fallout before escalating to a heavier validation rung; rerun only when the batch is ready or fresh diagnostics are needed.
- `./scripts/cargo-guard.sh` asks Cargo itself for the effective `target_directory`/`build_directory` under the exact wrapper context, enforces the binary 5 GiB free-space floor, and runs `cargo clean` only when the lowest-free-space filesystem across those directories is below that floor before or after a guarded build-like command; failure/interruption alone must not trigger cleanup.
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
  - If you add one of these comments, the parameter name must exactly match the callee signature.
- When possible, make `match` statements exhaustive and avoid wildcard arms.
- When writing tests, prefer comparing the equality of entire objects over fields one by one.
- When making a change that adds or changes an API, ensure that the documentation in the `docs/` folder is up to date if applicable.
- If you change `ConfigToml` or nested config types, run `just write-config-schema` to update `codex-rs/core/config.schema.json`.
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

Run `just fmt` (in `codex-rs` directory) automatically after you have finished making Rust code changes; do not ask for approval to run it.

For Rust validation in `codex-rs`, use this light-first ladder:

0. Before each rung below, invoke it via `./scripts/cargo-guard.sh cargo ...`; the wrapper asks Cargo for the effective `target_directory`/`build_directory`, checks the 5 GiB floor across those directories, and runs `cargo clean` only when low free space violates that guardrail.
1. Run the relevant quick/light Cargo checks first, with `./scripts/cargo-guard.sh cargo check -p <project>` as the default starting point.
2. Escalate to `./scripts/cargo-guard.sh cargo check -p <project> --tests` only when test targets, fixtures, macros, or integration surfaces are part of the touched scope.
3. Only if the relevant `cargo check` rung(s) are green, run `./scripts/cargo-guard.sh cargo test -p <project> --no-run`.
4. Only if `--no-run` is green, run `./scripts/cargo-guard.sh cargo test -p <project>` when runtime/behavior validation is actually needed.
5. Ask the user before running a complete suite such as workspace-wide `cargo test` / `just test`.
6. When warnings must be blocking for the selected target set, run `just clippy-strict ...` after the compile ladder. Add `--tests` only when test targets are intentionally in scope. If the deliverable is a shipped binary or another top-level target whose local dependencies must also be warning-clean under plain rustc, also run `just check-strict ...` on that same exact surface (for example `just check-strict -p codex-cli --bin codex`).

Before finalizing a large change to `codex-rs`, run `just fix -p <project>` (from the workspace root or inside `codex-rs`; the recipe routes through `./scripts/cargo-guard.sh`) to fix any linter issues in the code. Prefer scoping with `-p` to avoid slow workspace‑wide Clippy builds; only run `just fix` without `-p` if you changed shared crates. Do not re-run tests after running `fix` or `fmt`.

## TUI style conventions

See `codex-rs/tui/styles.md`.

## TUI code conventions

- When a change lands in `codex-rs/tui` and `codex-rs/tui_app_server` has a parallel implementation of the same behavior, reflect the change in `codex-rs/tui_app_server` too unless there is a documented reason not to.

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
