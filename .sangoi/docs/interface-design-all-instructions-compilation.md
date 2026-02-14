# Interface Design — Unified Instruction Set (Deduplicated)

## Deduplication Scope
### Source repositories and files


### Referenced content included in this same file
#### Local referenced file
- LICENSE (from upstream interface-design repository root: `/tmp/interface-design-VO7IK1/LICENSE`)

### External reference normalization
#### External web target found in source docs
- interface-design.dev/examples.html

#### Normalization policy
- External URL references are preserved as inert text labels only.
- No active markdown links, HTML anchors, or protocol URLs are kept.

## Redundancy Removal Policy
### Comparison baseline used for pruning
#### Global instruction stack in this environment
- /home/lucas/.codex/config.toml (`developer_instructions` block)
- /home/lucas/.codex/sangoi_base_instructions_jules.md

### What was removed as redundant
#### Generic agent-operation overlaps removed from this file
- Generic planning/process governance already enforced globally.
- Generic “ask before guessing” workflow wording that does not add interface-design specifics.
- Generic “do not narrate internal mode” wording duplicated by global behavior rules.

### What was retained
#### Interface-design-specific rules retained in full intent
- Product-domain UI scope restrictions.
- Direction establishment workflow and design rationale requirements.
- Per-component checkpoint contract.
- Craft principles and depth/spacing/token systems.
- Command-specific behavior (`init`, `status`, `audit`, `extract`, `critique`).
- Memory and validation rules for `.interface-design/system.md`.
- Template schema and example system presets.
- Clarification and communication constraints only where they are explicit parts of interface-design workflow (not as global agent-policy restatements).

## Coverage and Pruning Ledger
### Rule-level traceability (source to retained/pruned output)
#### README.md
- Retained in output: scope, installation, command list, flow with/without `system.md`, direction families, naming migration.
- Pruned as redundant: repeated generic framing copy and duplicate command descriptions already covered in command-spec sections.

#### .claude/skills/interface-design/SKILL.md
- Retained in output: intent-first contract, domain exploration payload, checkpoint contract, craft tests, workflow expectations, memory persistence behavior.
- Pruned as redundant: generic process narration rules duplicated by global instruction stack unless directly tied to interface-design behavior.

#### .claude/skills/interface-design/references/principles.md
- Retained in output: token architecture, surface hierarchy, depth strategy rules, spacing/radius/typography/data/motion/navigation/dark-mode principles.
- Pruned as redundant: repeated examples that duplicate already-retained principle statements.

#### .claude/skills/interface-design/references/validation.md
- Retained in output: when to save patterns, what not to save, reuse-first rule, consistency validation checklist.
- Pruned as redundant: repeated motivational prose.

#### .claude/skills/interface-design/references/critique.md
- Retained in output: critique dimensions and anti-hack constraints.
- Pruned as redundant: repeated critique framing text.

#### .claude/skills/interface-design/references/example.md
- Retained in output: practical depth/surface interpretation represented in principle and preset sections.
- Pruned as redundant: duplicative stylistic examples already captured by retained rules.

#### .claude/commands/init.md
- Retained in output: required initialization behavior, intent-first enforcement, checkpoint expectation, save-offer requirement.
- Pruned as redundant: duplicated command preamble text.

#### .claude/commands/status.md
- Retained in output: expected status output with and without `system.md`.
- Pruned as redundant: implementation boilerplate.

#### .claude/commands/audit.md
- Retained in output: audit scope, violation categories, expected reporting behavior.
- Pruned as redundant: repeated usage boilerplate.

#### .claude/commands/extract.md
- Retained in output: extraction targets, inference flow, create/customize prompt behavior.
- Pruned as redundant: repeated extraction framing examples not adding new rules.

#### .claude/commands/critique.md
- Retained in output: post-build critique focus and delivery behavior.
- Pruned as redundant: repeated critique framing boilerplate.

#### reference/system-template.md
- Retained in output: canonical `system.md` structure and decision-log contract.
- Pruned as redundant: template formatting scaffolding duplicated by retained schema summary.

#### reference/examples/system-precision.md
- Retained in output: Precision & Density preset characteristics, token profile, pattern tendencies, rationale pattern.
- Pruned as redundant: low-level duplicate values already represented in preset summary.

#### reference/examples/system-warmth.md
- Retained in output: Warmth & Approachability preset characteristics, token profile, pattern tendencies, rationale pattern.
- Pruned as redundant: low-level duplicate values already represented in preset summary.

#### LICENSE
- Retained in output: full upstream interface-design MIT license text.
- Pruned as redundant: none.

### Baseline anchors used for redundancy pruning decisions
#### Config overlay anchors
- `## Research discipline (global)`
- `## Continual scratchpad protocol`
- `## Sub-agents`
- `## Before you end your turn:`

#### Base instruction anchors
- `## Personality`
- `## Planning`
- `## Editing constraints`
- `## Presenting your work`
- `## Validating your work`

## Product Scope
### Intended use
#### Applicable UI domains
- Dashboards
- Admin panels
- Product applications
- Internal tools
- SaaS interfaces

