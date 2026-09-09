# Scripts Atlas

## Purpose

`scripts/` owns local validation orchestration, release/package helpers, Bazel helper wrappers, install scripts, and Cooldex-specific workflow tooling. Root `AGENTS.md` remains the policy owner for when these scripts must be used; this file is a local map for editing inside this directory.

## Key Files

- `scripts/cargo-guard.sh` - guarded Cargo/build-like command wrapper, disk/resource enforcement, cleanup boundaries, and receipt writing.
- `scripts/cargo-validate.py` - deterministic planner/runner for changed-surface mechanical prep and validation.
- `scripts/cargo-validation.toml` - validation/prep command map, resource profiles, generator commands, and package/surface routing.
- `scripts/test-cargo-guard.sh` and `scripts/test-cargo-validate.py` - local regression coverage for the guard and planner.
- `scripts/cargo-validate-windows.ps1` and `scripts/test-cargo-validate-windows.py` - native-Windows manifest executor and its deterministic PowerShell/fake-tool coverage.
- `scripts/clear-windows-build-cache.ps1` and `scripts/test-clear-windows-build-cache.py` - exact `F:\.cache` cleanup seam and regression coverage.
- `scripts/cooldex/rust-blast-radius-guard.py` - Rust reachability/impact-map helper for unresolved impact questions.
- `scripts/cooldex/test-rust-blast-radius-guard-items.py` - Python regression coverage for blast-radius item resolution and report-summary behavior.
- `scripts/codex_package/` - Python package/release layout helpers and tests.
- `scripts/install/install.sh` and `scripts/install/test_install_sh.py` - standalone GitHub Release resolution, checksum-verified installation, and regression coverage.
- `scripts/run_bazel_with_buildbuddy.py`, `scripts/run-bazel-query.sh`, `scripts/list-bazel-*.sh` - Bazel execution/query helpers.
- `scripts/macos-signing/` and release/archive scripts - remaining platform packaging and signing helpers.

## Durable Notes

- Keep Cargo/build-like validation behavior centralized in `cargo-guard.sh`, `cargo-validate.py`, and `cargo-validation.toml`; do not add parallel ad hoc validation wrappers.
<!-- Merge-safety anchor: native-Windows execution remains a thin checked-in PowerShell backend for the planner-owned manifest; full-mode WSL test preparation follows the config-owned explicit package mapper, with WSL guard dispatch and exact F-cache cleanup kept separate. -->
- `cargo-validation.toml` and `cargo-validate.py` own the frozen manifest, platform
  partitions, explicit exclusions, and the native-Windows 16-build-job/8-test-thread
  resource contract. The executor fixes native test-child `RUST_MIN_STACK` at `8388608`
  (8 MiB). `cargo-guard.sh` owns WSL dispatch; the PowerShell helpers own native Windows
  execution, writes, and cleanup, not a second partitioning policy.
<!-- Merge-safety anchor: native aggregate dev/test opt1 configuration remains literal
command arguments with limited symbols, debug assertions, and overflow checks; WSL
codegen remains unchanged. -->
- `commands.windows-nextest-workspace` owns the exact native `--config` pairs for
  `profile.dev` and `profile.test`: `opt-level=1`, `debug="limited"`,
  `debug-assertions=true`, and `overflow-checks=true`. The settings are direct TOML argv
  entries for the existing profiles. The default adds no diagnostic verbosity and leaves
  WSL codegen unchanged.
<!-- Merge-safety anchor: voice source remains workspace-owned, while only
codex-voice-host validation is deliberately excluded through the TOML selection policy
and planner warnings retain the unvalidated limitation. -->
- `defaults.validation_excluded_packages` in `cargo-validation.toml` excludes only
  `codex-voice-host` from package-derived WSL validation rungs and the full native
  workspace aggregate. Retain its explicit path classification and plan warning; do not
  treat this validation exclusion as source removal or product proof.
- `cargo-validate.py` owns parsing and frozen-manifest projection of explicit
  `--windows-reuse-root` and native-Windows-only `--yolo`; `cargo-validate-windows.ps1`
  owns selected-root compatibility and source checks, in-place synchronization, fresh run
  paths, the native test-child environment, and the recorded resource-floor bypass. Root
  `AGENTS.md` owns the operator-facing cold/reuse, override, and validation-result-reuse
  rules.
- Native Windows Cargo/Nextest is valid only through the WSL guard, frozen manifest, and
  checked-in PowerShell executor. Direct Windows Cargo/Nextest and the former Windows
  `just test` route are invalid.
- The supported WSL access path mounts Windows volumes read-only. Native `pwsh.exe` or
  `pwsh` is the technical mechanism for Windows-side writes, not a user prohibition or
  extra permission checkpoint. The checked-in executor and cleanup scripts retain
  ownership of maintained native Cargo/Nextest, candidate materialization, and destructive
  cleanup; a bounded diagnostic need not be checked in. Installation, destructive actions,
  and privileged work retain their separate authorization boundaries.
- Windows-created mutable candidate, target, Cargo/Rustup, temporary, cache, staging, log,
  and evidence state must remain below literal `F:\.cache`; C: toolchains are read-only
  inputs, and WSL must not write directly to `/mnt/f`.
