# Cooldex Workspace Rules

## Instruction Scope

- This file contains fork-local durable Cooldex workspace rules and stable navigation, not an upstream instruction mirror. `.sangoi/local/subagents/**` also contains only durable workspace guidance. Task-, session-, machine-, review-, and history-specific values belong in the current thread, canonical plan, review bundle, receipt, or task log rather than in prompt-resident workspace instructions.
- Stable owner paths and normative contracts may remain here. Do not record a
  particular plan path or status, branch or Git object, review state, live
  alias/symlink/binary identity, temporary path, validation receipt or hash, or
  last-reviewed timestamp.
- `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md` and `/home/lucas/.codex/.base_instructions/sangoi_subagent_instructions.md` own shared lead and child behavior. `/home/lucas/.codex/config.toml` is the canonical default resident runtime config and references `/home/lucas/.codex/.profile_instructions/sangoi_orchestrator_instructions.md`, which owns the Orchestrator workflow, route registry, and resident harness contract; registered `/home/lucas/.codex/agents/*.toml` files own role-specific behavior and verdict semantics. Use narrow pointers instead of copying those rule bodies into this workspace.

## Local Baseline

- When the operator says `main`, use the local branch named `main`.
- The supported operator path is WSL with ChatGPT Pro authentication through ordinary
  `codex`/`cdx` TUI or exec sessions.
- `/home/lucas/.cargo/bin/codex` and `/home/lucas/.local/bin/cdx-dev` are stable selectors for independent local package lanes. Each lane owns its selected complete package, source-built helpers and sidecars, daemon namespace, and pinned subordinate managed daemon package; user authentication, sessions, general configuration, SQLite, and history remain shared under one `CODEX_HOME`.
- `cdx` selects `/home/lucas/.codex/packages/standalone/current/bin/codex` through `/home/lucas/.cargo/bin/cdx` for the standalone release. It is a separate lane and is excluded from the local source-promotion procedure. The supported topology has no interactive Bash `codex` alias. `cdx-pro` is retired and must not be recreated as an alias, wrapper, or compatibility path.
- Before installing, promoting, removing, or publishing command surfaces, inspect the
  live resolution, aliases, symlink targets, executable identities, and hashes with
  `type -a`, `alias -p`, `readlink -f`, and `sha256sum` as applicable. This file defines
  supported topology; it does not attest current machine state.
- Current upstream structure and behavior are the baseline. Add only approved fork-owned
  islands and the smallest native seams required to reach them.
- `Merge-safety anchor:` markers are mandatory only at real upstream/base conflict seams. By default, all of the following must be true: the path is tracked by this root Git repository, exists in the selected upstream/base tree, carries a fork-local divergence, and contains a concrete invariant or seam that an upstream edit could overwrite or make ambiguous during conflict resolution. Use the file's native comment syntax (`//`, `///`, `#`, `<!-- -->`, etc.); the required marker text is `Merge-safety anchor:`, not literal `//` everywhere. If a file cannot carry inline comments, add the nearest durable technical note that names the invariant being preserved. Missing required anchors are STOP-SHIP.
- Do not add an anchor merely because a file was touched. New or fork-only files, generated or mechanical artifacts, lockfiles, snapshots, receipts, plans, task logs, caches, root-ignored paths, and separate repositories including `.sangoi/**` need no anchor by default. Do not remove existing anchors solely because they fall outside this default.
- Existing `Merge anchor:` comments are legacy debt. Normalize one to `Merge-safety anchor:` only when its eligible seam is touched for customization-preserving work.
- Keep `.github/**` and upstream API/wire behavior upstream-owned unless the current user
  explicitly changes that scope.
- Resolve semantic source conflicts manually from the canonical owner outward. Do not use whole-side selection for semantic source or preserve obsolete local paths through aliases, wrappers, dual reads, or fallback adapters. Generated snapshots follow the provisional checkpoint rule below.
- For an admitted upstream sync, the root identifies the common base, local state,
  and selected upstream; distinguishes upstream-only changes from fork divergences
  and affected seams; reconciles changed contracts; updates directly affected
  schemas, fixtures, and validation mapping; then validates bounded batches and
  the integrated candidate. Every batch continues through errors to terminal
  completion before failure investigation or correction. Do not freeze particular refs in durable
  guidance or redesign unrelated upstream code.

### Durable branch-promotion and upstream-sync route

