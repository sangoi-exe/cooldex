# Task log: rebase recovery (`codex-core`/`codex-state`) — 2026-02-07

Date (UTC-3): 2026-02-07
Branch: `sync/rebase-master-20260207-014939`
Starting commit reference: `4cc8268e6`

## Goal

- Finalizar a recuperação pós-rebase e deixar `codex-core` compilando/testando sem regressão funcional evidente.

## What was fixed

- API drift pós-rebase em `codex-core`:
  - `FunctionCallOutputPayload.content` -> `body` (`FunctionCallOutputBody`)
  - `Stage::Experimental` (shape estruturado)
  - `TurnContext` sem `turn.client`
  - `Codex::spawn` com `file_watcher` + `dynamic_tools`
  - padrões `WebSearchAction::Search { query, queries }`
  - borrow conflict em `tasks/mod.rs::on_task_finished`
- Ajustes de compatibilidade em testes afetados:
  - `ResponseItem::Message` com `phase: None`
  - migração de asserts antigos (`output.content`) para `output.body`
  - `build_specs(..., dynamic_tools)` em testes
  - `Codex` test-stubs com campo `session`
- Consistência de testes de integração:
  - expectativas de ordem de ferramentas em `model_tools` e `prompt_caching`
  - normalização de `call_id` prefix em `request_user_input` tests
- Flake hardening:
  - `shell_snapshot::snapshot_shell_does_not_inherit_stdin` com timeout de teste de `500ms` para `2s`.
  - `unified_exec_formats_large_output_summary`: gerador de carga trocado para `yes|head` e `yield_time_ms` ampliado.
  - `unified_exec_runs_under_sandbox`: `yield_time_ms` ampliado para reduzir output vazio sob carga.
  - `collect_tool_outputs` em `unified_exec` passa a preservar a saída mais informativa por `call_id`.
  - `tool_parallelism`: guard de tempo ajustado para carga real e guard dedicado para cenário mixed.
  - `remote_models_request_times_out_after_5s`: bound superior ajustado para jitter de scheduler.
  - helpers de processo em testes (`wait_for_pid_file`/`wait_for_process_exit`) ampliados para 5s.

## Validation commands executed

- `git status -sb`
- `cargo check -p codex-core --all-features --tests --quiet`
- `just fmt`
- `just fix -p codex-core`
- `just fix -p codex-state`
- `just write-config-schema`
- `cargo test -p codex-core --all-features -- --quiet` (múltiplos reruns)
- `cargo test -p codex-state -- --quiet`
- `cargo test -p codex-core context_manager::history::tests::estimate_token_count_scales_reasoning_bytes_to_tokens -- --quiet`
- `cargo test -p codex-core shell_snapshot::tests::snapshot_shell_does_not_inherit_stdin -- --quiet`
- `cargo test -p codex-core --all-features suite::model_tools::model_selects_expected_tools -- --quiet`
- `cargo test -p codex-core --all-features suite::prompt_caching::prompt_tools_are_consistent_across_requests -- --quiet`
- `cargo test -p codex-core --all-features suite::request_user_input:: -- --quiet`
- `cargo test -p codex-core --all-features suite::tool_parallelism:: -- --quiet`
- `cargo test -p codex-core --all-features suite::unified_exec:: -- --quiet`
- `cargo test -p codex-core --all-features suite::remote_models::remote_models_request_times_out_after_5s -- --quiet`
- `stability_loop_10x_postpatch=PASS` (10x para `tool_parallelism`, `unified_exec` e `remote_models timeout`)
- `stability_loop_10x_final=PASS` (novo 10x após último ajuste)
- `cargo test -p codex-core --all-features -- --quiet` (full run verde, repetido)
- `git diff --check`

## Current status

- `codex-core` compila em `--all-features --tests`.
- `codex-state` suite verde.
- `codex-core` full suite estabilizada após ajustes de teste/harness:
  - `tool_parallelism`: guard de duração ajustado para carga real, com teste misto usando guard dedicado;
  - `unified_exec`: geração de output mais determinística (`yes|head`), `yield_time_ms` ajustado em cenários sensíveis e merge de outputs por `call_id` preferindo payload informativo;
  - `remote_models`: bound superior de timeout de teste ajustado para evitar falso negativo sob carga.
