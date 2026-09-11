# Cooldex Workspace Rules

## Instruction Scope

- This file and `.sangoi/local/subagents/**` contain only durable Cooldex
  workspace rules and stable navigation. Task-, session-, machine-, review-, and
  history-specific values belong in the current thread, canonical plan, review
  bundle, receipt, or task log rather than in prompt-resident workspace
  instructions.
- Stable owner paths and normative contracts may remain here. Do not record a
  particular plan path or status, branch or Git object, review state, live
  alias/symlink/binary identity, temporary path, validation receipt or hash, or
  last-reviewed timestamp.
- `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md` and
  `/home/lucas/.codex/.base_instructions/sangoi_subagent_instructions.md` own
  shared lead and child behavior. The exact session-selected profile config owns
  its profile and route registry (for an Orch session,
  `/home/lucas/.codex/orch.config.toml`); registered
  `/home/lucas/.codex/agents/*.toml` files own role-specific behavior and verdict
  semantics. Use narrow pointers instead of copying those rule bodies into this
  workspace.

## Local Baseline

- When the operator says `main`, use the local branch named `main`.
- The supported operator path is WSL with ChatGPT Pro authentication through ordinary
  `codex`/`cdx` TUI or exec sessions.
- The supported command topology is `/home/lucas/.cargo/bin/codex` as the reviewed
  regular Cooldex executable, `/home/lucas/.local/bin/cdx-dev` for development sessions,
  and `cdx` selecting `/home/lucas/.codex/packages/standalone/current/bin/codex` through
  `/home/lucas/.cargo/bin/cdx` for the standalone release. The supported topology has no
  interactive Bash `codex` alias. `cdx-pro` is retired and must not be recreated as an
  alias, wrapper, or compatibility path.
- Before installing, promoting, removing, or publishing command surfaces, inspect the
  live resolution, aliases, symlink targets, executable identities, and hashes with
  `type -a`, `alias -p`, `readlink -f`, and `sha256sum` as applicable. This file defines
  supported topology; it does not attest current machine state.
- Current upstream structure and behavior are the baseline. Add only approved fork-owned
  islands and the smallest native seams required to reach them.
- `Merge-safety anchor:` markers are MANDATORY, not optional, on every touched workspace-local divergence file and every touched seam whose behavior, docs, tests, schema, serialization, cache, or operator surface must stay aligned with those customizations. Use the file's native comment syntax (`//`, `///`, `#`, `<!-- -->`, etc.); the required marker text is `Merge-safety anchor:`, not literal `//` everywhere. If a file cannot carry inline comments, add the nearest durable technical note that names the invariant being preserved. Missing merge-safety markers in touched customized or customization-adjacent seams are STOP-SHIP.
- Existing `Merge anchor:` comments are legacy debt. Whenever you touch one of those files for customization-preserving work, normalize it to `Merge-safety anchor:` in the same change.
- Keep `.github/**` and upstream API/wire behavior upstream-owned unless the current user
  explicitly changes that scope.
- Resolve upstream conflicts manually from the canonical owner outward. Do not use
  whole-side conflict selection or preserve obsolete local paths through aliases,
  wrappers, dual reads, or fallback adapters.
- For an admitted upstream sync, the root identifies the common base, local state,
  and selected upstream; distinguishes upstream-only changes from fork divergences
  and affected seams; reconciles changed contracts; updates directly affected
  schemas, fixtures, and validation mapping; then validates bounded batches and
  the integrated candidate. Every batch continues through errors to terminal
  completion before failure investigation or correction. Do not freeze particular refs in durable
  guidance or redesign unrelated upstream code.
- When the current thread explicitly attaches a plan, verify its root branch and Git
  object, separate `.sangoi` branch and Git object, upstream instruction blob, and review
  anchors before mutation. Do not infer plan attachment from workspace files, completed
  plans, task logs, rollout markers, or recency.
- The delimited upstream core below must remain byte-identical to the blob named by its
  opening marker. Local policy belongs outside that core.

<!-- Merge-safety anchor: local regular `codex` promotion preserves the guarded build,
metadata-resolved candidate, atomic replacement without backup or rollback, and exclusions
for companion command, package, host, and Computer Use surfaces. -->
### One-shot local regular `codex` promotion

- This procedure applies only to one-shot WSL/Linux promotion of the regular
  `/home/lucas/.cargo/bin/codex` executable.
- Before building, revalidate the relevant root branch/OID and clean task-owned source
  state. Revalidate live command resolution, aliases, symlink or regular-file identity,
  executable identity, and hashes with `type -a`, `alias -p`, `readlink -f`, and
  `sha256sum` as applicable.
