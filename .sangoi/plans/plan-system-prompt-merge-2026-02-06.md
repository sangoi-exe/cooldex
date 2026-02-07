# Plan (medium→hard): System prompt “v3” (merge prompt1 + prompt2 + dev instructions)

Date: 2026-02-06

## Goal (explicit)

Produce a clean, maintainable Codex CLI model instructions file that:

- Keeps the best parts of `.sangoi/prompt2.md` (Codex CLI baseline).
- Pulls in the best parts of `.sangoi/prompt1.md` (review mindset + defensive git guidance) without the noisy bits.
- Transfers the “non‑negotiables” from the provided `developer_instructions` into the base prompt:
  - underspecified → ask; options; recommendation; next action
  - fail loud; root-cause fixes; no silent fallbacks unless explicitly desired and correct
  - pre-flight ceremony (recon → plan file → ask for “yes” before edits)
  - tool-output hygiene / `manage_context` baseline
- Removes instruction conflicts (esp. “Unless the user explicitly asks for a plan…” vs. the pre-flight ceremony).
- Stays compatible with repo-specific `AGENTS.md` rules (including `/home/lucas/work/stable-diffusion-webui-codex/AGENTS.md`) by using one clear precedence model and explicitly deferring to repo-scoped rules.
- Does not assume a specific workspace layout (no `.sangoi` dependency in the prompt content).

## Non-goals

- No behavioral changes to the codebase itself (this is prompt/docs work only).
- No repo-specific policies hardcoded into the base prompt (we’ll point to repo instructions like `AGENTS.md` and repo-local docs as the source of truth; in this repo those docs live under `.sangoi`).

## Definition of Done

- `.sangoi/sangoi_base_instructions.md` is clean Markdown (LF, UTF-8), readable (real newlines), and internally consistent.
- No known-false claims survive (e.g. “user does not see command output”).
- Planning rule is coherent with the pre-flight ceremony and does not deadlock pure Q&A.
- The base instructions explicitly defer to repo-scoped `AGENTS.md` within scope (no hidden contradictions).
- The prompt text stays cross-repo portable (no `.sangoi` dependency in the content).

## Outputs (proposed)

Canonical outputs:
- [x] `.sangoi/sangoi_base_instructions.md` (final “v3” base instructions file for Codex CLI `model_instructions_file`)

Non-goals for this run:
- [x] Do not modify `.sangoi/prompt1.md` or `.sangoi/prompt2.md` (they remain as raw rollout dumps).

## Step-by-step checklist (no guessing)

- [x] Recon inputs (DONE = we can parse/repair, not “strip and hope”)
  - Inspect `.sangoi/prompt1.md`, `.sangoi/prompt2.md`, `developer_instructions`, and `/home/lucas/work/stable-diffusion-webui-codex/AGENTS.md`.
  - Identify: what’s authoritative vs what’s duplicated boilerplate.

- [x] Draft `.sangoi/sangoi_base_instructions.md` (DONE = one coherent rule-set, no contradictions)
  - Start from prompt2 baseline.
  - Remove/replace planning rule: drop “Unless the user explicitly asks for a plan…”.
  - Insert pre-flight ceremony in a way that does not deadlock pure Q&A:
    - pre-flight applies before file edits / non-trivial tool runs, not before trivial answers.
  - Add: underspecified → ask/options/recommend/next.
  - Add: fail loud + root-cause fixes (no silent fallbacks).
  - Add: tool-output hygiene / `manage_context` baseline.
  - Add: precedence model + explicit deferral to repo-scoped `AGENTS.md`.
  - Ensure the prompt text does not reference `.sangoi` (cross-repo portability).

- [x] Verification pass (DONE = all checks green; manual skim passes)

## Lanes / sub-agents

- [x] Senior Plan Advisor: review this plan for missing steps/conflicts/verification.
- [x] Senior Code Reviewer: reviewed diffs and punchlisted fixes.
- [ ] No extra sub-agents unless we discover additional repos/prompts to reconcile.

## Verification (must be green)

- [x] `wc -l .sangoi/sangoi_base_instructions.md` is “normal” (expect >> 1; likely > 50).
- [x] No mojibake in outputs (no `â` / `Ã` sequences).
- [x] No stray JSON artifacts in `.sangoi/sangoi_base_instructions.md` (e.g. `"base_instructions"`, `\\n`, `\\\"`).
- [x] `rg -n -F "Unless the user explicitly asks for a plan" .sangoi/sangoi_base_instructions.md` returns empty.
- [x] `rg -n -F "does not see command execution outputs" .sangoi/sangoi_base_instructions.md` returns empty.
- [x] `rg -n -F ".sangoi" .sangoi/sangoi_base_instructions.md` returns empty.
- [x] Manual skim: the base instructions have one clear precedence rule and no contradictory “do X / don’t do X” pairs.