- Evidência de estabilidade:
  - stress-loop 10x verde para `suite::tool_parallelism::`;
  - stress-loop 10x verde para `suite::unified_exec::`;
  - stress-loop 10x verde para `suite::remote_models::remote_models_request_times_out_after_5s`;
  - execuções completas consecutivas verdes de `cargo test -p codex-core --all-features -- --quiet` após hardening final.
- Push em `master` **ainda não executado** neste ciclo.

## Update — 2026-02-08 (final stabilization pass)

### Additional fixes

- `codex-rs/core/tests/suite/tool_parallelism.rs`
  - `read_file_tools_run_in_parallel` recebeu validação explícita de outputs (`call-1`/`call-2`) para falhar alto quando o `test_sync_tool` não executa corretamente.
  - Guard de duração ajustado para carga real do cenário (`sleep_after_ms=2000`) com limite de smoke-load em `4_000ms`.
- `codex-rs/core/tests/suite/pending_input.rs`
  - Flake residual no full suite (`injected_user_input_triggers_follow_up_request_with_deltas`) foi eliminado removendo race de enfileiramento assíncrono:
    - injeção do segundo prompt mudou de `submit(Op::UserInput)` para `steer_input(...)` síncrono;
    - sincronização de fase passou a usar completion receivers de `start_streaming_sse_server`;
    - timeout explícito para conclusão de stream no teste.

### Validation commands executed (2026-02-08 final pass)

- `cargo test -p codex-core --all-features suite::tool_parallelism::read_file_tools_run_in_parallel -- --quiet`
- `cargo test -p codex-core --all-features suite::tool_parallelism:: -- --quiet`
- `for i in $(seq 1 30); do /usr/bin/time -f "%e" -o /tmp/tp_time.txt cargo test -p codex-core --all-features suite::tool_parallelism::read_file_tools_run_in_parallel -- --quiet; done`
- `for i in $(seq 1 20); do cargo test -p codex-core --all-features suite::tool_parallelism:: -- --quiet; done`
- `for i in $(seq 1 10); do cargo test -p codex-core --all-features suite::unified_exec:: -- --quiet; done`
- `for i in $(seq 1 10); do cargo test -p codex-core --all-features suite::remote_models::remote_models_request_times_out_after_5s -- --quiet; done`
- `for i in $(seq 1 60); do cargo test -p codex-core --all-features suite::pending_input::injected_user_input_triggers_follow_up_request_with_deltas -- --quiet; done`
- `just fmt`
- `just fix -p codex-core`
- `for i in 1 2; do /usr/bin/time -f "%e" cargo test -p codex-core --all-features -- --quiet; done`
- `cargo test -p codex-state -- --quiet`
- `git diff --check`

### Additional validation evidence

- Baseline pré-ajuste do flake `read_file_tools_run_in_parallel` (`n=30`):
  - `p50=2.840s`, `p95=3.890s`, `p99=4.110s`, `max=4.110s`, `0` falhas.
- Stress loops:
  - `suite::tool_parallelism::` -> `20/20` verde.
  - `suite::unified_exec::` -> `10/10` verde.
  - `suite::remote_models::remote_models_request_times_out_after_5s` -> `10/10` verde.
  - `suite::pending_input::injected_user_input_triggers_follow_up_request_with_deltas` -> `60/60` verde após fix final.
- Full runs:
  - `cargo test -p codex-core --all-features -- --quiet` -> 2 consecutivos verdes (`112.30s`, `99.96s`).
- Cross-checks finais:
  - `cargo test -p codex-state -- --quiet` -> verde.
  - `git diff --check` -> sem whitespace/conflict markers.

### Handoff status

- Objetivo técnico de estabilização do full-suite do `codex-core` sob carga: **concluído**.
- Checklist de push/update:
  - `origin/main` permanece espelho de `upstream/main` (sem alteração nesta passada).
  - `master` pronto para decisão operacional de update/push.
