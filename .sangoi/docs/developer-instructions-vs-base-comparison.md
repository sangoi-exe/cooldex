# Comparison: Config `developer_instructions` vs Base Instructions

## Scope
### Requested path status
#### `~/.config.toml` was not found in this environment.

### Effective config used for comparison
#### `/home/lucas/.codex/config.toml` (contains inline `developer_instructions` and references to model/subagent instruction files).

### Base instructions file
#### `/home/lucas/.codex/sangoi_base_instructions_jules.md`

## Inputs and extraction
### Extracted config block
#### Source
`developer_instructions = """ ... """` block from `/home/lucas/.codex/config.toml`.

### Extracted base file
#### Source
Full content of `/home/lucas/.codex/sangoi_base_instructions_jules.md`.

## Quantitative snapshot
### Line counts
#### Config developer block
- 210 lines

#### Base instructions file
- 379 lines

## Structural comparison
### Config block profile
#### What it mainly contains
- User-specific overlays and strict workflow constraints
- Research discipline requirements
- Continual scratchpad protocol
- Sub-agent playbook details (`Senior Plan Advisor`, `Senior Code Reviewer`)
- End-of-turn improvement checks

### Base file profile
#### What it mainly contains
- Canonical Codex CLI operating framework
- Core sections such as personality, writing style, AGENTS spec, planning, editing constraints, validation, and presentation rules
- Tool usage standards and output formatting contracts

### Heading-level comparison (`##`)
#### Result
- No shared `##` headings between extracted config developer block and base file
- Config block introduces overlay sections
- Base file carries the core framework sections

## Key differences
### Present in config block but not in base (as dedicated sections)
#### Notable overlays
- Research discipline (explicit web research rule)
- Continual scratchpad protocol (`/home/lucas/.codex/codex_learning_log.md`)
- Sub-agent process templates and mandatory reviewer gates
- Extra end-of-turn follow-up requirements

### Present in base file but not in config block (as dedicated sections)
#### Core framework coverage
- `## Personality`
- `## Writing style`
- `## AGENTS.md spec`
- `## Planning`
- `## Editing constraints`
- `## Validating your work`
- `## Presenting your work`
- `## Shell commands`
- `## apply_patch`
- `## manage_context`
- `## update_plan`

## Relationship between the two
### Practical interpretation
#### Layered model
- Base file provides the default operational contract.
- Config `developer_instructions` acts as an overlay with stricter, user-specific constraints and workflow add-ons.

### Activation pointers from config
#### Relevant keys
- `model_instructions_file = "/home/lucas/.codex/sangoi_base_instructions_jules.md"`
- `subagent_instructions_file = "/home/lucas/.codex/sangoi_subagent_instructions_jules.md"`

## Conclusion
### Summary
#### The config developer block does not replace the base file structure; it extends behavior with session-specific constraints (research discipline, scratchpad discipline, and stricter sub-agent review flow).

## Redundancy Pruning Applied to the Unified Interface-Design File
### File targeted for pruning
#### `.sangoi/docs/interface-design-all-instructions-compilation.md`

### Pruning criteria derived from this comparison
#### Removed from unified file as globally redundant
- Generic process-governance wording already enforced by the Codex base stack.
- Generic clarification/process narration rules that were not interface-design-specific.
- Repeated command/flow boilerplate that duplicated meaning without adding domain detail.

#### Kept in unified file
- All interface-design-specific craft, workflow, command, memory, validation, template, and preset rules.
- Inlined referenced local content (`LICENSE`) in the same `.md`.

### Traceability upgrade applied
#### Added to unified file
- A source-by-source `Coverage and Pruning Ledger` that records retained rule groups and explicit prune categories for each source file.
- Baseline anchor list from config/base sections used to justify redundancy pruning decisions.

### License provenance clarification
#### Clarified source target
- The inlined license in the unified file is explicitly labeled as the upstream interface-design repository license (`/tmp/interface-design-VO7IK1/LICENSE`), not this workspace root license file.

## Reproducibility Appendix
### Extracted `##` headings from config developer block
#### Values
- ## Before you end your turn:
- ## Continual scratchpad protocol
- ## Research discipline (global)
- ## Sub-agents

### Extracted `##` headings from base instructions file
#### Values
- ## AGENTS.md spec
- ## Ambition vs. precision
- ## Autonomy and Persistence
- ## Bug fixes (get B, not band-aids)
- ## Editing constraints
- ## Error handling (no gymnastics)
- ## Frontend tasks (avoid AI slop)
- ## Personality
- ## Planning
- ## Presenting your work
- ## Shell commands
- ## Special user requests
- ## Task execution
- ## Validating your work
- ## Writing style
- ## apply_patch
- ## manage_context
- ## update_plan

### Set comparison results
#### Only in config block
- ## Before you end your turn:
- ## Continual scratchpad protocol
- ## Research discipline (global)
- ## Sub-agents

#### Only in base file
- ## AGENTS.md spec
- ## Ambition vs. precision
- ## Autonomy and Persistence
- ## Bug fixes (get B, not band-aids)
- ## Editing constraints
- ## Error handling (no gymnastics)
- ## Frontend tasks (avoid AI slop)
- ## Personality
- ## Planning
- ## Presenting your work
- ## Shell commands
- ## Special user requests
- ## Task execution
- ## Validating your work
- ## Writing style
- ## apply_patch
- ## manage_context
- ## update_plan

#### Common headings
- (none)
