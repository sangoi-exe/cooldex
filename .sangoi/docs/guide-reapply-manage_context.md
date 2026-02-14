# Guia de reaplicação — `manage_context` (v2 hard break, detalhado)

## Objetivo

Reaplicar `manage_context` com contrato v2 estrito e comportamento fail-loud, sem compatibilidade silenciosa com payload legado.

## Escopo técnico obrigatório

- `codex-rs/core/src/tools/spec.rs`
- `codex-rs/core/src/tools/handlers/manage_context.rs`
- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/sanitize_prompt.md`
- `codex-rs/core/src/config/mod.rs`
- `docs/manage_context.md`
- `docs/manage_context_model.md`
- `docs/manage_context_cheatsheet.md`

## Contrato v2 (fonte da verdade)

### Schema público da tool

- Campos suportados:
  - `mode`
  - `policy_id`
  - `plan_id`
  - `state_hash`
  - `chunk_summaries`
- Sem campos extras (`additional_properties = false`).

### Parser runtime do handler

- `serde(deny_unknown_fields)` também bloqueia payload fora do contrato.
- `mode` válido: apenas `retrieve` ou `apply`.
- `retrieve`:
  - exige `policy_id`
  - aceita somente `mode + policy_id`
- `apply`:
  - exige `policy_id`, `plan_id`, `state_hash`, `chunk_summaries`
  - `chunk_summaries` não-vazio
  - `chunk_summaries.len() <= max_chunks_per_apply`
- `policy_id` precisa casar com `manage_context_policy.quality_rubric_id`.

## Legado proibido (deve falhar loud)

- `snapshot_id`
- `new_snapshot_id`
- `ops`
- `max_top_items`
- `include_prompt_preview`
- `allow_recent`

Se qualquer campo acima for aceito em parse/execução, é regressão.

## Fluxo `retrieve` (v2)

Entrada:

```json
{"mode":"retrieve","policy_id":"<runtime policy_id>"}
```

Saída obrigatória:

- `plan_id`
- `state_hash`
- `policy_id`
- `chunk_manifest`
- `top_offenders`
- `convergence_policy`
- `progress_report`

Forma esperada (resumo):

```json
{
  "mode":"retrieve",
  "plan_id":"...",
  "state_hash":"...",
  "policy_id":"...",
  "chunk_manifest":[...],
  "top_offenders":[...],
  "convergence_policy":{
    "fixed_point_k":2,
    "stalled_signature_threshold":2,
    "max_chunks_per_apply":8,
    "quality_rubric_id":"..."
  },
  "progress_report":{
    "manifest_chunk_count":12,
    "remaining_apply_batches":2,
    "max_chunks_per_apply":8
  }
}
```

Semântica:

- `chunk_manifest` é a referência canônica de `chunk_id` para `apply`.
- `plan_id` e `state_hash` são tokens anti-drift para o próximo `apply`.

## Fluxo `apply` (v2)

Entrada:

```json
{
  "mode":"apply",
  "policy_id":"<runtime policy_id>",
  "plan_id":"<from latest retrieve>",
  "state_hash":"<from latest retrieve>",
  "chunk_summaries":[
    {
      "chunk_id":"chunk_001",
      "tool_context":"...",
      "reasoning_context":"..."
    }
  ]
}
```

Validações obrigatórias por chunk:

- `chunk_id` não-vazio
- `chunk_id` único dentro do payload
- `chunk_id` presente no `chunk_manifest` atual
- `tool_context` não-vazio
- `reasoning_context` não-vazio

Semântica por chunk aplicado:

- gera exatamente 1 `<tool_context>` e 1 `<reasoning_context>`
- tenta `replacement` quando o resumo cabe em `approx_bytes`
- se não couber, faz `exclude` do item original
- retorna evento aplicado em `applied_events`

Saída obrigatória:

- `applied_events`
- `new_state_hash`
- `progress_report`
- `stop_reason`

Forma esperada (resumo):

```json
{
  "mode":"apply",
  "applied_events":[
    {
      "chunk_id":"chunk_001",
      "source_id":"r42",
      "index":17,
      "excluded":false,
      "replacement_applied":true,
      "tool_context":"<tool_context>...</tool_context>",
      "reasoning_context":"<reasoning_context>...</reasoning_context>"
    }
  ],
  "new_state_hash":"...",
  "progress_report":{
    "requested_chunks":1,
    "applied_chunks":1,
    "excluded_chunks":0,
    "replaced_chunks":1,
    "notes_added":2,
    "manifest_chunk_count_before":12,
    "remaining_manifest_chunks":11,
    "manifest_chunk_count_after":11,
    "max_chunks_per_apply":8
  },
  "stop_reason":"target_reached"
}
```

## Matriz de `stop_reason`

### Sucesso de `apply`

- `target_reached`
- `fixed_point_reached`

### Falhas de contrato/consistência

- `invalid_summary_schema`
- `state_hash_mismatch`
- `plan_id_invalid`
- `invalid_contract`

Observação:

- No caminho de erro, o handler responde envelope estruturado com `stop_reason` + `message` (fail-loud).

## Anti-drift (obrigatório no reapply)

- `apply` deve usar `plan_id` e `state_hash` da **última** resposta `retrieve`.
- Se o estado mudou:
  - falha com `state_hash_mismatch` ou `plan_id_invalid`
  - refazer `retrieve` antes de novo `apply`.

## Integração com `/sanitize` (obrigatória)

### Prompt/policy

- `/sanitize` injeta policy runtime no prompt (`policy_id`, `fixed_point_k`, `stalled_signature_threshold`, `max_chunks_per_apply`).
- prompt de rubrica bloqueia campos legados e força contrato v2.

### Loop de execução

- `/sanitize` permite apenas a tool `manage_context`.
- sequência esperada: `retrieve -> apply -> retrieve ...` até convergência.
- seed inclui pares completos call/output de `manage_context` sem truncação silenciosa.

### Detecção de follow-up (ponto sensível)

- não usar slicing por tamanho de history.
- usar delta entre snapshots `before/after` por assinatura de output:
  - `(call_id, output_signature)`
  - filtrando apenas call IDs de `manage_context` no snapshot `after`.
- isso evita idle falso quando `manage_context.apply` reescreve history.

## Sequência de reaplicação recomendada

1. Reaplicar schema da tool (`spec.rs`) com campos v2 apenas.
2. Reaplicar parser/handler (`manage_context.rs`) com `deny_unknown_fields` e fail-loud.
3. Reaplicar validações anti-drift (`policy_id`, `plan_id`, `state_hash`).
4. Reaplicar invariantes de `chunk_summaries` e emissão dupla de tags por chunk.
5. Reaplicar integração `/sanitize`:
   - rubrica/prompt
   - loop follow-up por delta estável
   - estagnação/fixed-point por policy.
6. Sincronizar docs de uso (`docs/manage_context*.md`).

## Checklist de verificação (obrigatório)

- contrato:
  - `rg -n "policy_id|plan_id|state_hash|chunk_summaries" codex-rs/core/src/tools/spec.rs codex-rs/core/src/tools/handlers/manage_context.rs`
  - `rg -n "snapshot_id|new_snapshot_id|ops|max_top_items|include_prompt_preview|allow_recent" codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tools/spec.rs`
  - nota: matches em testes de rejeição de legado são esperados; foco é garantir ausência em caminho produtivo.
- integração sanitize:
  - `rg -n "manage_context_follow_up_events_since|output_signature|history_before_request" codex-rs/core/src/tasks/sanitize.rs`
- execução:
  - `cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1`
  - `cd codex-rs && cargo test -p codex-core --lib sanitize -- --test-threads=1`
  - `cd codex-rs && cargo build -p codex-core`

## Critérios de regressão

- payload legado parseia/executa
- `retrieve` aceita campos de `apply`
- `apply` não exige anti-drift completo (`policy_id/plan_id/state_hash`)
- `chunk_summaries` aceita `chunk_id` inexistente/duplicado
- chunk aplicado sem par `<tool_context>` + `<reasoning_context>`
- `/sanitize` volta a depender de tamanho de history para follow-up

## Notas de operação

- `manage_context` não deve esconder erro de contrato atrás de fallback.
- Ao detectar drift, sempre refazer `retrieve`.
- Se build/teste acusar dead code fora do escopo de reaplicação, tratar em patch separado e explícito.
