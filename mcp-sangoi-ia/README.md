# Sangoi Codex Service

Dedicated Sangoi-facing Codex service.

This package exists to give `sangoi-ia` a clean backend -> service -> Codex seam without depending on the current `mcp-standalone` hack.

## Scope

Current scope:
- `POST /api/sangoi/v1/parse/urban-info`
- `GET /healthz`

The first route parses `informacoes urbanisticas`-style text into structured fields that Sangoi can consume without piling on brittle regex.

## Principles

- domain-first, not generic
- structured output only
- validate request and response locally
- no dependence on `mcp-standalone`
- no normative/legal decision-making in this service

## Quick start

```bash
cd /home/lucas/work/codex/mcp-sangoi-ia
npm install
cp .env.example .env
npm run dev
```

## Environment

See `.env.example`.

Important knobs:
- `SANGOI_CODEX_RUNTIME_MODE`
- `SANGOI_CODEX_MCP_HOST`
- `SANGOI_CODEX_MCP_PORT`
- `SANGOI_CODEX_MODEL`
- `SANGOI_CODEX_TIMEOUT_MS`
- `SANGOI_CODEX_WORKDIR`

Runtime modes:

- `dev`: current local test path; the service expects `codex` on `PATH`, typically from the operator's Rust-built local Codex checkout. This mode is local-operator-only and is not a silent fallback for non-local/containerized/shared launches.
- `prod`: placeholder only for now; the intended future path is a production-owned Codex runtime plus CLI-native auth retrieval from backend/JWT-backed state. The current service fails loud at startup if `prod` is selected.

<!-- Merge-safety anchor: this service consumes the Codex CLI account/auth runtime through `codex exec`; it must not become a second account or provider-token owner. -->

`SANGOI_CODEX_RUNTIME_MODE` must always be set explicitly. Copying `.env.example` into a local `.env` is the supported local convenience path. Launches that do not set runtime mode must fail closed instead of silently inheriting local `dev`.

`CODEX_API_KEY`, `CODEX_HOME`, and other Codex auth/config are inherited from the host environment in `dev`; account roster, active-account selection, leases, usage, autoswitch, and request-auth derivation stay owned by the local Codex CLI runtime. This service does not read `auth.json`, carry provider tokens, or select active ChatGPT accounts itself. Do not treat inherited dev auth/config as the final production auth model.

Even in future `prod`, `CODEX_HOME` remains writable and owns sessions, logs, skills, caches, and rollout state. It is not the final source of truth for provider auth, and file-backed `auth.json` is not the intended production truth.

Planned production placeholders:
- `SANGOI_CODEX_PROD_AUTH_MODE`
- `SANGOI_CODEX_PROD_AUTH_BACKEND_URL`
- `SANGOI_CODEX_PROD_RUNTIME_JWT`

In that future production path, the runtime JWT authenticates the Codex runtime to the auth backend; provider tokens stay in the backend store rather than inside the JWT.

## Request example

```json
{
  "documentId": "document_123",
  "fileName": "informacoes-urbanisticas.pdf",
  "mimeType": "application/pdf",
  "extractedText": "INFORMACOES URBANISTICAS\nZona: 12.d\nRecuo frontal: 4,00 m\nAltura: COMAR\nAltura ate 14,00 m: dispensado; acima disso H/7\nTO 65%\nIA 1,10\nUso residencial unifamiliar/bifamiliar permitido",
  "projectType": "obra_nova_unifamiliar_bifamiliar"
}
```

## Response example

```json
{
  "parser": "codex-exec",
  "durationMs": 12345,
  "result": {
    "status": "complete",
    "cadastralSupportProfile": "urban_info",
    "supportLevel": "structured",
    "zoneCode": "12.d",
    "frontSetbackMeters": 4,
    "sideSetbackRule": null,
    "heightRule": "COMAR",
    "occupancyIndex": 0.65,
    "floorAreaIndex": 1.1,
    "residentialUseSignal": true,
    "evidenceSnippets": [
      "Zona: 12.d",
      "Recuo frontal: 4,00 m",
      "TO 65%",
      "IA 1,10"
    ],
    "warnings": []
  }
}
```