- Build only through root `just build-codex-bin`, which delegates to
  `scripts/cargo-guard.sh`; raw Cargo and `just install` are not promotion routes.
- Resolve the effective Cargo target directory through guarded Cargo metadata in the build
  context. Take the candidate from the metadata-resolved path rather than assuming
  `codex-rs/target/debug/codex`.
- Before replacement, prove that the candidate is a regular executable Linux x86-64 ELF
  with the intended executable mode and SHA-256, then run `--version`, `--help`,
  `exec --help`, and `app-server --help`.
- Stage exactly one temporary sibling beneath `/home/lucas/.cargo/bin`, set its intended
  executable mode, and atomically rename it to replace only
  `/home/lucas/.cargo/bin/codex`; never overwrite that target in place.
- Retain no backup or rollback copy. If post-write proof fails, stop and report the actual
  installed state; never silently restore.
- After replacement, prove candidate and installed SHA-256 equality; regular executable,
  mode, and Linux x86-64 ELF identity; live command resolution and aliases; the same
  bounded smoke results; and absence of the temporary sibling.
- Already-running processes retain their old executable inode; only new processes use the
  replacement.
- `cdx`, `cdx-dev`, standalone packaging, aliases, shell profiles, `codex-code-mode-host`,
  `codex-computer-use-mcp`, and Computer Use tooling are excluded unless direct dependency
  evidence later proves an inseparable follower and the current user expands scope.

## Computer Use Operator Contract

- The canonical local config owner for Computer Use runtime knobs is
  `[features.computer_use]`.
- The supported keys are `mcp_bin`, `sky_bin`, `xvfb`, `openbox`, `temp_root`,
  `display_ready_timeout`, and `shutdown_grace_period`.
- `display_ready_timeout` and `shutdown_grace_period` are millisecond values.
- Path precedence is: matching environment-variable override first, then
  `config.toml`, then the current runtime owner default or absence.
- The supported path override environment variables are
  `CODEX_COMPUTER_USE_MCP_BIN`, `CODEX_COMPUTER_USE_SKY_BIN`,
  `CODEX_COMPUTER_USE_XVFB_BIN`, `CODEX_COMPUTER_USE_OPENBOX_BIN`, and
  `CODEX_COMPUTER_USE_TEMP_ROOT`.
- Missing `mcp_bin` and `sky_bin` leaves the source/development Computer Use MCP
  runtime unavailable. Do not invent a fallback runtime pair.
- `codex-rs/ext/computer-use/AGENTS.md` is the Computer Use extension owner. Its
  live worktree reserves Xvfb starting at `FIRST_DISPLAY = 90` and scans a high
  display range rather than relying on the default allocation path; retain that
  high-range invariant unless current evidence changes it.

## Upstream Defect Policy

- Do not fix an upstream bug or suspected upstream bug in this fork unless direct causal
  evidence shows that it affects the local harness, an approved island or its minimal
  seam, agent runtime performance, or token/context consumption.
- When a failure materially blocks or invalidates the current fork task, classify it as
  upstream-owned only after direct evidence reproduces the same defect on the exact
  current upstream baseline without relying on fork-owned behavior. If upstream does not
  reproduce it, reclassify the failure before further action.
- After upstream reproduction, search the upstream issue tracker. React to an adequate
  existing report, or file a new issue using the current repository template, a redacted
  minimal reproduction, the exact upstream commit or version, environment evidence, and
  the demonstrated impact on the current task. Apply
  `/home/lucas/.codex/skills/external-mutation-safety/SKILL.md` before the external
  mutation.
- The issue or reaction URL is admissible disposition evidence, not proof of causality.
  Filing or reacting does not authorize a local workaround, backport, compatibility path,
  regression-test owner, or scope expansion. Do not wait for an upstream response when
  ownership and the required local disposition are already proven.
- If authentication or external mutation is unavailable, prepare the complete issue body
  and record the exact blocker instead of weakening the ownership proof or inventing a
  local fix.
- Meeting an exception makes a local fix eligible for scoped planning; it does not widen
  the current task automatically.

## Product Architecture

<!-- Merge-safety anchor: full-history V2 usage-hint identity persists a typed birth binding;
do not reconstruct it from mutable configuration, hashes, thread settings, events, or rendered context. -->
- `SessionMeta.agent_usage_hint_binding` is the canonical birth binding for durable
  MultiAgentV2 usage-hint identity. `AgentIdentitySnapshot` and runtime `Config` carry
  it; the runtime field is not a `ConfigToml` key. The PRD owns state, inheritance, and
  legacy-restoration limits.
- `.sangoi/reference/areas/master-refactor-v2-prd-rfc.md` owns planned
  requirements, architecture boundaries, and shipped-status interpretation.

