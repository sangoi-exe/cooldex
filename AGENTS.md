# AGENTS.md — Codex Repo Guidance

This file provides repo‑specific guidance for agents and contributors working in this project. Keep it concise and actionable.

## Commit & Push Policy (General)

- Always commit and push only the files actually modified for the task — nothing else, under any circumstance.
- Before committing, verify the staged set is exactly what you intend:
  - `git diff --cached --name-only` (staged files)
  - `rg -n "<<<<<<<|=======|>>>>>>>"` (ensure no merge markers)
- Before opening/updating a PR, verify the delta vs. base contains only the expected files:
  - `git diff --name-only upstream/main...HEAD`
- Do not add vendored/build artifacts, lockfiles, or unrelated edits unless explicitly requested.
- Do not run destructive commands that reset or hide unrelated work (e.g., `git clean`, `git revert`, forced resets). If a clean base is required, branch from `upstream/main` and add only your intended changes.

## Rust (codex-rs) conventions (abridged)

- Run `just fmt` after Rust changes; run `just fix -p <crate>` before finalizing.
- Prefer inlined `format!` args; collapse nested `if` where clippy suggests; use method refs over trivial closures.
- Snapshot tests: use `cargo insta` workflow; accept snapshots intentionally.

## Contact

If a task risks touching more files than originally scoped, stop and ask before proceeding.
