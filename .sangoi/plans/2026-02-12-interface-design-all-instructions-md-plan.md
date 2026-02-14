# Plan — Single `.md` compilation of all instructions (`dammyjay93/interface-design`)

**Difficulty:** medium

## 1) Scope lock (must confirm with user)
- Confirm what “all instructions” means:
  - **A:** entire repository instruction docs
  - **B:** only interface-design skill/command docs
- Confirm output path/filename.
- Confirm style:
  - **verbatim-heavy** (closer to original text)
  - **normalized** (cleaned, reorganized, concise)
- Confirm whether inline `Source: <path>` traceability is required.

## 2) Exhaustive source discovery (via `gh`)
- Enumerate candidate instruction docs from repo tree.
- Classify each candidate as in-scope or excluded (with reason).
- Artifacts:
  - `./.sangoi/docs/interface-design-instruction-candidates.txt`
  - `./.sangoi/docs/interface-design-instruction-sources.txt`
  - `./.sangoi/docs/interface-design-instruction-excluded.txt`

## 3) Section architecture
- Build deterministic heading map in final file:
  - `#` document title
  - `##` major groups
  - `###` file-level sections
  - `####` instruction blocks
- Add conflict handling subsection when sources disagree.

## 4) Assemble final single `.md`
- Generate one English `.md` file at confirmed path.
- Ensure every in-scope source is represented.
- Add source traceability (if confirmed).

## 5) Validation + review gate
- Structural checks: file exists, headings capped at `####`.
- Coverage checks: all in-scope sources represented.
- Classification checks: every candidate classified.
- Language sanity check (English output).
- Senior Code Reviewer gate and fix any blockers/nits.

## Done criteria
- [ ] Exactly one final `.md` exists at confirmed location.
- [ ] Output uses heading levels `# ## ### ####` only.
- [ ] Every in-scope source appears in the compilation.
- [ ] All candidates are either included or excluded with reason.
- [ ] Final output is English.
- [ ] Reviewer gate returns `READY` (or nits resolved to READY).

## Verification commands (to run)
- `test -s <output.md>`
- `rg -n '^#{5,}\s' <output.md>` (must return empty)
- `for h in '^# ' '^## ' '^### ' '^#### '; do rg -n "$h" <output.md> >/dev/null || echo "MISSING:$h"; done`
- `while IFS= read -r f; do rg -F -n "Source: $f" <output.md> >/dev/null || echo "MISSING_SOURCE:$f"; done < .sangoi/docs/interface-design-instruction-sources.txt`

## Lane split
- Single implementation lane (document build is linear).
- Gate 1: Senior Plan Advisor (done).
- Gate 2: Senior Code Reviewer (after implementation).