## Repository Boundaries

- `.sangoi/` is a separate Git repository and is intentionally ignored by the root
  repository. Commit, inspect, and publish its artifacts from the inner repository.
- Name both branch/OID pairs whenever a task owns artifacts in both repositories.
- Root commits materially authored by Codex include the exact trailer
  `Co-authored-by: Codex <codex@openai.com>` unless the user explicitly says otherwise.

## Guarded Rust Validation

- On the supported WSL/Linux operator path, route every Cargo build, test, check, lint,
  run, benchmark, and generator command through `./scripts/cargo-guard.sh` or a root
  `just` recipe that delegates to that wrapper. Raw Cargo execution is not valid Cooldex
  workspace evidence.
- If a guarded local WSL build hits the upstream `rusty_v8` prebuilt `404` path, do not
  invent a mirror or a broad local compatibility route. Read the current owners first:
  `third_party/v8/README.md`, `scripts/codex_package/v8.py`, and
  `scripts/cargo-validation.toml`. Those owners define the paired
  `RUSTY_V8_ARCHIVE` plus `RUSTY_V8_SRC_BINDING_PATH` route for package/release/CI
  flows, require an exact `codex-rs/Cargo.lock` version match, and already register the
  guarded host-artifact command `first-party-runtime-support-bins` with
  `codex_v8_target = "host"`. Keep this root note at owner-pointer altitude only.
- `scripts/cargo-validation.toml` owns resource profiles, job caps, one-thread runtime-test
  limits, and receipt placement under `.sangoi/validation/`. Missing guard, policy,
  profile, or receipt ownership fails closed.
- For each coherent semantic batch and changed contract, establish impact coverage
  for canonical owners and direct followers. Use direct owner/follower evidence,
  compiler/test results, and targeted searches when those decide the surface; use
  `scripts/cooldex/rust-blast-radius-guard.py` for unresolved Rust-impact
  questions. When used, preserve its complete uncapped report outside model
  context, consume an owner/follower summary plus unresolved hits, and manually
  account for followers it misses. A silent report is not completeness proof.
- Before completion-class Code Review, materialize the actual review object and complete
  every targeted, finite-closure, and repository-input-provenance validation needed to
  establish that object. Earlier advisory review is non-final.
- For release or publication work, the review object includes the real staged package,
  asset set, checksums, executable identity, metadata, and staged version. Do not request
  completion-class review before those artifacts exist.
- Expensive cumulative suites may remain post-review only when they do not define the
  reviewed object. Apply
  `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md#evidence-efficient-execution` when remediation or
  proof-only corrections may reuse unchanged evidence.
- This root-wide sequencing rule applies to every admitted batch—not only guarded
  validation—including diagnostic, prep, initial, retry, focused, and full runs:
  it must continue through errors and reach terminal completion before failure
  investigation or correction. Failure collection and waiting are allowed; existing
  resource and safety boundaries remain in force. This sequencing rule does not
  change validator implementation or CLI availability.

<!-- Merge-safety anchor: native-Windows bulk validation is planner-accounted and PowerShell-executed; full-mode WSL test preparation uses the config-owned explicit package mapper while Linux production builds remain on the guarded WSL path. -->
### Native-Windows bulk test procedure

- The canonical operator entry point remains WSL: use `./scripts/cargo-guard.sh plan ...`
  to inspect the frozen plan and `./scripts/cargo-guard.sh verify ...` to execute it. Use
  `--changed` to select paths from the current worktree and `--range <base>..<merge>` for
  a merge commit, with `<base>` set to its first parent. `--changed` does not materialize
  arbitrary unstaged or untracked worktree bytes: the native executor consumes the index
  candidate, which requires worktree/index equality and no ordinary untracked source. The
  root owns exact task staging; Workers do not stage. An explicit `--windows-reuse-root
  'F:\.cache\...existing-run-root...'` on either guarded action selects in-place native
  reuse; no selector keeps cold preparation. `--mode full` is the bulk collector. Never run
  Cargo or Nextest directly on native Windows, and never use the former Windows `just test`
  route.
- `scripts/cargo-validation.toml` and `scripts/cargo-validate.py` are the only owners of
  selection, platform classification, exclusions, the frozen manifest, and resource
  contracts. The PowerShell executor runs only manifest-authorized Windows entries; it
  must not infer or invent partitions.
- Native Windows runs the bulk platform-neutral and Linux-relevant test surface.
  Windows-only tests must be explicit exclusions and do not count as coverage.
  Linux/Unix-only tests run only as targeted guarded WSL commands; macOS-only tests are
  not applicable in this topology. WSL runs Linux checks and builds, plus those targeted
  Linux/Unix-only tests.
