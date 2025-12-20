#!/usr/bin/env markdown
# Handoff Protocol — v2 (Single Source of Truth)

Purpose
- Enable exact continuity between sessions even when chat history is truncated.
- Avoid duplicating content that already lives in the live docs.

Single Sources
- Live ledger (reverse‑chronological): `.sangoi/handoffs/HANDOFF_LOG.md`.
- Template (single): `.sangoi/handoffs/HANDOFF_TEMPLATE.md` — do not invent new sections.
- Live docs (do not paste into the handoff):
  - Task logs: `.sangoi/task-logs/*.md`
  - Changelog: `.sangoi/CHANGELOG.md`
  - Planning: `.sangoi/planning/*.md`
  - Runbooks: `.sangoi/runbooks/*.md`
  - Reference/specs: `.sangoi/reference/**`

Minimum Per Entry (no repetition)
1) Header: Date (UTC‑3), Author, Anchor (planning/task‑log).
2) TL;DR (3–5 lines): objective, delivered, in‑flight, next command.
3) What I did: concrete actions in past tense with links to evidence.
4) File‑by‑file changes: one item per path — “why” before “how”.
5) In progress: stop point and acceptance criteria to resume.
6) Follow‑up: ordered list with dependencies and expected outcome.
7) Relevant notes: informal decisions, traps, sensitive areas (names only).
8) Commands I ran: one per line with ✅/❌ and key error.
9) Useful links: pointers to live docs only.

End‑of‑Session Checklist
- [ ] I wrote an entry using the template exactly.
- [ ] I updated planning/checklists under `.sangoi/planning/` with progress and pendings.
- [ ] I logged evidence in `.sangoi/task-logs/` and linked it from the handoff.
- [ ] If something broke, I described the broken state explicitly.

Start‑of‑Session Ritual
1) Read the top entry in `.sangoi/handoffs/HANDOFF_LOG.md`.
2) Re‑validate assumptions/decisions before proceeding.

Good Practice
- ≤ ~25 lines per entry; short bullets; link to details.
- No huge logs in the handoff; use `.sangoi/task-logs` with anchors.
- No shell continuations; one command per line.
- If it only lives in the working tree, say so. If it’s committed, reference commit/branch.

Optional Fields
- Pending tests before merge.
- Deploy: status/note when applicable.
- Questions for the next session.