### Out-of-scope use
#### Excluded target
- Marketing/landing-page design workflows

## Core Outcome Contract
### Three expected outcomes
#### Craft
- UI decisions must be intentional and product-contextual.
- Visual result must avoid generic/default look.

#### Memory
- Decisions are captured in `.interface-design/system.md` for reuse.

#### Consistency
- Components follow one coherent system across sessions.

## Installation and Activation
### Plugin path (recommended)
#### Steps
- Add marketplace: `/plugin marketplace add Dammyjay93/interface-design`
- Open plugin menu: `/plugin menu`
- Select plugin: `interface-design`
- Restart Claude Code

### Manual path (advanced)
#### Steps
- Clone repository locally
- Copy `.claude/*` to `~/.claude/`
- Copy `.claude-plugin/*` to `~/.claude-plugin/`
- Restart Claude Code

## Command Surface
### Command list
#### Operational commands
- `/interface-design:init`
- `/interface-design:status`
- `/interface-design:audit`
- `/interface-design:audit <path>`
- `/interface-design:extract`
- `/interface-design:extract <path>`
- `/interface-design:critique`

## Session Workflow
### Startup sequence
#### Always do first
- Load skill guidance and principles.
- Determine whether `.interface-design/system.md` exists.

#### If system file exists
- Read system definitions.
- Reuse established patterns.
- Keep consistency with declared tokens and patterns.

#### If system file does not exist
- Assess context and intended product feel.
- Propose one direction with rationale.
- Get confirmation before implementation.
- Establish initial tokens/patterns and offer to save.

### Intent-first checkpoint
#### Required pre-build questions
- Who is the real user in this moment/context?
- What exact task must they complete?
- What should the interface feel like?

#### Fail-loud rule
- If answers are vague, pause and request clarification.

### Direction proposal requirement
#### Required proposal payload
- Domain concepts from this product’s world.
- Color world grounded in the domain.
- One signature element unique to the product.
- Explicit defaults to avoid.
- One recommended direction tied to those findings.

## Mandatory Component Checkpoint
### Every component must declare
#### Technical design declaration
- Intent
- Palette
- Depth
- Surfaces
- Typography
- Spacing

### Decision quality rule
#### Non-default standard
- Every choice must have specific rationale.
- “Common” or “it works” is not sufficient rationale.

## Craft Validation Gates
### Pre-presentation checks
#### Swap test
- If replacing typeface/colors leaves the UI effectively unchanged, identity is too generic.

#### Squint test
- At low-detail perception, hierarchy and composition should still read clearly.

#### Signature test
- At least one product-specific design signal must remain visible.

#### Token test
- Values should come from one coherent token system.

## Design System Principles
### Token architecture
#### Rule
- Use stable primitive tokens and semantic aliases.
- Avoid random one-off values.

### Surface model
#### Rule
- Define clear elevation layers:
  - base canvas
  - cards
  - overlays/popovers/modals
- Use subtle, controlled differences between layers.

### Depth strategy
#### Rule
- Choose one primary depth strategy and keep it consistent:
  - Borders-only
  - Subtle shadows
  - Layered shadows
  - Surface lightness shifts

### Border behavior
#### Rule
- Borders must support structure without visual noise.
- Keep contrast progression subtle and consistent.

### Spacing system
#### Rule
- Choose a base unit.
- Keep values on the scale grid.
- Avoid arbitrary spacing values.

### Padding coherence
#### Rule
- Preserve predictable internal spacing logic.
- Exceptions require explicit content-driven justification.

### Radius system
#### Rule
- Use a small, consistent radius scale.
- Apply by component role/category.

### Typography system
#### Rule
- Define role-based levels (headline, body, label, metadata, data).
- Use more than size alone (weight/spacing/contrast).

### Data typography
#### Rule
- Use monospace and tabular numbers where numeric alignment matters.

### Controls and inputs
#### Rule
- Prefer styleable controls for polished UI consistency.
- Avoid brittle native controls when they break visual/system coherence.

### State completeness
#### Required states
- default
- hover
- active
- focus
- disabled
- loading
- empty
- error

### Motion constraints
#### Rule
- Motion should clarify state and hierarchy changes.
- Keep transitions quick and purposeful.

### Navigation context
#### Rule
- Users should always know where they are and next available actions.

### Dark mode adaptation
#### Rule
- Keep structural logic equivalent to light mode.
- Tune contrast and layering intentionally; do not just invert values.

## Critique Loop
### Mandatory quality pass
#### Evaluate
- Composition rhythm and focal hierarchy.
- Craft details (grid, spacing, typography, surfaces, states).
- Content sequencing and scanability.
- Structural implementation quality.

### Anti-hack constraints
#### Avoid as design substitutes
- Negative-margin patching
- `calc()` band-aids for fundamental spacing/layout logic
- Arbitrary absolute-position escapes

### Release rule
#### Before presenting
- If critique finds quality gaps, iterate first.

## Memory and System Evolution
### When to add patterns
#### Add to `.interface-design/system.md` when
- Pattern appears in 2+ places.
- Pattern is reusable project-wide.
- Pattern parameters are stable and worth standardizing.