<!-- Merge-safety anchor: only the native aggregate uses direct dev/test opt1 overrides;
it retains limited symbols, debug assertions, and overflow checks while WSL codegen stays
unchanged. -->
- `commands.windows-nextest-workspace` in `scripts/cargo-validation.toml` applies direct
  Cargo `--config` overrides to both `dev` and `test`: `opt-level=1`,
  `debug="limited"`, `debug-assertions=true`, and `overflow-checks=true`. These settings
  apply only to the native aggregate; WSL commands retain their existing codegen settings.
  The default uses the existing profiles and does not add diagnostic verbosity.
<!-- Merge-safety anchor: codex-voice-host source and workspace membership stay intact,
but the planner must exclude only its validation and visibly retain its unvalidated limit. -->
- `codex-voice-host` remains in the workspace and its source is not removed, but the
  planner excludes it from every package-derived WSL validation rung and the native full
  workspace aggregate. Each applicable plan must warn that it remains unvalidated; this
  is not a claim that voice functionality works.
- In `--mode full`, WSL test-target check/link preparation follows only the explicit WSL
  package mapping in `scripts/cargo-validation.toml`, which remains the list owner.
  Normal per-package Linux checks, strict Clippy, and Linux product builds stay on WSL;
  other modes retain their existing selection policy.
- Build and product output are always Linux/WSL. An ephemeral `codex.exe` is permitted
  only when a platform-neutral test requires it; it must never be installed, promoted,
  published, or operated as the Windows Codex CLI product.
- Cold preparation retains its 120-GiB free-disk and 30-GiB available-RAM requirements.
  An explicit reuse root uses the 5-GiB warm disk floor derived only from
  `[resource_profiles.windows_nextest].reserve_free_gib`; it does not add another
  configurable value or guarantee that every incremental build will fit. Both cold and
  reuse retain 30 GiB of available RAM and the 16-build-job/8-test-thread ceiling. Do not run Cargo or Nextest
  concurrently in WSL and native Windows. A missing prerequisite, tool, manifest,
  candidate identity, space or RAM requirement, or required evidence must fail loud.
- `--yolo` is an explicit per-plan native-Windows-only RAM/disk-floor override for
  `./scripts/cargo-guard.sh plan ... --yolo` or `verify ... --yolo` when the selected
  validation plan contains a native Windows command. It is invalid for prep and direct
  guarded WSL Cargo, records the actual/required values and any bypass in native
  preflight evidence, and does not weaken writer, mutex, bootstrap, candidate, or input
  checks. It can cause paging, out-of-memory, disk-full, or incomplete outputs; it never
  triggers automatic cleanup.
- Every Windows-created mutable path belongs below literal `F:\.cache`: cold preparation
  creates its disposable candidate checkout, target directory, applicable `CARGO_HOME` and
  `RUSTUP_HOME`, `TEMP`/`TMP`, V8/compiler/tool caches, helper staging, and logs/evidence
  there. A selected reuse root retains its existing candidate, target, mutable tool homes,
  and compatible pinned tools in place; each execution still creates fresh evidence and a
  short, hyphen-free `TEMP`/`TMP` root below `F:\.cache`. C: may provide executables and
  toolchains only as read-only inputs; it must not hold a build cache, target directory, or
  temporary state.
- `F:\codex-tools\bin\python3.exe` is a read-only native test-child input. The child
  `PATH` keeps its directory first ahead of WindowsApps and also includes the selected
  native Git `usr\bin` directory that provides `true.exe`; this never changes parent/global
  `PATH` or installs tools. `FORCE_COLOR=0` applies only to the test child. Retain
  `RUST_MIN_STACK=8388608`, and keep `PYTHONPYCACHEPREFIX` below that run's `TEMP`
  directory in `F:\.cache`.
- The supported WSL access path mounts Windows volumes read-only. Native `pwsh.exe` or
  `pwsh` is the technical mechanism for Windows-side writes, not a user prohibition or
  extra permission checkpoint. Maintained Cargo/Nextest execution, candidate
  materialization, and cache cleanup remain owned by checked-in
  `scripts/cargo-validate-windows.ps1` and `scripts/clear-windows-build-cache.ps1`;
  a bounded diagnostic need not be checked in. Do not write directly to `/mnt/f`.
  Installation, destructive actions, and privileged work retain their separate
  authorization boundaries. Fail loud when neither PowerShell 7 command is available.