<!-- Merge-safety anchor: admitted upstream syncs resolve the exact stable upstream Rust release and synchronize the canonical workspace version with its mechanical followers. -->
<!-- Merge-safety anchor: each semantic conflict retains profile-registered Luna Recon evidence and direct-follower propagation rather than treating a textual merge as proof that the integrated contract survives. -->
<!-- Merge-safety anchor: post-merge tests use clean committed merge and correction candidates; pre-test checkpoints do not authorize publication. -->
- Fast-forward local `master` and `origin/master` from a reviewed `dev` candidate;
  do not rewrite either history.
- Fetch `upstream/main`, fast-forward local `main`, then mirror that exact tip to
  `origin/main` while retaining local `main`'s `upstream/main` tracking relationship.
- Merge the updated `main` into `dev` with a normal two-parent merge that preserves
  Cooldex as the first-parent lineage.
- Before resolving each semantic conflict, obtain read-only Recon through the selected profile's registered Luna lane. Compare the common base, fork, and selected upstream to identify the changed contract and map canonical producers and consumers, callers, causally related automatically merged code, direct and integration tests, helpers, fixtures, snapshots, and generators. Inspect test expectations against the intended integrated contract rather than merely matching symbols or filenames. Related textual hunks may share one Recon, but account for every semantic conflict and state reviewed, unreviewed, and blocked related surfaces; incomplete evidence is not complete coverage. Recon supplies evidence and does not choose material behavior.
- Preserve admitted Cooldex behavior by adapting fork-owned islands to current upstream canonical owners, contracts, and architecture; do not retain obsolete local structure through aliases, wrappers, dual reads, fallbacks, or whole-side semantic source selection.
- Carry affected tests and other direct followers, including stale expectations in automatically merged tests, into the same coherent resolution and propagation batch. Complete the selected resident profile's validation and review route before publishing the final `dev` candidate to `origin/dev`; that profile retains task classification, planning, Worker, Gate, lifecycle, and completion ownership.
- Once source and manifests reconcile, use the existing guarded `prep-plan` and `plan` actions against the complete merge selection before expensive preparation and validation to expose routing or mapping gaps. `scripts/cargo-validation.toml` and `scripts/cargo-validate.py` remain the selection owners; close a demonstrated mapping class coherently without changing full-versus-affected runtime behavior.
- After resolving semantic source conflicts and every unmerged Git index entry and completing necessary existing non-test preparation, the root commits the normal two-parent merge and verifies that `MERGE_HEAD` is absent before test execution. Generated snapshots need only the provisional Git resolution below at this checkpoint, not final regeneration or test execution. Test the committed source with a clean index and worktree, not an open merge or staged-only candidate. This pre-test commit is an unvalidated checkpoint, not approval or authority to push.
- Resolve generated snapshot Git conflicts provisionally with the selected upstream snapshots to close the merge; those bytes are unaccepted expectations, not semantic source selection or correctness proof. After the committed checkpoint, regenerate snapshots through their existing native harness, semantically review the results, and commit accepted snapshots before final validation and publication. Follow the committed-source testing, initial complete collection, terminal-batch-before-diagnosis/correction, and clean committed correction/revalidation rules; provisional selection does not waive them. This distinction applies only to generated snapshots, not schemas or other generated artifacts.
- For an admitted Cooldex upstream sync, run one initial complete validation collection for the integrated committed merge selection. After that collection reaches terminal completion, attribute failures under Guarded Rust Validation before choosing corrections. Start each correction batch from a clean committed source; edits may temporarily dirty the worktree, but the root commits the completed coherent batch and verifies clean index/worktree equality with that commit before revalidation. Use the supported focused selector derived from the corrected files or correction range, preserving affected package and contract coverage. Repeat the complete collector only when changed inputs, tooling, contracts, or evidence invalidate the prior collection.
- Diagnose materialization, compilation, and runtime-test failures at their own owners after the current batch reaches terminal completion. Git pack or checksum failures require source-object integrity diagnosis rather than repeated test execution. Bazel external-materialization failures require inspection of output, repository, and content-cache ownership; `.bazelrc` and `.github/scripts/run_bazel_with_buildbuddy.py` are live owner anchors, not a recovery recipe. Do not authorize automatic repair, cleanup, bypass, or new wrappers.
- Keep required full staged diff checks. Attribute whitespace to local-resolution bytes or the relevant upstream blob before correcting it; do not normalize unrelated imported snapshots or licenses merely to silence checks. Resolve generating source first, then regenerate derived conflicts through existing owners, with generated snapshots following the post-checkpoint rule above; do not substitute task-only checks for mandatory commit checks.
- For every admitted upstream sync, resolve the highest exact non-prerelease upstream tag matching `rust-vMAJOR.MINOR.PATCH`, set `[workspace.package].version` to that version, update Cargo-generated or mechanical followers through guarded repository routes, and validate the resulting build and version identity.
- Commit and publish task-owned sync documentation from the separate `.sangoi` repository,
  excluding unrelated dirty inner-repository work.

