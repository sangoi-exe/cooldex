# Scripts Atlas

Last reviewed: 2026-07-25.

## Purpose

`scripts/` owns local validation orchestration, release/package helpers, Bazel helper wrappers, install scripts, and Cooldex-specific workflow tooling. Root `AGENTS.md` remains the policy owner for when these scripts must be used; this file is a local map for editing inside this directory.

## Key Files

- `scripts/cargo-guard.sh` - guarded Cargo/build-like command wrapper, disk/resource enforcement, cleanup boundaries, and receipt writing.
- `scripts/cargo-validate.py` - deterministic planner/runner for changed-surface mechanical prep and validation.
- `scripts/cargo-validation.toml` - validation/prep command map, resource profiles, generator commands, and package/surface routing.
- `scripts/test-cargo-guard.sh` and `scripts/test-cargo-validate.py` - local regression coverage for the guard and planner.
- `scripts/cooldex/rust-blast-radius-guard.py` - Rust reachability/impact-map helper required by root policy.
- `scripts/cooldex/test-rust-blast-radius-guard-items.py` - Python regression coverage for blast-radius item resolution and report-summary behavior.
- `scripts/codex_package/` - Python package/release layout helpers and tests.
- `scripts/install/install.sh` and `scripts/install/test_install_sh.py` - standalone GitHub Release resolution, checksum-verified installation, and regression coverage.
- `scripts/run_bazel_with_buildbuddy.py`, `scripts/run-bazel-query.sh`, `scripts/list-bazel-*.sh` - Bazel execution/query helpers.
- `scripts/macos-signing/` and release/archive scripts - remaining platform packaging and signing helpers.

## Durable Notes

- Keep Cargo/build-like validation behavior centralized in `cargo-guard.sh`, `cargo-validate.py`, and `cargo-validation.toml`; do not add parallel ad hoc validation wrappers.
- The standalone installer's default GitHub Release repository is
  `sangoi-exe/cooldex`. The `releases.openai.com` source remains an explicit
  opt-in path; keep its upstream URLs and behavior separate from the Cooldex
  GitHub owner.
- The retired `scripts/cooldex/native-diff-budget.sh` is intentionally absent and is not a validation gate, proof owner, seam selector, or progress metric.
- `scripts/cargo-validation.toml` resource profiles own build-job bounds and runtime-thread caps. Official profiles keep runtime tests serialized at one thread, restore the historical build-job max/hard caps, and set `cargo_jobs_default = "min"` so ordinary runs use the profile minimum unless a lower/equal inline Cargo/nextest build-job override is explicit.
- `cargo-guard.sh` rejects inline Cargo/nextest build-job values above the selected cap, but inline values below or equal to that cap are allowed and should not require temporary profile edits.
- Supported WSL/Linux developer helpers must build through `cargo-guard.sh`. Helpers
  that start long-lived Codex processes build first under the guard, resolve the
  effective Cargo target directory, and then launch the built binaries outside the
  guard so the build lock is not held for the process lifetime.
- `test-cargo-guard.sh` executes the real `tui-with-exec-server` just recipe with fake
  Cargo and fake built binaries so the transitive helper route remains covered.
- Planner-driven `verify` defaults to `--telemetry-level full`; direct guarded Cargo commands and known profiled `just` recipes that invoke `cargo-guard.sh` receive TSV paths under `.sangoi/validation/command-logs/**` beside stdout/stderr logs. Non-Cargo commands do not get fake telemetry artifacts. Use `summary` for lighter receipt metadata, `debug` for per-process rustc detail rows, and `off` only when telemetry is intentionally disabled.
- `cargo-validate.py` keeps pre-review mechanical materialization separate from validation: `prep-plan`/`prep` may run tree-mutating formatter, generator, and lock-refresh commands before review, while `plan`/`verify` must stay non-mutating validation actions.
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
- `cargo-guard.sh` preserves successful `-p/--package` caches and cleans only the failed package with `cargo clean -p <package>` after package-targeted failures or disk emergencies; broad clean stays limited to clean-required pressure without package targets.
- When changing validation command selection, resource profiles, receipt semantics, cleanup behavior, or target-cache behavior, update the matching script tests and root Atlas/validation notes if validation truth changes.
- `cargo-validate.py` and `cargo-validation.toml` should fail loud on unknown durable surfaces instead of silently skipping them.
- Runtime receipts belong under `.sangoi/validation`; scripts should not write validation receipts or helper state under `codex-rs/target`.
- Ignore generated Python caches such as `__pycache__/`. Do not stage cache files when editing script sources or tests.
- `scripts/codex_package/README.md` is package documentation, not an agent-instruction owner.