- Without `--windows-reuse-root`, candidate materialization must be fresh, pristine,
  disposable, and match the frozen manifest and index identity without changing root refs,
  index, or worktree. With an explicit selector, reuse the selected existing native working
  set in place—candidate, target, `CARGO_HOME`/`RUSTUP_HOME`, and compatible pinned
  tools—and synchronize only the changed tracked source needed to match the frozen index;
  do not rewrite unchanged source. A selected root that cannot meet source, index, or tool
  requirements must report the actual blocker: never move or delete its cache, auto-select
  a latest root, or silently replace it with cold preparation. After synchronization,
  tracked-source/index mismatch and ordinary untracked files still fail, while post-test
  ignored outputs may remain; the root source-invariance check remains independent.
  Receipts must bind the candidate, command, platform, executor, and terminal result.
- For Windows space pressure, `scripts/clear-windows-build-cache.ps1` is the only cleanup
  path. Its default is preflight; deletion requires `-Delete`, proof that no Windows or WSL
  writer exists, literal `F:\.cache` as the target, preservation of that root, and JSON
  stdout captured outside the target. Reuse must not trigger automatic cleanup. Deleted
  content is unrecoverable; do not issue a manual partial cleanup command.
- The root-wide sequencing rule applies to every admitted native batch—including
  diagnostic, prep, initial, retry, focused, and full runs: it continues through
  errors to terminal completion before failure investigation or correction. Retry
  reuse matches action, stage, plan, input, and
  validation-tooling identities exactly. When changed input or tooling leaves no matching
  prior evidence, `--only-failed` can execute zero commands and records partial coverage;
  `--resume` refuses partial summaries. Use `--fresh` to start a new full validation and
  emit fresh results; it does not require discarding reusable compiled artifacts.
  Compiled-cache reuse is separate from validation-result reuse. Keep this procedure
  durable: do not add branch or object IDs, session IDs, timestamps, receipt/run paths,
  execution hashes, or machine-state claims.

<!-- cooldex-wsl-release-procedure:begin -->
## WSL Release Procedure

- This procedure applies only when the current user explicitly requests creation or
  publication of a reusable Cooldex release. It does not apply to one-shot local
  installation or promotion of an existing reviewed artifact. Historical release plans
  and task logs do not activate or select this route.
- Build standalone Cooldex releases directly on the supported WSL host for
  `x86_64-unknown-linux-musl`; Docker is not the Cooldex release route.
- Keep mutable Rustup and Cargo state, target installation, Zig bootstrap, build targets,
  temporary files, staged assets, and validation receipts under a release-scoped
  directory in `/home/lucas/.cache/codex/`.
- Reuse outside that directory only verified host proxy executables and immutable or
  content-addressed caches whose identity is bound to the release evidence. When
  isolated Rustup state is required, use the proven `--no-self-update` bootstrap and
  validate the toolchain and musl target before the long build.
- Route every Cargo invocation through `./scripts/cargo-guard.sh`, and construct the
  standalone package through `scripts/build_codex_package.py` and
  `scripts/codex_package/`.
- Use `.github/scripts/install-musl-build-tools.sh` only for its exact dependency
  bootstrap.
- Notify the user immediately before the first command that requires `sudo`, and state the exact dependency-bootstrap purpose.
- Never echo, store, commit, or embed the user password in a command, receipt, plan, log, or repository artifact.
- Build, package, locally validate, and freeze the exact staged release object before
  final Code Review. Verify archive layout, checksums, executable identity, ELF
  properties, package metadata, and staged executable version.
- Final Code Review covers the actual source commit plus staged asset hashes and local
  validation results.
- Apply `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md#evidence-efficient-execution` to proof-only
  corrections. When source and staged-asset hashes are unchanged, rerun the corrected
  mechanical proof against the same object; do not rebuild or reopen semantic review
  unless an accepted claim was explicitly invalidated.
- After review, reread remote branch, tag, release, workflow, and asset preconditions
  immediately before the first remote mutation.
- Immediately after publication, run the public latest-release installer in an isolated
  Rust-free WSL user environment and require the released version.
<!-- cooldex-wsl-release-procedure:end -->

<!-- upstream-agents-core:begin blob=fd0b9ed9e781bdb29bb11a8f62b777ad773fec81 source=upstream/main -->
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
  - A method's sole non-self argument is exempt when the method and parameter names match, such as `.enabled(false)` for `fn enabled(&self, enabled: bool)`.
  - Do not add these comments for string or char literals unless the comment adds real clarity; those literals are intentionally exempt from the lint.
  - The parameter name in the comment must exactly match the callee signature.
  - You can run `just argument-comment-lint` to run the lint check locally. This is powered by Bazel, so running it the first time can be slow if Bazel is not warmed up, though incremental invocations should take <15s. Most of the time, it is best to update the PR and let CI take responsibility for checking this (or run it asynchronously in the background after submitting the PR). Note CI checks all three platforms, which the local run does not.