- When the current thread explicitly attaches a plan, verify its root branch and Git object, separate `.sangoi` branch and Git object, selected upstream instruction sources or their evidenced removal, the plan's instruction disposition, and review anchors before mutation. Do not infer plan attachment from workspace files, completed plans, task logs, rollout markers, or recency.

<!-- Merge-safety anchor: local source promotion selects one complete package generation for one explicit stable local lane, preserving package-local helpers and the lane-bound daemon lifecycle without runtime Git-branch awareness. -->
### Branch-scoped local complete-package promotion

- This procedure applies only to one WSL/Linux local source-promotion invocation for one explicit destination: `codex` or `cdx-dev`. `cdx`, Windows/macOS product promotion, release publication, and remote mutation remain separate and excluded.
- Before building, verify one clean source checkout by its explicit branch and immutable HEAD, plus one explicit destination. Reject ambiguity, never infer the destination from the branch name, and never make runtime code inspect Git branches.
- Before any operation, record the live command, alias, symlink, package, and executable identities and pre-operation hashes for both the selected and excluded destinations with `type -a`, `alias -p`, `readlink -f`, and `sha256sum` as applicable.
- Rebuild every applicable source-owned package input only through root `just build-local-codex-package-inputs`, which delegates guarded Cargo work. Raw Cargo, prior-generation compiled inputs, partial helper or sidecar replacement, and cross-lane reuse are not promotion routes.
- Assemble exactly one complete package from those exact guarded inputs through `scripts/build_codex_package.py` and `scripts/codex_package/`; do not assemble, select, or reuse a second lane package in the invocation.
- Before selection, validate the package manifest and layout; CLI, Code Mode host, bwrap, Computer Use MCP and Sky, rg, and applicable zsh resources; target, modes, and hashes; and bounded CLI `--version`, `--help`, `exec --help`, and `app-server --help` smoke behavior.
- Stage the complete package only beneath the selected lane package root, then atomically select that lane's `current` generation and stable command selector. Never overwrite an executable, helper, sidecar, package, or selector in place.
- The existing app-server-daemon owner may create or select only the selected lane's validated, pinned subordinate managed copy. That copy derives from the selected complete package; it is neither another promotion nor an independent update authority.
- Observe the excluded lane only for required read-only identity and non-mutation proof. Build, select, and mutate only the selected lane; do not consume or reuse excluded-lane package, selector, helper, sidecar, daemon, socket, state, or generation-bound resources.
- Retain no backup, rollback copy, pruning, cleanup, or partial temporary staging directory or sibling. If a proof fails, stop and report the actual selected state without silently restoring or altering either lane.
- After selection, prove selected command resolution; package and resource identity; lane identity; daemon socket, state, and managed selection; pinned status; daemon-backed resume; Computer Use availability; selected candidate-to-package hash linkage; excluded-lane non-mutation; and temporary staging absence.
- Existing processes retain their old generation; only new processes use the selected generation.

## Computer Use Operator Contract

- The canonical local config owner for Computer Use runtime knobs is `[features.computer_use]`.
- The supported keys are `mcp_bin`, `sky_bin`, `xvfb`, `openbox`, `temp_root`, `display_ready_timeout`, and `shutdown_grace_period`; `display_ready_timeout` and `shutdown_grace_period` are millisecond values.
- MCP/Sky resolution requires a complete pair and uses this exact precedence: a complete explicit process override pair; otherwise a complete package-local pair for a recognized local package; otherwise a complete shared `config.toml` pair only for unbundled, source, or legacy execution. A missing or one-sided package pair fails loud without mixing sources or falling back to shared config.
- Xvfb, Openbox, and temporary-root paths use their matching environment override when supplied, then the shared configuration; the timeout values remain configuration-owned.
- The supported path override environment variables are `CODEX_COMPUTER_USE_MCP_BIN`, `CODEX_COMPUTER_USE_SKY_BIN`, `CODEX_COMPUTER_USE_XVFB_BIN`, `CODEX_COMPUTER_USE_OPENBOX_BIN`, and `CODEX_COMPUTER_USE_TEMP_ROOT`.
- Missing `mcp_bin` and `sky_bin` leaves unbundled/source/legacy Computer Use unavailable. Do not invent a fallback runtime pair.
- `codex-rs/ext/computer-use/AGENTS.md` is the Computer Use extension owner. Its live worktree reserves Xvfb starting at `FIRST_DISPLAY = 90` and scans a high display range rather than relying on the default allocation path; retain that high-range invariant unless current evidence changes it.

