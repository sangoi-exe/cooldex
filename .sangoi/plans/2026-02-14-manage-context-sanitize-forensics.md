# Plan — manage_context + /sanitize Forensics (complex)

## Objective
Produzir um levantamento completo, auditável e acionável de **tudo** que se relaciona a `manage_context` e `/sanitize` no estado atual (funcional), incluindo funcionamento interno, contratos, integrações, e análise causal de por que a versão nova quebrou.

## Scope
- Repositório: `/home/lucas/work/codex`
- Base atual (funcional): `HEAD` detached em `ee8f56eb0`
- Base de comparação (regressão): branch `reapply/accounts-20260209` (commit `8418fe049...`)

## Lanes (fan-out)
- [ ] Lane A — Arquitetura + Inventário estático (owner: explorer-A)
  - Touch: `codex-rs/core/src/tools/handlers/manage_context.rs`, `codex-rs/core/src/tasks/sanitize.rs`, `codex-rs/core/src/codex.rs`, `codex-rs/core/src/tools/spec.rs`, `codex-rs/core/src/client_common.rs`, `codex-rs/core/sanitize_prompt.md`, `codex-rs/core/src/config/*`, `codex-rs/core/src/tasks/mod.rs`, testes relevantes.
  - Done criteria: mapa de símbolos/funções/tipos/callsites com linhas.
  - Verification commands:
    - `rg -n "manage_context|sanitize" codex-rs/core/src codex-rs/core/tests`

- [ ] Lane B — Fluxo runtime `/sanitize` end-to-end (owner: explorer-B)
  - Touch: `codex-rs/core/src/codex.rs`, `codex-rs/core/src/tasks/sanitize.rs`, prompt e configuração.
  - Done criteria: sequência exata do comando `/sanitize` até task + loop + parada + mensagens ao usuário.
  - Verification commands:
    - `rg -n "spawn_sanitize_task|SanitizeTask|sanitize_first_turn_reasoning|task_complete|stalled" codex-rs/core/src`

- [ ] Lane C — Contrato `manage_context` (owner: explorer-C)
  - Touch: `codex-rs/core/src/tools/spec.rs`, `codex-rs/core/src/tools/handlers/manage_context.rs`, wrappers de output.
  - Done criteria: payloads aceitos, regras de validação, anti-drift/snapshot, efeitos em state, shape de resposta.
  - Verification commands:
    - `rg -n "create_manage_context_tool|snapshot_id|ops|allow_recent|consolidate_reasoning|handle_apply|handle_retrieve" codex-rs/core/src`

- [ ] Lane D — Testes + docs + governança (owner: explorer-D)
  - Touch: testes unit/integration/suite e docs de reapply/sanitize.
  - Done criteria: matriz de cobertura (o que está testado vs gaps), docs existentes e drift.
  - Verification commands:
    - `rg -n "manage_context|sanitize" codex-rs/core/tests codex-rs/core/src .sangoi/docs`

- [ ] Lane E — Diff forense funcional vs regressão (owner: explorer-E)
  - Touch: comparação `ee8f56eb0` ↔ `8418fe049` nos arquivos centrais.
  - Done criteria: tabela old/new por função, regressões introduzidas, causalidade técnica de falha.
  - Verification commands:
    - `git diff --word-diff -- codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs`

## Fan-in
- [ ] Consolidar resultados das lanes em relatório único:
  - Arquivo: `.sangoi/reports/2026-02-14-manage-context-sanitize-full-forensics.md`
  - Estrutura:
    1. Inventário completo (arquivos/símbolos)
    2. Fluxo runtime `/sanitize` (diagrama textual)
    3. Contrato detalhado `manage_context`
    4. Por que funciona no estado atual
    5. Por que quebrou no código novo
    6. Matriz de testes/gaps
    7. Checklist de verificação manual
    8. Recomendações de hardening (sem shims de compat)

## Final verification
- [ ] Validar referências/linhas citadas e consistência cruzada
  - Commands:
    - `rg -n "manage_context|sanitize" .sangoi/reports/2026-02-14-manage-context-sanitize-full-forensics.md`
    - `git status --short --branch`