- When possible, make `match` statements exhaustive and avoid wildcard arms.
- Newly added traits should include doc comments that explain their role and how implementations are expected to use them.
- Discourage both `#[async_trait]` and `#[allow(async_fn_in_trait)]` in Rust traits.
  - Prefer native RPITIT trait methods with explicit `Send` bounds on the returned future, as in `3c7f013f9735` / `#16630`.
  - Preferred trait shape:
    `fn foo(&self, ...) -> impl std::future::Future<Output = T> + Send;`
  - Implementations may still use `async fn foo(&self, ...) -> T` when they satisfy that contract.
  - Do not use `#[allow(async_fn_in_trait)]` as a shortcut around spelling the future contract explicitly.
- When writing tests, prefer comparing the equality of entire objects over fields one by one.
- Do not add tests for values that are statically defined.
- Do not add negative tests for logic that was removed.
- Do not add general product or user-facing documentation to the `docs/` folder. The official Codex documentation lives elsewhere. The exception is app-server API documentation, which is covered by the app-server guidance below.
- Prefer private modules and explicitly exported public crate API.
- If you change `ConfigToml` or nested config types, run `just write-config-schema` to update `codex-rs/core/config.schema.json`.
- When working with MCP tool calls, prefer using `codex-rs/codex-mcp/src/mcp_connection_manager.rs` to handle mutation of tools and tool calls. Aim to minimize the footprint of changes and leverage existing abstractions rather than plumbing code through multiple levels of function calls.
- Do not call `reset_client_session` unnecessarily; let the incremental check logic decide whether to reuse the previous request.
- If you change Rust dependencies (`Cargo.toml` or `Cargo.lock`), run `just bazel-lock-update` from the
  repo root to refresh `MODULE.bazel.lock`, and include that lockfile update in the same change. CI
  verifies lockfile drift.
- Bazel does not automatically make source-tree files available to compile-time Rust file access. If
  you add `include_str!`, `include_bytes!`, `sqlx::migrate!`, or similar build-time file or
  directory reads, update the crate's `BUILD.bazel` (`compile_data`, `build_script_data`, or test
  data) or Bazel may fail even when Cargo passes.
- Do not create small helper methods that are referenced only once.
- For tracing async work, instrument the function or method definition with
  `#[tracing::instrument(...)]` instead of attaching spans to futures with
  `.instrument(...)` at call sites. Before adding instrumentation, check whether the callee—or
  the implementation method it immediately delegates to—is already instrumented.
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
- When running Rust commands (e.g. `just fix` or `just test`) be patient with the command and never try to kill them using the PID. Rust lock can make the execution slow, this is expected.

Run `just fmt` (in the `codex-rs` directory) automatically after you have finished making code changes anywhere in this repository; do not ask for approval to run it. Additionally, run the tests:

1. Do not run `cargo test` directly. Use `just test` so test execution follows the repo defaults.
2. Run the test for the specific project that was changed. For example, if changes were made in `codex-rs/tui`, run `just test -p codex-tui`.
3. Once those pass, if any changes were made in common, core, or protocol, run the complete test suite with `just test`. Avoid `--all-features` for routine local runs because it expands the build matrix and can significantly increase `target/` disk usage; use it only when you specifically need full feature coverage. project-specific or individual tests can be run without asking the user, but do ask the user before running the complete test suite.

Before finalizing a large change to `codex-rs`, run `just fix -p <project>` (in `codex-rs` directory) to fix any linter issues in the code. Prefer scoping with `-p` to avoid slow workspace‑wide Clippy builds; only run `just fix` without `-p` if you changed shared crates. Do not re-run tests after running `fix` or `fmt`.

## The `codex-core` crate

Over time, the `codex-core` crate (defined in `codex-rs/core/`) has become bloated because it is the largest crate, so it is often easier to add something new to `codex-core` rather than refactor out the library code you need so your new code neither takes a dependency on, nor contributes to the size of, `codex-core`.

To that end: **resist adding code to codex-core**!

Particularly when introducing a new concept/feature/API, before adding to `codex-core`, consider whether:

- There is an existing crate other than `codex-core` that is an appropriate place for your new code to live.
- It is time to introduce a new crate to the Cargo workspace for your new functionality. Refactor existing code as necessary to make this happen.

Likewise, when reviewing code, do not hesitate to push back on PRs that would unnecessarily add code to `codex-core`.

## Code Review Rules

### Crate API surface

Keep crate API surfaces as small as possible. Avoid proliferating test-only helpers.

### Model visible context

Codex maintains a context (history of messages) that is sent to the model in inference requests.