## Upstream Defect Policy

<!-- Merge-safety anchor: confirmed upstream reports require environment-aware independent reproduction; fork-related repair stays at current upstream owners and material decisions remain user-owned. -->
- Do not fix an upstream bug or suspected upstream bug in this fork unless direct causal evidence shows that it affects the local harness, an approved island or its minimal seam, agent runtime performance, or token/context consumption.
- Classify any failure as a confirmed isolated upstream bug only after reproducing it independently of fork-owned behavior on the exact selected upstream baseline, establishing that expected behavior is violated, and ruling out relevant local setup effects. Reproduction in the same faulty environment is not sufficient. If upstream does not reproduce it, reassess the attribution before further action; an uncertain defect remains uncertain and is not reported as confirmed.
- For a confirmed isolated upstream bug, search the upstream issue tracker. React to an adequate existing report, or file a new issue using the current repository template, a redacted minimal reproduction, the exact upstream commit or version, environment evidence, and the demonstrated impact on the current task. Explicitly disclose that an agent is executing a task in a fork. The current user gives standing authorization to use authenticated `gh` for this bounded reporting disposition without new per-case permission; it does not authorize speculative reports or other remote actions. Apply `/home/lucas/.codex/skills/external-mutation-safety/SKILL.md` before the external mutation.
- The issue or reaction URL is admissible disposition evidence, not proof of causality. Filing or reacting does not authorize a local workaround, backport, compatibility path, regression-test owner, dependency change, or Scope expansion. Do not wait for an upstream response when ownership and the required local disposition are already proven.
- If authentication or reporting transport is unavailable, prepare the complete issue body and record the exact blocker instead of weakening the ownership proof or inventing a local fix. Resolve an ambiguous write outcome by readback before any retry; never repeat an ambiguous write blindly.
- For a fork-related failure, explain the causal fork change and assess optimization, improvement, simplification, or adaptation of the admitted feature to current upstream contracts and canonical owners before considering or proposing a patch or workaround. The current user retains material architecture, Scope, and remediation decisions. Do not preserve obsolete fork structure through fallbacks, aliases, wrappers, or dual reads.
- Meeting an exception makes a local fix eligible for scoped planning; it does not widen the current task automatically.

## Product Architecture

<!-- Merge-safety anchor: cache locking and initialization must not hand off the Tokio scheduler core; creator-thread retirement can deliver Linux PDEATHSIG to its subprocesses while the runtime remains alive. -->
- `codex-rs/utils/cache/src/lib.rs` owns `BlockingLruCache`. Preserve direct standard-library mutex locking and synchronous `OnceLock` initialization outside the global cache lock, shared initialization for a resident cell, and deterministic replacement after eviction. Do not reintroduce `block_in_place` or another cache-origin scheduler handoff, and do not remove `PDEATHSIG` to mask creator-thread retirement.

<!-- Merge-safety anchor: full-history V2 usage-hint identity persists a typed birth binding;
do not reconstruct it from mutable configuration, hashes, thread settings, events, or rendered context. -->
- `SessionMeta.agent_usage_hint_binding` is the canonical birth binding for durable
  MultiAgentV2 usage-hint identity. `AgentIdentitySnapshot` and runtime `Config` carry
  it; the runtime field is not a `ConfigToml` key. Its state, inheritance, and
  legacy-restoration limits are owned by `codex-rs/protocol/src/protocol.rs`,
  `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/agent/identity.rs`, and
  `codex-rs/core/src/session/multi_agents.rs`.

## Voice Support Boundary

<!-- Merge-safety anchor: Cooldex intentionally leaves the upstream voice-host surface
unsupported on every route; preserve its source only for synchronization until an explicit
current-user rescope. -->
- Cooldex voice support and `codex-voice-host` are intentionally unsupported on every
  operator and validation route. Do not build, test, validate, package, install, promote,
  or otherwise enable `codex-voice-host`, and do not install GLib, GStreamer, or another
  voice dependency for Cooldex work.
- For a supported complete-workspace test, use `just test --workspace --exclude codex-voice-host`; never use bare `just test`, which includes the voice-host package.
- A failure confined to `codex-voice-host` or a dependency exclusive to it is Scope-excluded
  and nonblocking. Disclose the exclusion instead of repairing the failure or satisfying its
  dependency.
