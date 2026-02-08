# Prompt-to-self guide (clean-session resume)
Date: 2026-01-22
Last Review: 2026-01-22
Status: Active

This repo is big, and model context is finite. When a session is long or you want to restart with a clean context window, you don’t “remember what we did”.
You generate a **prompt-to-self**: a single copy/paste prompt that contains the minimum *actionable* context to resume work correctly.

This guide defines how to produce that prompt when the user asks for something like **"um prompt pra ti mesmo"**.

## Where the template lives
- Template: `.sangoi/templates/PROMPT_TO_SELF_TEMPLATE.md`

## Output rules (non-negotiable)
- Output the prompt in a new Markdown file graciously formatted.
- Use repo-relative paths (clickable) and exact commands.
- Include the last relevant commit hash(es) and the branch.
- Prefer links to `.sangoi/**` artifacts over repeating long text.
- No secrets, tokens, private URLs, or local machine paths outside the repo.
- No giant diffs or logs; summarize and link instead.

## What the prompt must contain
Think of it as a clean restart capsule, aimed at a *fresh instance of yourself*.

Required sections:
1) **Context header**
   - Repo + CWD
   - Branch + last commit hash
   - Date (UTC-3)
2) **Objective**
   - The goal in one sentence.
3) **State**
   - What is done
   - What is in progress
   - What is blocked / risky
4) **Decisions / constraints**
   - The “locked” decisions that must not be re-litigated (e.g., Option A “canonical use-case per mode”).
5) **Follow-up list**
   - Ordered next tasks (3–7 bullets).
6) **Next immediate step**
   - The single next thing to do (one sentence + 1–3 commands).
7) **Files**
   - Files changed (from the last commit(s)).
   - Focus files to open first (paths + why each matters).
8) **Validation**
   - Commands to run (tests/build/link-check) and what success looks like.
9) **References**
   - Links to `.sangoi/**` docs and any `.refs/**` upstream anchors that matter.

## What not to do
- Do not assume the next session has any memory of today’s thread.
- Do not say “as discussed above”.
- Do not paste whole files or long diffs into the prompt.
- Do not hide ambiguity: if a decision is still open, list it as a decision point.

## Quick checklist
- [ ] Includes commit hash + branch
- [ ] Includes ordered follow-ups + 1 next step
- [ ] Lists changed files + focus files
- [ ] Includes validation commands
- [ ] Links the relevant `.sangoi/**` artifacts