### What not to save
#### Exclude
- One-off components
- Temporary experiments
- Variants better handled by props

### Reuse-first rule
#### Before creating new variants
- Check existing pattern inventory first.
- Extend known patterns instead of duplicating near-identical ones.

### Consistency validation checklist
#### Against current system
- Spacing follows declared grid.
- Depth usage matches chosen strategy.
- Colors stay within declared palette.
- Components reuse documented patterns.

## Command Behavior Specification
### `interface-design:init`
#### Purpose
- Start implementation under interface-design discipline.

#### Required behavior
- Read skill guidance first.
- Enforce intent-first setup.
- Run component checkpoint before each component implementation.
- Use system file when available; otherwise establish direction then confirm.
- Offer to save new patterns at end of task.

### `interface-design:status`
#### Purpose
- Display current design system status.

#### If system exists
- Show direction (personality, foundation, depth).
- Show token summary (spacing, radius, colors).
- Show key reusable patterns.
- Show recent update context.

#### If system missing
- Report no system found.
- Suggest next steps (`build` path or `extract` path).

### `interface-design:audit`
#### Purpose
- Detect violations against declared system.

#### Coverage
- Spacing off-grid values
- Depth strategy violations
- Palette drift
- Pattern drift across components

#### Output expectation
- Report file-level violations.
- Provide actionable fix guidance.

### `interface-design:extract`
#### Purpose
- Infer system from existing code patterns.

#### Extraction targets
- Repeated spacing values
- Radius scales
- Button patterns
- Card patterns
- Dominant depth strategy

#### End behavior
- Propose a generated system.
- Ask whether to create/customize `.interface-design/system.md`.

### `interface-design:critique`
#### Purpose
- Perform a post-build improvement pass.

#### Focus areas
- Composition
- Craft details
- Content clarity
- Structural implementation quality

#### Delivery behavior
- Return improved UI result.
- Do not expose internal process narration.

## System File Contract
### Canonical structure of `.interface-design/system.md`
#### Direction
- Personality
- Foundation
- Depth

#### Tokens
- Spacing (base + scale)
- Colors
- Radius
- Typography

#### Patterns
- Reusable component definitions with dimensions, treatment, and usage context.

#### Decisions log
- Decision statement
- Rationale
- Date

## Direction Families
### Supported direction catalog
#### Family list
- Precision & Density
- Warmth & Approachability
- Sophistication & Trust
- Boldness & Clarity
- Utility & Function
- Data & Analysis

## Preset Example System — Precision & Density
### Direction
#### Characteristics
- Personality: Precision & Density
- Foundation: Cool (slate)
- Depth: Borders-only

### Tokens
#### Spacing
- Base: 4px
- Scale: 4, 8, 12, 16, 24, 32

#### Colors
- foreground: slate-900
- secondary: slate-600
- muted: slate-400
- faint: slate-200
- border: rgba(0, 0, 0, 0.08)
- accent: blue-600

#### Radius
- 4px, 6px, 8px

#### Typography
- system-ui stack
- scale optimized for dense interfaces
- mono stack for numeric data

### Patterns
#### Button
- compact height
- compact horizontal padding
- small radius
- border-led treatment

#### Card
- faint border
- compact padding
- no shadow

#### Table cell
- compact cell padding
- tabular numeric typography
- faint row separators

### Decisions
#### Rationale pattern
- prioritize density and clarity for power users
- prefer border depth over decorative lift
- keep performance-oriented typography choices

## Preset Example System — Warmth & Approachability
### Direction
#### Characteristics
- Personality: Warmth & Approachability
- Foundation: Warm (stone)
- Depth: Subtle shadows

### Tokens
#### Spacing
- Base: 4px
- Scale skewed toward more generous spacing

#### Colors
- warm neutral text scale
- accent in warm hue family
- subtle shadow token for lift

#### Radius
- 8px, 12px, 16px

#### Typography
- approachable, readability-focused scale

### Patterns
#### Button
- taller comfortable control height
- generous horizontal padding
- subtle elevation

#### Card
- soft radius
- generous padding
- subtle shadow surface

#### Input
- comfortable control height
- balanced padding
- gentle border emphasis

### Decisions
#### Rationale pattern
- favor comfort and approachability over density
- use gentle depth and warmer palette

## Migration Notes
### Naming transition
#### Historical note
- Older setups may refer to `frontend-design`.
- Current naming is `interface-design`.

### Practical migration actions
#### Update points
- Use command namespace `interface-design:*`.
- Keep local folder naming aligned with `.interface-design`.

## Condensed Operator Checklist
### Before implementation
#### Must complete
- Clarify user/task/feel context.
- Propose and confirm direction.
- Choose one coherent system strategy.

### During implementation
#### Must enforce
- Component checkpoint contract.
- Token/spacing/depth consistency.
- Full state coverage.

### Before handoff
#### Must enforce
- Critique pass completed.
- Weaknesses fixed.
- Offer pattern persistence to `.interface-design/system.md`.