- Retain voice source and workspace membership only as an upstream synchronization surface;
  do not modify or remove either to circumvent this boundary. Future voice support requires a
  new explicit current-user rescope.

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
- If a guarded local WSL build hits the upstream `rusty_v8` prebuilt `404` path, do not invent a mirror or broad local compatibility route. Read `third_party/v8/README.md`, `scripts/codex_package/v8.py`, `scripts/cargo-validation.toml`, and `scripts/cargo-validate.py`: they define the paired `RUSTY_V8_ARCHIVE` plus `RUSTY_V8_SRC_BINDING_PATH` route for package/release/CI flows, require an exact `codex-rs/Cargo.lock` version match, and apply `codex_v8_target = "host"` to guarded V8-consumer commands. Keep this root note at owner-pointer altitude only.
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
<!-- Merge-safety anchor: post-merge failure attribution separates real executor/environment causes from upstream-versus-fork ownership before correction. -->
- After the current collection reaches terminal completion, every post-merge test failure requires environment and causal attribution before correction. Identify the actual Windows or WSL platform/executor, committed candidate, and causally relevant inputs, configuration, dependencies, and resources; investigate local-environment causes separately from upstream-versus-fork attribution. A failed run, baseline equality, reproduction upstream in the same bad environment, or a WSL pass alone does not establish a code bug or its owner. Use Upstream Defect Policy for confirmed upstream reporting and fork-related repair assessment; this requires relevant evidence, not an exhaustive new environment inventory or validator.
- This root-wide sequencing rule applies to every admitted batch—not only guarded
  validation—including diagnostic, prep, initial, retry, focused, and full runs:
  it must continue through errors and reach terminal completion before failure
  investigation or correction. Failure collection and waiting are allowed; existing
  resource and safety boundaries remain in force. This sequencing rule does not
  change validator implementation or CLI availability.

### Local snapshot commands

- From the repository root, use `./scripts/cargo-guard.sh cargo insta pending-snapshots --manifest-path codex-rs/tui/Cargo.toml` to list pending TUI snapshots and `./scripts/cargo-guard.sh cargo insta show --manifest-path codex-rs/tui/Cargo.toml codex-rs/tui/path/to/file.snap.new` to preview a specific snapshot. Replace the placeholder with the real repository-relative snapshot path. Do not use `-p codex-tui` with these cargo-insta subcommands.
- Only when intentionally accepting all pending snapshots in the TUI crate, run `./scripts/cargo-guard.sh cargo insta accept --manifest-path codex-rs/tui/Cargo.toml`. Snapshot generation still uses the supported guarded test route.
- Whenever `scripts/cargo-guard.sh` changes, check current command interfaces with non-mutating help or plan commands and reconcile affected command examples and documentation, including this file and `scripts/AGENTS.md`; passing script tests alone does not establish that documented commands remain usable.

<!-- Merge-safety anchor: native-Windows bulk validation is planner-accounted and PowerShell-executed; matching focused WSL passes through the default guarded route or the constrained original-binary comparison may settle failed or ambiguous portable-test acceptance without changing native outcomes, while normal WSL preparation uses the config-owned package mapper and Linux production builds remain on the guarded WSL path. -->
### Native-Windows bulk test procedure

- The canonical operator entry point remains WSL: use `./scripts/cargo-guard.sh plan ...` to inspect the frozen plan and `./scripts/cargo-guard.sh verify ...` to execute it. Use `--changed` to select all current staged, unstaged, and ordinary untracked paths, not a delta since a prior full collection; use `--range <first-parent>..<merge>` to select a clean committed merge, because `--changed` does not select its committed changes. For a narrower post-collection correction batch, use `--file <corrected-path>` or a range covering the committed correction batch. `--changed` does not materialize arbitrary unstaged or untracked worktree bytes: the native executor consumes the index candidate, which requires worktree/index equality and no ordinary untracked source. For post-merge tests, the index and worktree must also equal the committed candidate under the clean-candidate rule above; the native index executor is unchanged. The root owns exact task staging and commits; Workers do neither. Every native validation automatically uses the canonical reusable workset at literal `F:\.cache\cw\workset`. Standard and strict runtime selections send their eligible selected packages plus evidenced native binary prerequisites to native Nextest while its test filter runs only affected package tests; non-runtime selections do not acquire a native test run. `--mode full` remains the complete bulk collector; admitted upstream syncs follow the initial-complete-then-focused-correction cycle in the durable upstream-sync route above. Never run Cargo or Nextest directly on native Windows, and never use the former Windows `just test` route.
- `scripts/cargo-validation.toml` and `scripts/cargo-validate.py` are the only owners of
  selection, platform classification, exclusions, the frozen manifest, and resource
  contracts. The PowerShell executor runs only manifest-authorized Windows entries; it
  must not infer or invent partitions.
