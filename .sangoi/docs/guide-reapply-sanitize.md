# Guia de reaplicação — `/sanitize` (v2)

## Objetivo

Reaplicar `/sanitize` para operar somente com `manage_context` v2, sem caps silenciosos de follow-up e sem compactações ocultas que prejudiquem sumarização.

## Invariantes travados

- `/sanitize` usa `run_sampling_request` (modelo no loop).
- `/sanitize` só habilita `manage_context` no loop.
- fluxo esperado: `retrieve -> apply -> retrieve` até convergência.
- convergência/estagnação dirigidas por `manage_context_policy`:
  - `fixed_point_k`
  - `stalled_signature_threshold`
  - `max_chunks_per_apply`
  - `quality_rubric_id`
- seed de `manage_context` deve incluir pares completos call+output, sem truncações/caps silenciosos.
- prompt força contrato v2 e bloqueia campos legados.
- estagnação para em fail-loud quando assinaturas de `retrieve` atingem o threshold de repetição (inclui ciclos sem follow-up event).

## Remoções obrigatórias do legado

- não usar `snapshot_id/new_snapshot_id`
- não usar `ops`/`max_top_items`
- não usar `include_prompt_preview`/`allow_recent`
- sem `MAX_SANITIZE_FOLLOW_UP_REQUESTS`
- sem fallback silencioso de prompt-only seed

## Arquivos-chave

- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/sanitize_prompt.md`

## Verificação mínima

```bash
cd codex-rs
cargo test -p codex-core --lib sanitize -- --test-threads=1
cargo build -p codex-core
```

## Fail-loud

- loop sem convergência e sem parada explícita => regressão
- uso de payload legado no prompt/parse => regressão
- seed truncado sem policy explícita => regressão
