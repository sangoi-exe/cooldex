# AGENTS.md — Codex Repo Guidance

This file provides repo‑specific guidance for agents and contributors working in this project. Keep it concise and actionable.

## Configuration Example (docs/config.toml)

- Canonical example lives at `docs/config.toml`.
- It lists every TOML key Codex accepts, sets effective defaults where applicable, leaves optional keys commented, and includes short annotations on purpose and valid values.
- When contributing this example upstream:
  - Branch from `upstream/main`.
  - Include only one file in the PR: `docs/config.toml`.
  - Verify before pushing: `git diff --name-only upstream/main...HEAD` must output exactly `docs/config.toml`.

## PR Hygiene

- Commit and push only what changed; do not mix unrelated edits.
- Do not add vendored/build artifacts or lockfiles unless the task explicitly requires it.
- Avoid destructive resets/cleanups that could hide unrelated changes.

## Rust (codex-rs) conventions (abridged)

- Run `just fmt` after Rust changes; run `just fix -p <crate>` before finalizing.
- Prefer inlined `format!` args; collapse nested `if` where clippy suggests; use method refs over trivial closures.
- Snapshot tests: use `cargo insta` workflow; accept snapshots intentionally.

## Contact

If a task risks touching more than `docs/config.toml` for the config example, stop and ask before proceeding.