- Native Windows is the default broad executor for platform-neutral tests, including heavy complete portable-test suites, for convenience and crash avoidance rather than as an assumed correctness oracle. Investigate Windows/local setup effects separately from code causes under Guarded Rust Validation. Windows-only tests remain explicit exclusions and do not count as coverage, Linux/Unix-only tests run as targeted guarded WSL commands, and macOS-only tests are not applicable in this topology. WSL runs Linux checks and builds, targeted Linux/Unix-only tests, and the focused portable-test comparisons below.
- After the native batch reaches terminal completion, a platform-neutral test with a failed or ambiguous Windows result may be checked individually on WSL, without renewed permission or a new case-specific Windows-result waiver. The default route remains `./scripts/cargo-guard.sh` or a current repository `just` recipe that delegates to it. Keep these comparisons small and focused, with exactly one build job and one runtime-test thread: use explicit Cargo `-j 1` and `-- --test-threads=1`, or Nextest `--build-jobs 1 --test-threads 1`, through the guard; a just recipe must preserve both limits. Only when demonstrated executor interference invalidates the intended original test environment, compile the original tests through the guard with one build job on the same source candidate as Windows, resolve the original test binary produced by that compilation, and execute only the exact original cases directly with one test thread, unchanged assertions, and the intended environment restored. Both routes retain applicable existing resource limits and the existing shared mutex/workflow exclusion against concurrent WSL or native Cargo/Nextest execution. Record candidate, binary provenance, commands, environment, executor-interference evidence when applicable, and terminal results truthfully. A WSL execution that actually runs and passes the matching test on the same current-source candidate as Windows, preserving the original test behavior, overrides that Windows acceptance blocker. Preserve the original Windows failure, timeout, or ambiguity and the matching WSL result truthfully; never relabel the native result as a pass. A WSL failure or skip, a different case or source candidate, or changed test behavior does not qualify. This narrow original-binary comparison does not authorize raw Cargo, direct native Cargo/Nextest, or a general test-runtime bypass, and changes neither normal planner selection nor cache cleanup policy. This is test-result acceptance only, not a Code Gate override or a runtime compatibility fallback; a WSL pass alone does not prove upstream ownership or a Windows/environment defect.
<!-- Merge-safety anchor: native Nextest uses direct dev/test opt1 overrides; it retains
limited symbols, debug assertions, and overflow checks while WSL codegen stays unchanged. -->
- `commands.windows-nextest-workspace` in `scripts/cargo-validation.toml` is the canonical native Nextest argv: it applies direct Cargo `--config` overrides to both `dev` and `test` (`opt-level=1`, `debug="limited"`, `debug-assertions=true`, and `overflow-checks=true`), and selected package commands derive their common policy, platform exclusions, features, and test filter from it; these settings apply only to native Nextest, WSL commands retain their existing codegen settings, and the default uses existing profiles without diagnostic verbosity.
<!-- Merge-safety anchor: codex-voice-host source and workspace membership stay intact,
but the planner must exclude only its validation and visibly retain its unvalidated limit. -->
- `codex-voice-host` remains in the workspace and its source is not removed, but the planner excludes it from every package-derived WSL validation rung, native selected-package test, and native full workspace aggregate; each applicable plan must warn that it remains unvalidated, which is not a claim that voice functionality works.
- Normal planner-driven WSL runtime and test-target check/link preparation follow the explicit WSL package mapping in `scripts/cargo-validation.toml`, which remains the list owner; normal per-package Linux checks, strict Clippy, and Linux product builds stay on WSL, quick omits runtime execution, standard and strict use native selected-package tests, and full retains the explicit workspace collector. The focused portable-test comparisons above do not change that selection or list ownership.
- Build and product output are always Linux/WSL. An ephemeral `codex.exe` is permitted
  only when a platform-neutral test requires it; it must never be installed, promoted,
  published, or operated as the Windows Codex CLI product.
