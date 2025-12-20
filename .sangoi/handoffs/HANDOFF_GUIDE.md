# Handoff Guide — Codex Sessions

Goal: resume work after a context reset without Jira and without rewriting history.

Principles
- Keep it short (≤ 1 page), factual, and link to live docs.
- The last entry goes on top of `.sangoi/handoffs/HANDOFF_LOG.md`.
- Detailed backlog lives under `.sangoi/planning/`; the handoff only points to it.

How to write
1) Copy `.sangoi/handoffs/HANDOFF_TEMPLATE.md`.
2) Fill: Date (UTC‑3), TL;DR, What I did, File‑by‑file changes, In progress, Follow‑up, Relevant notes, Commands, Links.
3) Paste at the top of `.sangoi/handoffs/HANDOFF_LOG.md` (above the previous entry).
4) Update the planning docs under `.sangoi/planning/*.md` (progress, pendings, scope adjustments).

End‑of‑session checklist
- [ ] New entry at the top of `HANDOFF_LOG.md` following the Template.
- [ ] Planning updated in `.sangoi/planning/*.md`.
- [ ] Links to changed paths and evidence in `.sangoi/task-logs/`.

Useful link structure (examples)
- Planning: `.sangoi/planning/migration-master-plan.md`
- Roadmap: `.sangoi/planning/ROADMAP.md`
- Runbooks: `.sangoi/runbooks/*.md`
- Reference: `.sangoi/reference/**`

Notes
- Legacy scraping (WhatsApp Web) will not be ported; placeholders only document the past.
- v2 and v1 may run in parallel; cutover lives in the plan.
