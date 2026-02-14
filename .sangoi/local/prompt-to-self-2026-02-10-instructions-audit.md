# Prompt to self — resume instructions audit (Jules / drift / noise)

```text
You are working in the repo `codex`.
Continue from:
- CWD: /home/lucas/work/codex
- Branch: reapply/accounts-20260209
- Last commit: 54b401aa5fb2f2a7dec3ae13ac2a93a0cbc7bb9a
- Commit date (UTC): 2026-02-09 15:16:54 +0000

Objective (1 sentence)
- Deliver an implacable audit of instruction quality (drift, technical debt, noise, churn, conflicts) across base instructions, sub-agent instructions, and developer instructions, and produce a prioritized adjustment plan while preserving Jules tone.

State
- Done:
  - `~/.codex/config.toml` points to Jules instruction files:
    - `model_instructions_file = "/home/lucas/.codex/sangoi_base_instructions_jules.md"`
    - `subagent_instructions_file = "/home/lucas/.codex/sangoi_subagent_instructions_jules.md"`
  - Current metrics collected:
    - Base: `sangoi_base_instructions.md` = 388 lines / 29189 chars
    - Base Jules: `sangoi_base_instructions_jules.md` = 427 lines / 22394 chars
    - Similarity ratio (line-based): ~0.4687 (high drift risk)
    - Sub-agent similarity ratio: ~0.8980 (lower drift risk)
  - Key semantic drift evidence captured:
    - Base `update_plan` block was shortened/rephrased in Jules version.
    - Several narrative/constraint lines changed from source wording.
  - Scratchpad path divergence identified:
    - Active path in developer instructions: `/home/lucas/.codex/codex_learning_log.md`
    - Old path still exists: `/home/lucas/.codex/scratchpad/codex_learning_log.md`
- In progress:
  - Final audit report (severity-ranked findings + concrete remediation plan).
- Blocked / risks:
  - Context bloat risk is high in long runs; keep command output minimal.
  - Confusion/churn risk because non-Jules and Jules files coexist and differ materially.
  - If we “rewrite for style” again without guardrails, semantics may drift further.

Decisions / constraints (locked)
- Keep Jules tone and style.
- Do not lose instruction detail.
- Sub-agent has its own dedicated instruction file.
- Developer instructions remain overlay policy (not full replacement for base).
- Prefer smallest-risk, highest-signal changes first.
- Fail loud on ambiguity: if scope is unclear, ask instead of guessing.

Follow-up (ordered)
1. Produce a severity matrix: hard conflicts, soft tensions, stale clauses, duplication/noise.
2. Decide canonical source of truth per layer:
   - Base (runtime behavior),
   - Sub-agent base (spawn behavior),
   - Developer overlay (operator policy / orchestration).
3. Define anti-drift acceptance checks (must-pass invariants).
4. Reconcile scratchpad path into one canonical location and migrate safely.
5. Recommend precise edits with rollback order and validation criteria.

Next immediate step (do this first)
- Generate the audit findings table with evidence and severity.
Commands:
python - <<'PY'
from pathlib import Path
import difflib
base=Path('/home/lucas/.codex/sangoi_base_instructions.md').read_text('utf-8').splitlines()
j=Path('/home/lucas/.codex/sangoi_base_instructions_jules.md').read_text('utf-8').splitlines()
print('base_vs_jules_ratio', round(difflib.SequenceMatcher(a=base,b=j).ratio(),4))
PY
diff -u /home/lucas/.codex/sangoi_base_instructions.md /home/lucas/.codex/sangoi_base_instructions_jules.md | sed -n '1,220p'
diff -u /home/lucas/.codex/sangoi_subagent_instructions.md /home/lucas/.codex/sangoi_subagent_instructions_jules.md | sed -n '1,160p'
nl -ba /home/lucas/.codex/config.toml | sed -n '1,40p'
nl -ba /home/lucas/.codex/config.toml | sed -n '186,205p'

Files
- Changed files (last relevant changes for this task are local config artifacts, not repo commits):
  - `/home/lucas/.codex/config.toml`
  - `/home/lucas/.codex/sangoi_base_instructions.md`
  - `/home/lucas/.codex/sangoi_base_instructions_jules.md`
  - `/home/lucas/.codex/sangoi_subagent_instructions.md`
  - `/home/lucas/.codex/sangoi_subagent_instructions_jules.md`
  - `/home/lucas/.codex/codex_learning_log.md`
  - `/home/lucas/.codex/scratchpad/codex_learning_log.md`
- Focus files to open first:
  - `/home/lucas/.codex/config.toml` — active wiring and overlay behavior.
  - `/home/lucas/.codex/sangoi_base_instructions.md` — baseline semantics.
  - `/home/lucas/.codex/sangoi_base_instructions_jules.md` — active runtime file to audit for drift.
  - `/home/lucas/.codex/sangoi_subagent_instructions.md` — baseline sub-agent semantics.
  - `/home/lucas/.codex/sangoi_subagent_instructions_jules.md` — active sub-agent runtime file.

Validation (what “green” looks like)
- Config parses:
  - `python -c "import tomllib, pathlib; tomllib.loads(pathlib.Path('/home/lucas/.codex/config.toml').read_text())"`
  - Expected: exits 0.
- Active wiring points to intended files:
  - `rg -n "^model_instructions_file|^subagent_instructions_file" /home/lucas/.codex/config.toml`
  - Expected: both set to `*_jules.md`.
- Audit output quality:
  - Findings include severity + evidence (`path:line`) + impact + remediation + rollback note.
  - Plan is prioritized and executable in small batches.

Known traps / gotchas
- Editing non-Jules files does nothing while config points to `*_jules.md`.
- “Style rewrite” can silently change semantics; require explicit invariant checks before accepting edits.
- Scratchpad exists in two places; this creates behavior ambiguity and operator confusion.
- Do not confuse repo commit history with local `~/.codex` config state.

References (.sangoi/**)
- `.sangoi/local/prompt-to-self-2026-02-09-accounts-reapply.md`
- `.sangoi/local/prompt-to-self-2026-02-10-instructions-audit.md`
```