- Cold first use is allowed only when the operator has cleared the whole literal `F:\.cache`; it retains the 120-GiB free-disk floor. A valid canonical workset reuses automatically with the 5-GiB warm free-disk floor derived only from `[resource_profiles.windows_nextest].reserve_free_gib`. Both modes retain the 16-build-job/8-test-thread ceiling; available RAM is observed as telemetry, not an admission floor. Do not run Cargo or Nextest concurrently in WSL and native Windows. A missing prerequisite, tool, manifest, candidate identity, space requirement, or required evidence must fail loud.
- `--yolo` is an explicit per-plan native-Windows-only disk-floor override for `./scripts/cargo-guard.sh plan ... --yolo` or `verify ... --yolo` when the selected validation plan contains a native Windows command. It is invalid for prep and direct guarded WSL Cargo, records the actual/required disk values and any disk bypass in native preflight evidence, and bypasses only the initial native Windows disk floors. It does not disable the runtime disk abort, weaken writer, mutex, bootstrap, candidate, or input checks, or trigger cleanup.
- A rejected cold admission may retain only fresh non-reparse run evidence. It may retry without another cleanup only when no workset or other persistent cache state exists. The guarded workflow admission must prevent concurrent Cargo or Nextest execution across WSL and native Windows.
- Every Windows-created mutable path belongs below literal `F:\.cache`. The canonical workset retains the candidate, target, `CARGO_HOME`, `RUSTUP_HOME`, helper state, V8/compiler/tool caches, and compatible pinned tools. Each execution creates fresh evidence and a short, hyphen-free `TEMP`/`TMP` root below `F:\.cache`. C: may provide executables and toolchains only as read-only inputs; it must not hold a build cache, target directory, or temporary state.
- `cache-state.json` records reusable-input readiness after successful candidate materialization and native bootstrap, before approved Cargo/Nextest validation commands; it is not validation success. An executor interruption after preparation does not by itself prevent warm reuse, which still requires valid state, live candidate/source integrity, compatible tools, writer exclusion, and resource admission.
- Available native command stdout/stderr is forwarded while the command runs, with complete per-command evidence logs and the final machine-readable result retained. Output forwarding does not change the terminal-batch collection requirement.
- `F:\codex-tools\bin\python3.exe` is a read-only native test-child input. The child
  `PATH` keeps its directory first ahead of WindowsApps and also includes the selected
  native Git `usr\bin` directory that provides `true.exe`; this never changes parent/global
  `PATH` or installs tools. `FORCE_COLOR=0` applies only to the test child. Retain
  `RUST_MIN_STACK=8388608`, and keep `PYTHONPYCACHEPREFIX` below that run's `TEMP`
  directory in `F:\.cache`.
- The canonical workset co-locates the Cargo target directory and Cargo intermediate build directory at the canonical target root below the Windows cache root; C: must not hold either output class.
- The supported WSL access path mounts Windows volumes read-only. Native PowerShell is the technical mechanism for Windows-side writes, not a user prohibition or extra permission checkpoint. Maintained Cargo/Nextest execution and candidate materialization remain owned by checked-in `scripts/cargo-validate-windows.ps1`; a bounded diagnostic need not be checked in. For direct diagnostics, use the quoted absolute `/mnt/c/Program Files/PowerShell/7/pwsh.exe` path without changing shell or PATH, installing tools, or adding a wrapper. Fail loud if it is unavailable. Do not write directly to `/mnt/f`. Installation, destructive actions, and privileged work retain their separate authorization boundaries.
- Source synchronization updates only changed tracked source needed to match the frozen index candidate; unchanged source and compatible Cargo, Nextest, and tool caches remain in place. Same-HEAD and supported committed-HEAD transitions preserve unchanged staged-tail source. Changed code relies on the normal build system to rebuild affected artifacts; do not delete or prune the cache merely because a crate, test, or package changed. A missing, residual, corrupt, source-mismatched, tool-incompatible, or otherwise unusable canonical workset must report the actual blocker. Never use a different or latest root, silently fall back to cold preparation, or automatically delete or prune the cache. After synchronization, tracked-source/index mismatch and ordinary untracked files still fail, while post-test ignored outputs may remain; the root source-invariance check remains independent. Receipts must bind the candidate, command, platform, executor, and terminal result.
- Runtime disk monitoring aborts the contained native command tree before accepting quiescence when configured absolute or percentage free-space thresholds are crossed. It records truthful failure and termination evidence and performs no cleanup.
- Windows cache cleanup belongs only to the current user as a manual operation. Agents must not perform or script it; validation never deletes or prunes the Windows cache. For Windows space pressure or unusable cache state, report the actual blocker and leave cleanup to the current user. This does not change the guarded WSL cleanup contract.
- The root-wide sequencing rule applies to every admitted native batch—including
  diagnostic, prep, initial, retry, focused, and full runs: it continues through
  errors to terminal completion before failure investigation or correction. Retry
  reuse matches action, stage, plan, input, and
  validation-tooling identities exactly. When changed input or tooling leaves no matching
  prior evidence, `--only-failed` can execute zero commands and records partial coverage;
  `--resume` refuses partial summaries. `--fresh` controls validation-result reuse and
  emits fresh evidence; it does not discard reusable compiled artifacts. Compiled-cache
  reuse is separate from validation-result reuse. Keep this procedure
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

## Cooldex Root Atlas