1. No history rewrite - the context must be built up incrementally.
2. Avoid frequent changes to context that cause cache misses.
3. No unbounded items - everything injected in the model context must have a bounded size and a hard cap.
4. No items larger than 10K tokens.
5. Highlight new individual items that can cross >1k tokens as P0. These need an additional manual review.
6. All injected fragments must be defined as structs in `core/context` and implement ContextualUserFragment trait

### Breaking changes

Search for breaking changes in external integration surfaces:

- app-server APIs
- raw response item events (`rawResponseItem/*`), even while experimental
- CLI parameters
- configuration loading
- resuming sessions from existing rollouts

### Test authoring guidance

For agent changes prefer integration tests over unit tests. Integration tests are under `core/suite` and use `test_codex` to set up a test instance of codex.

Features that change the agent logic MUST add an integration test:

- Provide a list of major logic changes and user-facing behaviors that need to be tested.

If unit tests are needed, put them in a dedicated test file (\*\_tests.rs).
Avoid test-only functions in the main implementation.

Check whether there are existing helpers to make tests more streamlined and readable.

### Change size guidance (800 lines)

Unless the change is mechanical the total number of changed lines should not exceed 800 lines.
For complex logic changes the size should be under 500 lines.

If the change is larger, explore whether it can be split into reviewable stages and identify the smallest coherent stage to land first.
Base the staging suggestion on the actual diff, dependencies, and affected call sites.

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

### Test module organization

- When adding a new test module, define its contents in a separate sibling file rather than inline in the implementation file.
- Use an explicit `#[path = "..._tests.rs"]` attribute so the test filename is descriptive and easy to locate:

  ```rust
  #[cfg(test)]
  #[path = "parser_tests.rs"]
  mod tests;
  ```

- This applies only when introducing a new test module. Do not move or rewrite existing inline `#[cfg(test)] mod tests { ... }` modules solely to follow this convention.

### Snapshot tests

This repo uses snapshot tests (via `insta`), especially in `codex-rs/tui`, to validate rendered output.

**Requirement:** any change that affects user-visible UI (including adding new UI) must include
corresponding `insta` snapshot coverage (add a new snapshot test if one doesn't exist yet, or
update the existing snapshot). Review and accept snapshot updates as part of the PR so UI impact
is easy to review and future diffs stay visual.

When UI or text output changes intentionally, update the snapshots as follows:

- Run tests to generate any updated snapshots:
  - `just test -p codex-tui`
- Check what’s pending:
  - `cargo insta pending-snapshots -p codex-tui`
- Review changes by reading the generated `*.snap.new` files directly in the repo, or preview a specific file:
  - `cargo insta show -p codex-tui path/to/file.snap.new`
- Only if you intend to accept all new snapshots in this crate, run:
  - `cargo insta accept -p codex-tui`

If you don’t have the tool:

- `cargo install --locked cargo-insta`

### Benchmarks

cargo benchmarks can be run with `just bench`, use the divan crate to write new ones.

Use `just bench-smoke` to dry-run the benchmark for a single iteration to ensure it works.

### Test assertions

- Tests should use pretty_assertions::assert_eq for clearer diffs. Import this at the top of the test module if it isn't already.
- Prefer deep equals comparisons whenever possible. Perform `assert_eq!()` on entire objects, rather than individual fields.
- Avoid mutating process environment in tests; prefer passing environment-derived flags or dependencies from above.

### Spawning workspace binaries in tests (Cargo vs Bazel)

- Prefer `codex_utils_cargo_bin::cargo_bin("...")` over `assert_cmd::Command::cargo_bin(...)` or `escargot` when tests need to spawn first-party binaries.
  - Under Bazel, binaries and resources may live under runfiles; use `codex_utils_cargo_bin::cargo_bin` to resolve absolute paths that remain stable after `chdir`.
- When locating fixture files or test resources under Bazel, avoid `env!("CARGO_MANIFEST_DIR")`. Prefer `codex_utils_cargo_bin::find_resource!` so paths resolve correctly under both Cargo and Bazel runfiles.

### Integration tests

#### codex_core integration testing

- Prefer the utilities in `core_test_support::responses` when writing end-to-end Codex tests.
- Use `TestCodexBuilder::build_with_auto_env()` by default to ensure that new tests work with
  foreign app/exec OSes. See $remote-tests for details.
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

#### app-server integration testing

- Tests should exercise app-server's public JSON-RPC API.
- Use similar server mocking as for core integration tests.
- Use `TestAppServer::builder().build()` and `TestAppServer::send_thread_start_request_with_auto_env()`
  by default to ensure that new tests work with foreign app/exec OSes. See `$remote-tests` for
  details.

## App-server API Development Best Practices

These guidelines apply to app-server protocol work in `codex-rs`, especially:

