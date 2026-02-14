# Plan — implementar guias `guide-reapply` pendentes (2026-02-12)

Label: **complex**

## Escopo proposto
- Implementar os itens pendentes dos guias `guide-reapply` além de `/accounts` (já reaplicado e validado):
  - `guide-reapply-manage_context.md`
  - `guide-reapply-subagents.md`
  - `guide-reapply-sanitize.md`
- Manter fail-loud e sem commit/push nesta fase.
- Se Advisor/Reviewer apontarem ajustes de contrato, corrigir também os próprios guias para manter código e documentação alinhados.

## Decisão de execução (escolhida)
- Reaplicação manual por contrato/comportamento (sem cherry-pick cego).
- Validar cada módulo por etapa, com gate `passed > 0` nos filtros.
- Só avançar para a próxima etapa após green da etapa atual.

## Fan-out / fan-in (proposta)
- **Lane A:** `manage_context` + docs correspondentes.
- **Lane B:** sub-agents (`collab`/`agent status`) + formatter de status no `exec`.
- **Lane C:** `/sanitize` (task + slash routing + docs).
- **Fan-in:** rodada única de validação consolidada + auditoria de drift.

## Checklist
- [ ] 1) Confirmar escopo final dos guias
  - Done when: você confirma exatamente quais guias entram nesta rodada.
  - Verificação: confirmação explícita no chat.

- [ ] 2) Recon técnico por guia (gap atual vs contrato)
  - Done when: gaps por arquivo/função estão mapeados para `manage_context`, sub-agents e `/sanitize`.
  - Verificação:
    - `rg -n "collab|spawn_agent|wait\\(|subagent_instructions_file|developer_instructions" codex-rs/core/src/tools/handlers/collab.rs codex-rs/core/src/agent/control.rs codex-rs/core/src/agent/status.rs`
    - `rg -n "SlashCommand::Sanitize|sanitize" codex-rs/tui/src/slash_command.rs codex-rs/core/src/tasks`
    - `rg -n "manage_context|snapshot_id|consolidate_reasoning|replace|exclude" codex-rs/core/src`

- [ ] 3) Implementar `manage_context` (guides + contrato)
  - Done when: handler e contrato anti-drift/ops estão alinhados ao guia.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core manage_context -- --quiet`
    - `cd codex-rs && cargo test -p codex-core model_tools -- --quiet`

- [ ] 4) Implementar sub-agents (status/instruções/fail-loud)
  - Done when: spawn/wait/status e uso de instruções dedicadas batem com o guia.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core collab -- --quiet`
    - `cd codex-rs && cargo test -p codex-core agent::status -- --quiet`
    - `cd codex-rs && cargo test -p codex-core agent::control -- --quiet`
    - `cd codex-rs && cargo test -p codex-exec completed_status_message -- --quiet`

- [ ] 5) Implementar `/sanitize` (task + slash + docs)
  - Done when: `/sanitize` dispara task dedicada correta e documentação está coerente.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core sanitize -- --quiet`
    - `cd codex-rs && cargo test -p codex-core review -- --quiet`
    - `cd codex-rs && cargo test -p codex-tui slash_command -- --quiet`

- [ ] 6) Rodada final de qualidade e auditoria
  - Done when: `fmt` + diff audit + reviewer gate final estiverem verdes.
  - Verificação:
    - `cd codex-rs && just fmt`
    - `git diff --check --cached`
    - Senior Code Reviewer: `READY` ou `READY_WITH_NITS`

- [ ] 7) Sincronizar guias com feedback dos seniors
  - Done when: todo ajuste exigido por Advisor/Reviewer estiver refletido no código **e** nos guias `guide-reapply` relevantes.
  - Verificação:
    - `rg -n "manage_context|subagent|sanitize|fail-loud|passed > 0" .sangoi/docs/guide-reapply-*.md`
    - `git diff --cached -- .sangoi/docs/guide-reapply-*.md`
