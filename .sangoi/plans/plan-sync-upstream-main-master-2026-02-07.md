# Plan — sync `upstream/main` → `main` → `master` (2026-02-07)

Plan level: **hard** (working tree suja + `force-with-lease` em `origin/main` + merge/cherry-pick).

## Objetivo

Executar o guia oficial `.sangoi/docs/guia-sync-upstream-main-master.md` sem perder modificações locais.

## Checklist

- [x] Confirmar pré-requisitos (`origin`/`upstream`, branch atual, estado sujo).
- [x] Criar backups (`backup/main-before-upstream-sync-*`, `backup/master-before-upstream-sync-*`).
- [x] Salvar WIP em branch dedicada com commit.
- [x] Atualizar `main` para refletir `upstream/main` e publicar `origin/main`.
- [x] Mesclar `main` em `master` e validar ancestrais/divergência.
- [x] Reaplicar WIP via `cherry-pick` em `master`.
- [x] Rodar `just fmt` + testes dos crates afetados.
- [ ] Publicar `origin/master`.
- [x] Registrar validações finais e estado do sync.

## Status update — 2026-02-07 (rebase recovery)

- `codex-core` voltou a compilar em `--all-features --tests` após migração de contratos (`FunctionCallOutputPayload.body`, `TurnContext`, `Codex::spawn`).
- `just fmt` + `just fix -p codex-core` + `just fix -p codex-state` executados.
- `cargo test -p codex-state -- --quiet` verde.
- `cargo test -p codex-core --all-features -- --quiet`:
  - estabilizado após hardening de testes/harness;
  - 2 execuções completas consecutivas verdes em carga total.
- Stress loops pós-fix:
  - `suite::tool_parallelism::` 10/10 verde;
  - `suite::unified_exec::` 10/10 verde;
  - `suite::remote_models::remote_models_request_times_out_after_5s` 10/10 verde.
- Push em `master` permanece pendente apenas de decisão operacional (não mais bloqueado por flakiness conhecida).

## Fan-out / fan-in

- Senior Plan Advisor: revisão implacável do plano antes da execução.
- Senior Code Reviewer: revisão implacável por item concluído que alterar código/config/docs/testes.

## Addendum — estabilização residual `codex-core` (2026-02-08)

Plan level: **hard + time-consuming**.

### Objetivo

Eliminar flakes residuais em `suite::tool_parallelism::read_file_tools_run_in_parallel` e `suite::pending_input::injected_user_input_triggers_follow_up_request_with_deltas`, sem perder poder de detecção de regressões, e fechar handoff seguro para update/push de `master`.

### Checklist

- [x] Confirmar baseline da falha residual (execução isolada + suíte `tool_parallelism`) sem alterar código.
- [x] Coletar distribuição de duração do teste flake (`n=30`) antes de mexer no guard.
- [x] Definir critério explícito de guard com discriminação paralelo vs serial (sem overfitting).
- [x] Aplicar ajuste mínimo em `codex-rs/core/tests/suite/tool_parallelism.rs`.
- [x] Rodar `just fmt` e `just fix -p codex-core`.
- [x] Rodar stress loops: `tool_parallelism` (20x), `unified_exec` (10x), `remote_models timeout` (10x).
- [x] Estabilizar flake residual em `suite::pending_input::injected_user_input_triggers_follow_up_request_with_deltas`.
- [x] Rodar full `cargo test -p codex-core --all-features -- --quiet` até 2 runs consecutivos verdes.
- [x] Confirmar `cargo test -p codex-state -- --quiet` e `git diff --check`.
- [x] Atualizar logs `.sangoi` com evidências finais e checklist de handoff/push.

### Status final do addendum (2026-02-08)

- `tool_parallelism` estabilizado com guard de carga real + validação explícita de outputs de tool call.
- `pending_input` estabilizado removendo race de injeção assíncrona (`submit(UserInput)`) em favor de `steer_input` síncrono no teste, com sincronização por completion receivers do streaming server.
- Full `codex-core --all-features` validado em 2 execuções consecutivas verdes após os ajustes finais.
- `codex-state` verde e `git diff --check` limpo.