- `/home/lucas/work/codex/AGENTS.md` — fork-local durable workspace policy and stable root owner map.
- `/home/lucas/.codex/.base_instructions/sangoi_base_instructions.md` and
  `/home/lucas/.codex/.base_instructions/sangoi_subagent_instructions.md` — shared
  lead and child behavior owners.
- `/home/lucas/.codex/config.toml` — canonical default resident runtime config and pointer to the resident developer-instruction owner.
- `/home/lucas/.codex/.profile_instructions/sangoi_orchestrator_instructions.md` — default Orchestrator workflow, route registry, and resident harness contract owner.
- `/home/lucas/.codex/agents/` — registered specialist role behavior and verdict
  owners.
- `/home/lucas/work/codex/.sangoi/reference/areas/cooldex-fork-feature-inventory.md` —
  detailed current fork-feature inventory, operator-support layout, and evidence limits.
<!-- Merge-safety anchor: pre-compaction handoff synthesis stays operation-local, while
the existing compaction installer and recovery owner atomically bind and consume it. -->
- `/home/lucas/work/codex/codex-rs/core/src/compact_handoff.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/session/mod.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/session/turn.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/session/rollout_reconstruction/post_compact_recovery.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/tasks/mod.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/context/post_compact_recovery.rs`, and
  `/home/lucas/work/codex/codex-rs/core/src/state/post_compact_recovery.rs` — bounded
  pre-compaction prompt-to-self synthesis, atomic recovery binding, current reconstruction,
  typed context, pending state, first-accepted-response consumption, and fatal recovery-error
  propagation through task-abort cleanup owners.
- `/home/lucas/work/codex/codex-rs/protocol/src/protocol.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/config/mod.rs`,
  `/home/lucas/work/codex/codex-rs/core/src/agent/identity.rs`, and
  `/home/lucas/work/codex/codex-rs/core/src/session/multi_agents.rs` — MultiAgentV2
  usage-hint binding, full-history identity, and contextual-rendering owners.
<!-- Merge-safety anchor: V2 fan-in and list presentation remain bounded to existing
handler owners; canonical statuses stay full. -->
- `/home/lucas/work/codex/codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs` and
  `/home/lucas/work/codex/codex-rs/core/src/tools/handlers/multi_agents_v2/list_agents.rs`
  — current owners for token-efficient V2 fan-in, body-free list presentation, and full
  canonical statuses.
- `/home/lucas/work/codex/scripts/cargo-guard.sh`, `/home/lucas/work/codex/scripts/cargo-validation.toml`, and `/home/lucas/work/codex/scripts/cooldex/rust-blast-radius-guard.py` — guarded Rust execution, validation policy, and impact-inventory owners.
- `/home/lucas/work/codex/codex-rs/utils/cache/src/lib.rs` and `/home/lucas/work/codex/codex-rs/utils/cache/Cargo.toml` — synchronous cache locking and outside-lock shared initialization without Tokio scheduler handoff; preserve the creator-thread lifetime seam during upstream merges.
- `/home/lucas/work/codex/justfile` / `build-local-codex-package-inputs` — complete local source-input owner for the CLI, Code Mode host, bwrap, and Computer Use MCP; prepares the exact GNU V8 archive/binding pair through `scripts/codex_package/v8.py` before guarded Cargo.
- `/home/lucas/work/codex/scripts/build_codex_package.py` and `/home/lucas/work/codex/scripts/codex_package/` — one-complete-package assembly and layout-validation owners.
- `/home/lucas/work/codex/codex-rs/install-context/` — typed local-lane recognition and package-resource lookup owner.
- `/home/lucas/work/codex/codex-rs/app-server-daemon/` — lane state, subordinate managed package, lifecycle, and pinning owner.
- `/home/lucas/work/codex/codex-rs/app-server-transport/` — lane socket, startup-lock, and recovery-path owner.
- `/home/lucas/work/codex/codex-rs/app-server/`, `/home/lucas/work/codex/codex-rs/cli/`, and `/home/lucas/work/codex/codex-rs/tui/` — direct app-server, CLI, and TUI lane followers.
- `/home/lucas/work/codex/scripts/cargo-validate-windows.ps1` — native-Windows manifest executor; Windows cache cleanup remains a current-user manual operation.
- `/home/lucas/work/codex/scripts/install/install.sh` — release installer owner.
- `/home/lucas/work/codex/codex-rs/ext/computer-use/AGENTS.md` — Computer Use extension crate, vendored payload provenance, and current package-local MCP/Sky owner.
- `/home/lucas/work/codex/codex-rs/tui/src/bottom_pane/AGENTS.md` — TUI bottom-pane
  subtree instruction owner.

This Atlas is a stable owner index, not a call graph, task attachment surface, or
shipped-state ledger.