- `Invoke-ApprovedCommand` in `cargo-validate-windows.ps1` owns the native Python
  prerequisite, child-only PATH/`true.exe`/color/stack settings, and run-local bytecode
  cache. Root `AGENTS.md` owns the corresponding operator contract.
<!-- Merge-safety anchor: the literal-root cleanup authority treats admitted nested
junctions as leaf entries and never traverses or deletes through their targets. -->
- `clear-windows-build-cache.ps1` is the only Windows cache cleanup path: preflight is the
  default, `-Delete` requires proven WSL/Windows quiescence, and it may delete only captured
  direct children of literal `F:\.cache` while preserving the root, removing admitted nested
  junctions as leaf entries, and emitting JSON outside it.
- The standalone installer's default GitHub Release repository is
  `sangoi-exe/cooldex`. The `releases.openai.com` source remains an explicit
  opt-in path; keep its upstream URLs and behavior separate from the Cooldex
  GitHub owner.
- The retired `scripts/cooldex/native-diff-budget.sh` is intentionally absent and is not a validation gate, proof owner, seam selector, or progress metric.
- `scripts/cargo-validation.toml` resource profiles own build-job bounds and runtime-thread
  caps. Official WSL/Linux profiles keep runtime tests serialized at one thread, restore the
  historical build-job max/hard caps, and set `cargo_jobs_default = "min"` so ordinary runs
  use the profile minimum unless a lower/equal inline Cargo/nextest build-job override is
  explicit. This WSL/Linux serialization rule does not redefine the separate planner-owned
  native-Windows 16/8 profile.
- `cargo-guard.sh` rejects inline Cargo/nextest build-job values above the selected cap, but inline values below or equal to that cap are allowed and should not require temporary profile edits.
- Supported WSL/Linux developer helpers must build through `cargo-guard.sh`. Helpers
  that start long-lived Codex processes build first under the guard, resolve the
  effective Cargo target directory, and then launch the built binaries outside the
  guard so the build lock is not held for the process lifetime.
- `test-cargo-guard.sh` executes the real `tui-with-exec-server` just recipe with fake
  Cargo and fake built binaries so the transitive helper route remains covered.
- Planner-driven `verify` defaults to `--telemetry-level full`; direct guarded Cargo commands and known profiled `just` recipes that invoke `cargo-guard.sh` receive TSV paths under `.sangoi/validation/command-logs/**` beside stdout/stderr logs. Non-Cargo commands do not get fake telemetry artifacts. Use `summary` for lighter receipt metadata, `debug` for per-process rustc detail rows, and `off` only when telemetry is intentionally disabled.
- `cargo-validate.py` keeps pre-review mechanical materialization separate from validation: `prep-plan`/`prep` may run tree-mutating formatter, generator, and lock-refresh commands before review, while `plan`/`verify` must stay non-mutating validation actions.
- In `--mode full`, `cargo-validate.py` gates WSL test-target preparation through the
  `wsl_runtime_packages` path-rule mapping in `cargo-validation.toml`; update that mapper
  and `test-cargo-validate.py` together. Other modes retain their current selection policy.
- Root `AGENTS.md` `Guarded Rust Validation` and `Native-Windows bulk test procedure`
  own all-batch terminal collection before failure investigation/correction plus the
  exact-match retry and fresh-collection requirements.
<!-- Merge-safety anchor: keep selector provenance and this local validation map aligned so committed package deletions remain plannable without weakening unknown-path failures. -->
- `cargo-validate.py` owns changed-surface selectors: `--changed`, `--commit`,
  `--range`, `--file`, and `--surface`. `--range` follows
  status-aware `git diff --name-status <rev-range>` output; `--commit` uses the
  matching non-merge commit view and must direct merge commits to
  `--range <base>..<merge>`. Revision selectors retain deletion provenance for
  committed package removals, select the destination of rename/copy records,
  and fail loud on malformed status records. A historical deletion cannot
  override a revision re-add, a current path, or an explicit `--file` selector.
  `--json` is machine-readable output, not a selector-input schema.
- `--changed` selects paths from cached, unstaged, and ordinary untracked worktree
  changes; it does not turn arbitrary worktree bytes into a native candidate.
  `cargo-validate-windows.ps1` consumes the index candidate and requires
  worktree/index equality with no ordinary untracked source. The root owns exact
  task staging; Workers do not stage.
- `cargo-guard.sh` preserves successful `-p/--package` caches and cleans only the failed package with `cargo clean -p <package>` after package-targeted failures or disk emergencies; broad clean stays limited to clean-required pressure without package targets.
- When changing validation command selection, resource profiles, receipt semantics, cleanup behavior, or target-cache behavior, update the matching script tests and root Atlas/validation notes if validation truth changes.
- `cargo-validate.py` and `cargo-validation.toml` should fail loud on unknown durable surfaces instead of silently skipping them.
- Runtime receipts belong under `.sangoi/validation`; scripts should not write validation receipts or helper state under `codex-rs/target`.
- Ignore generated Python caches such as `__pycache__/`. Do not stage cache files when editing script sources or tests.
- `scripts/codex_package/README.md` is package documentation, not an agent-instruction owner.
