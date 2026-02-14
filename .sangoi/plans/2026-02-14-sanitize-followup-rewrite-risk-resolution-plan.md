# Plano — correção do risco de idle falso no `/sanitize`

Complexidade: hard

## Escopo
- `codex-rs/core/src/tasks/sanitize.rs`
- validação de integração com `manage_context` sem alterar contrato público.

## Problema alvo
- O loop de `/sanitize` calcula eventos novos por slice de índice (`history_len_before_request..`).
- Quando `manage_context.apply` reescreve o history, o comprimento pode encolher e o slice vira vazio.
- Isso pode gerar idle falso e disparar `Stalled` prematuramente.

## Estratégia
1. Trocar detecção de eventos de follow-up para delta por `call_id` de outputs de `manage_context` entre snapshots `before` e `after`.
2. Manter parser atual de `retrieve/apply`, apenas mudando a coleta dos itens candidatos.
3. Cobrir com teste de regressão onde `after.len() < before.len()` e mesmo assim há evento novo detectável.

## Checklist
- [x] Implementar helper `manage_context_follow_up_events_since(before, after)` em `sanitize.rs`
- [x] Substituir uso de slice por índice no loop por novo helper
- [x] Adicionar teste de regressão de rewrite com histórico menor
- [x] Adicionar teste para ignorar outputs já existentes e captar só novos call_ids
- [x] Rodar `just fmt`
- [x] Rodar `cargo test -p codex-core --lib sanitize -- --test-threads=1`
- [x] Rodar `cargo build -p codex-core`

## Done criteria
- `/sanitize` não depende mais de `history_len_before_request` para detectar follow-up de `manage_context`.
- Cenário de rewrite com histórico menor continua detectando evento novo corretamente.
- Testes de `sanitize` passam e build de `codex-core` passa.
