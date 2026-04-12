# AGENTS.md

## Purpose

This folder owns the dedicated Sangoi Codex service.

It is intentionally domain-first and narrow:
- no dependency on `mcp-standalone`
- no generic bridge ambitions
- no NetSuite-specific runtime or session model
- no silent fallback to regex heuristics in this service

The first shipped capability is bounded parsing of Santa Maria `informacoes urbanisticas`-style support text through `codex exec` with a strict JSON schema.

## Key files

- `package.json`: standalone package surface and scripts.
- `src/index.ts`: Fastify app and HTTP routes.
- `src/config.ts`: environment contract.
- `src/contracts.ts`: request/result schemas and JSON schema exported to Codex.
- `src/codex-exec.ts`: dedicated child-process wrapper around `codex exec`.
- `src/prompts.ts`: Sangoi-specific parsing prompts.
- `.env.example`: local service env contract.

## Durable notes

- Keep this service dedicated to Sangoi. If a new capability is needed, add a new bounded route and prompt/schema pair instead of turning this package into a generic Codex session bridge.
- `codex exec` is the runtime seam here. Do not import or mirror logic from `mcp-standalone`.
- Current runtime split is explicit: `dev` uses the operator's local Codex CLI, while `prod` remains placeholder-only until the CLI grows its backend/JWT-backed auth retrieval path.
- `SANGOI_CODEX_RUNTIME_MODE` must be explicit. Missing mode must fail closed instead of silently inheriting local `dev`.
- In that future `prod` path, runtime JWT authenticates this service/CLI to the auth backend; provider tokens remain backend-owned and are not carried in the JWT.
- Even in future `prod`, writable `CODEX_HOME` still owns sessions, logs, skills, caches, and rollout state. Final provider auth truth must not regress back to file-backed `auth.json`.
- Structured output is mandatory. The service must validate Codex output again after the CLI returns.
- Parsing support documents is allowed here; normative truth still belongs to the official Santa Maria corpus and Sangoi's deterministic/runtime rules.

## Review metadata

- Last reviewed: 2026-04-12
