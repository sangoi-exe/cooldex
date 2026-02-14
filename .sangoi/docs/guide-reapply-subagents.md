# Guia de reaplicação — sub-agents (status + instruções dedicadas)

## Objetivo funcional

Reaplicar os ajustes de sub-agents para reduzir drift:

- root agent enxerga status real de execução (`running/completed/errored`);
- `wait` retorna status útil para fan-in correto;
- sub-agent usa prompt dedicado de `subagent_instructions_file`;
- remover herança indevida de `developer_instructions`/bloat quando houver prompt dedicado.
- formatter de status `completed` preserva mensagem completa (sem truncamento silencioso).

## Fonte (backup)

- Branch: `backup/master-before-upstream-sync-20260207-003149`
- Arquivos:
  - `codex-rs/core/src/agent/control.rs`
  - `codex-rs/core/src/agent/status.rs`
  - `codex-rs/core/src/tools/spec.rs`
  - `codex-rs/core/src/tools/handlers/collab.rs`
  - `codex-rs/exec/src/event_processor_with_human_output.rs`
  - `.sangoi/sangoi_subagent_instructions.md`
  - `.sangoi/sangoi_base_instructions.md`

## Contrato de implementação

1. Status pipeline:
   - estados de agent mapeados a partir de eventos (`Running`, `Completed`, `Errored`, `Shutdown`);
   - consulta de status síncrona/observável por `get_status` + receiver.
2. Spawn config:
   - quando `subagent_instructions_file` estiver definido, usar esse conteúdo como base do sub-agent;
   - evitar herdar instruções globais que causam prompt-bloat e drift.
3. Tool surface:
   - manter contrato de `spawn_agent`, `send_input`, `wait`, `close_agent` coerente.
4. Human-readable status:
   - `AgentStatus::Completed(Some(...))` deve manter conteúdo integral (apenas trim), sem preview truncado.

## Status reaplicado (2026-02-12)

- `subagent_instructions_file` foi adicionado ao modelo de config/profile e carregado em `Config::subagent_base_instructions`.
- `build_agent_spawn_config` agora prioriza instruções dedicadas de sub-agent quando presentes.
- Com prompt dedicado de sub-agent:
  - `developer_instructions` e `user_instructions` são limpos;
  - `project_doc_max_bytes` é zerado;
  - `Feature::ChildAgentsMd` é desabilitada.
- Foi adicionado teste focado para garantir esse contrato no `collab.rs`.

## Status complementar (2026-02-13)

- Fixture de testes de `Config` foi alinhada com o novo campo
  `subagent_base_instructions` para remover blocker de compilação (`E0063`) em `codex-core`.
- Validação focada de sub-agents foi executada com escopo unit (`--lib`) para isolar contrato funcional de core:
  `collab`, `agent::status` e `agent::control`.
- Verificação complementar de formatter humano foi mantida em `codex-exec`:
  `completed_status_message`.

## Reaplicação manual (ordem)

1. Portar status/lifecycle (`agent/status.rs`, `agent/control.rs`).
2. Portar build de spawn config no `collab.rs` (instruções dedicadas).
3. Sincronizar fixtures/literais de `Config` em testes quando houver novo campo obrigatório.
4. Portar ajustes de spec/handlers para ferramentas de colaboração.
5. Validar testes unitários de `collab.rs` sobre instruções dedicadas.

## Comandos de apoio

```bash
git show backup/master-before-upstream-sync-20260207-003149:codex-rs/core/src/tools/handlers/collab.rs
git show upstream/main:codex-rs/core/src/tools/handlers/collab.rs
git diff upstream/main..backup/master-before-upstream-sync-20260207-003149 -- codex-rs/core/src/tools/handlers/collab.rs codex-rs/core/src/agent/control.rs codex-rs/core/src/agent/status.rs codex-rs/core/src/tools/spec.rs
```

## Validação (green)

```bash
cd codex-rs
cargo build -p codex-core
cargo build -p codex-exec
rg -n "subagent_base_instructions: None" core/src/config/mod.rs
```

Validação aprofundada (opcional, ambiente/harness permitindo):

```bash
cargo test -p codex-core --lib collab -- --quiet
cargo test -p codex-core --lib agent::status -- --quiet
cargo test -p codex-core --lib agent::control -- --quiet
cargo test -p codex-exec completed_status_message -- --quiet
cargo test -p codex-core model_tools -- --quiet
```

## Fail-loud / rollback

- Se status não refletir `running` enquanto agente executa, considerar regressão crítica (não seguir).
- Se sub-agent continuar herdando prompt global quando `subagent_instructions_file` estiver setado, bloquear merge da reaplicação.
- Se `completed` voltar a truncar mensagem no formatter humano, bloquear merge da etapa.
- Se surgir `E0063` por campo novo de `Config` em fixtures, bloquear avanço até sincronizar literais de teste.
- Rollback preferencial (não destrutivo):

```bash
git switch --detach <checkpoint-sha>
git switch -c reapply/recover-from-<checkpoint-sha>
```

- Apenas com decisão explícita de descarte local:
  `git reset --hard <checkpoint-sha>`.