- `app-server-protocol/src/protocol/common.rs`
- `app-server-protocol/src/protocol/v2.rs`

### Core Rules

- All active API development should happen in app-server v2. Do not add new API surface area to v1.
- Follow payload naming consistently:
  `*Params` for request payloads, `*Response` for responses, and `*Notification` for notifications.
- Expose RPC methods as `<resource>/<method>` and keep `<resource>` singular (for example, `thread/read`, `app/list`).
- Always expose fields as camelCase on the wire with `#[serde(rename_all = "camelCase")]` unless a tagged union or explicit compatibility requirement needs a targeted rename.
- Always expose string enum values as camelCase on the wire with matching serde and TS `rename_all = "camelCase"` annotations unless an explicit compatibility requirement needs targeted renames.
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

- Regenerate schema fixtures when API shapes change:
  `just write-app-server-schema`
  (and `just write-app-server-schema --experimental` when experimental API fixtures are affected).
- Validate with `just test -p codex-app-server-protocol`.
- Avoid boilerplate tests that only assert experimental field markers for individual
  request fields in `common.rs`; rely on schema generation/tests and behavioral coverage instead.

## Python Development Best Practices

### Ignore Python 2 compatibility

This project uses Python 3+. You should not use the `__future__` module.

If you need to worry about feature compatibility between different 3.xx point releases, check the
closest `pyproject.toml`'s `requires-python` field to see what minimum runtime version is supported.

## Platform Support

Tests and features must support Linux, macOS and Windows unless feature is explicitly OS-specific.

Codex supports running connected app-server and exec-server on different operating systems. See the
`$remote-tests` skill for details about integration testing these configurations.
<!-- upstream-agents-core:end -->

## Cooldex Root Atlas

- `/home/lucas/work/codex/AGENTS.md` — durable workspace policy, exact upstream core,
  and stable root owner map.
- `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md` and
  `/home/lucas/.codex/.base_instructions/sangoi_subagent_instructions.md` — shared
  lead and child behavior owners.
- Session-selected profile config — profile route registry and profile-specific
  child-workflow owner; for an Orch session,
  `/home/lucas/.codex/orch.config.toml`.
- `/home/lucas/.codex/agents/` — registered specialist role behavior and verdict
  owners.
- `/home/lucas/work/codex/.sangoi/reference/areas/master-refactor-v2-prd-rfc.md` —
  product requirements, architecture boundaries, and shipped-status interpretation.
- `/home/lucas/work/codex/.sangoi/reference/areas/cooldex-fork-feature-inventory.md` —
  detailed current fork-feature inventory, operator-support layout, and evidence limits.
- `/home/lucas/work/codex/codex-rs/protocol/src/protocol.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/config/mod.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/agent/identity.rs`, and
  `/home/lucas/work/codex/codex-rs/core/src/session/multi_agents.rs` — MultiAgentV2
  usage-hint binding, full-history identity, and contextual-rendering owners.
<!-- Merge-safety anchor: V2 fan-in and list presentation remain bounded to existing
handler owners; the PRD owns their behavior boundary and canonical statuses stay full. -->
- `/home/lucas/work/codex/codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs` and
  `/home/lucas/work/codex/codex-rs/core/src/tools/handlers/multi_agents_v2/list_agents.rs`
  — token-efficient V2 fan-in and body-free list-presentation owners; the
  `master-refactor-v2` PRD / RFC owns their behavior boundary.
- `/home/lucas/work/codex/scripts/cargo-guard.sh`,
  `/home/lucas/work/codex/scripts/cargo-validation.toml`, and
  `/home/lucas/work/codex/scripts/cooldex/rust-blast-radius-guard.py` — guarded Rust
  execution, validation policy, and impact-inventory owners.
- `/home/lucas/work/codex/scripts/cargo-validate-windows.ps1` and
  `/home/lucas/work/codex/scripts/clear-windows-build-cache.ps1` — native-Windows manifest
  executor and exact `F:\.cache` cleanup owner.
- `/home/lucas/work/codex/scripts/install/install.sh` and
  `/home/lucas/work/codex/scripts/build_codex_package.py` and
  `/home/lucas/work/codex/scripts/codex_package/` — release installer and package
  construction owners.
- `/home/lucas/work/codex/codex-rs/ext/computer-use/AGENTS.md` — Computer Use
  extension crate, vendored payload provenance, and future MCP sidecar owner.
- `/home/lucas/work/codex/codex-rs/tui/src/bottom_pane/AGENTS.md` — TUI bottom-pane
  subtree instruction owner.

This Atlas is a stable owner index, not a call graph, task attachment surface, or
shipped-state ledger.
