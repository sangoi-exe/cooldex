# Plano — Reformulação Total do `manage_context` (hard break, mantendo `retrieve`/`apply`)

Complexidade: complex

## Objetivo

Reescrever o funcionamento de `manage_context` com ruptura intencional de contrato legado, preservando apenas os métodos `retrieve` e `apply`.

## Regras travadas

- O modelo deve ver contexto completo antes de qualquer sumarização de chunk.
- Cada chunk sanitizado deve gerar exatamente:
  - um `<tool_context>`
  - um `<reasoning_context>`
- Nenhum parâmetro silencioso de controle de loop.
- Parada sempre explícita em `stop_reason` estruturado.

## Decisões de ruptura (hard break)

1. `manage_context` continua com os modos `retrieve` e `apply`, mas payload v2 é obrigatório.
2. Campos legados de controle são removidos do contrato público.
3. `snapshot_id` legado deixa de ser canônico; `state_hash` vira o único controle de consistência.
4. `apply` sem `plan_id + state_hash + policy_id` falha loud.
5. Qualquer loop/threshold é configurado apenas por policy explícita (sem `MAX_*` oculto em runtime).

## Cherry-pick consolidado (melhor das 5 alternativas)

1. Determinismo por fases (Alt 1)
- chunking e aplicação em ordem estável/reprodutível.

2. Planner transacional obrigatório (Alt 2)
- `retrieve` gera `plan_id` + `state_hash` + manifesto.
- `apply` exige vínculo ao plano.

3. Memória em camadas (Alt 3)
- `hot/warm/cold` para priorização de chunk.

4. Event-sourcing + convergência por hash (Alt 4)
- cada mutação vira evento imutável; parada por convergência explícita.

5. Policy declarativa (Alt 5)
- estratégia e limites explícitos em `policy_id`.

## Arquitetura funcional

### `retrieve` (v2 obrigatório)

Entrada:
- `mode = "retrieve"`
- `policy_id`

Saída:
- `plan_id`
- `state_hash`
- `chunk_manifest` (canônico)
- `convergence_policy`
- `top_offenders`

### `apply` (v2 obrigatório)

Entrada:
- `mode = "apply"`
- `plan_id`
- `state_hash`
- `policy_id`

Execução:
1. Valida consistência (`state_hash` igual ao estado corrente).
2. Global pass com modelo usando contexto completo.
3. Chunk pass: uma chamada ao modelo por `chunk_id`.
4. Para cada chunk, persistir exatamente:
   - `<tool_context>` contendo `chunk_id`
   - `<reasoning_context>` contendo `chunk_id`
5. Aplicar mutações de contexto vinculadas ao chunk.

Saída:
- `applied_events`
- `new_state_hash`
- `progress_report`
- `stop_reason`

## Stop reasons permitidos

- `target_reached`
- `fixed_point_reached`
- `context_window_exceeded`
- `invalid_summary_schema`
- `state_hash_mismatch`
- `plan_id_invalid`
- `invalid_contract`

## Policy explícita (sem limite silencioso)

Campos mínimos da policy:
- `fixed_point_k`
- `stalled_signature_threshold`
- `max_chunks_per_apply` (explícito em policy, não escondido no runtime)
- `quality_rubric_id`

## Lanes (fan-out -> fan-in)

### Lane C — Policy e schema
- Arquivos:
  - `codex-rs/core/src/config/mod.rs`
  - `codex-rs/core/config.schema.json`
- Entrega:
  - policy v2 obrigatória para `manage_context`.

### Lane A — Contrato e handler (hard break)
- Arquivos:
  - `codex-rs/core/src/tools/spec.rs`
  - `codex-rs/core/src/tools/handlers/manage_context.rs`
  - `codex-rs/core/src/state/context.rs`
- Entrega:
  - payload v2 obrigatório em `retrieve/apply`;
  - remoção explícita dos caminhos legados.

### Lane B — Orquestração de sumarização
- Arquivos:
  - `codex-rs/core/src/tasks/sanitize.rs`
  - `codex-rs/core/sanitize_prompt.md`
- Entrega:
  - global pass full-context-first;
  - chunk summarization obrigatório;
  - 1 par de tags por chunk.

### Lane D — Testes de ruptura + docs
- Arquivos:
  - `codex-rs/core/tests/**`
  - `docs/manage_context.md`
  - `docs/manage_context_model.md`
  - `docs/manage_context_cheatsheet.md`
  - `.sangoi/docs/guide-reapply-manage_context.md`
  - `.sangoi/docs/guide-reapply-sanitize.md`
- Entrega:
  - cobertura de incompatibilidade intencional;
  - documentação de migração e operação.

### Fan-in
- Ordem: C -> A -> B -> D

## Plano executável (checklist)

- [ ] **Step 1 — Introduzir policy v2 obrigatória**
  - Done criteria:
    - `fixed_point_k/stalled_signature_threshold/max_chunks_per_apply` definidos em schema.
  - Verificação:
    - `rg -n "fixed_point_k|stalled_signature_threshold|max_chunks_per_apply" codex-rs/core/src/config/mod.rs codex-rs/core/config.schema.json`

- [ ] **Step 2 — Hard break no contrato de `manage_context`**
  - Done criteria:
    - `apply` exige `plan_id/state_hash/policy_id`.
    - payload legado retorna erro explícito de contrato inválido.
  - Verificação:
    - `rg -n "plan_id|state_hash|policy_id|invalid.*contract|state_hash_mismatch|plan_id_invalid" codex-rs/core/src/tools/spec.rs codex-rs/core/src/tools/handlers/manage_context.rs`
    - `cd codex-rs && cargo test -p codex-core manage_context -- --nocapture`

- [ ] **Step 3 — Global pass full-context-first**
  - Done criteria:
    - `apply` roda chamada inicial com contexto completo antes de chunking.
  - Verificação:
    - `rg -n "global pass|full context|global_digest|run_sampling_request" codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/tools/handlers/manage_context.rs`
    - `cd codex-rs && cargo test -p codex-core sanitize manage_context -- --nocapture`

- [ ] **Step 4 — Pairing invariants por chunk**
  - Done criteria:
    - cada `chunk_id` gera exatamente um `<tool_context>` e um `<reasoning_context>`.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core sanitize manage_context -- --nocapture`
    - `rg -n "<tool_context>|<reasoning_context>|chunk_id" codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs`

- [ ] **Step 5 — Remover caps ocultos do runtime**
  - Done criteria:
    - não existe limite de loop hardcoded fora de policy explícita.
  - Verificação:
    - `rg -n "MAX_SANITIZE_FOLLOW_UP_REQUESTS|MAX_STALLED_RETRIEVE_REPEATS|MAX_.*FOLLOW" codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/tools/handlers/manage_context.rs`

- [ ] **Step 6 — Stop reasons e eventos imutáveis**
  - Done criteria:
    - saída sempre com `stop_reason` e eventos aplicados reproduzíveis.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core manage_context sanitize -- --nocapture`

- [ ] **Step 7 — Docs de ruptura e migração**
  - Done criteria:
    - docs explicam ruptura intencional e payload v2 obrigatório.
  - Verificação:
    - `rg -n "breaking|v2 obrigatório|plan_id|state_hash|policy_id|stop_reason" docs/manage_context.md docs/manage_context_model.md docs/manage_context_cheatsheet.md .sangoi/docs/guide-reapply-manage_context.md .sangoi/docs/guide-reapply-sanitize.md`

- [ ] **Step 8 — Gate final**
  - Done criteria:
    - fmt/test/build verdes após ruptura.
  - Verificação:
    - `cd codex-rs && just fmt`
    - `cd codex-rs && cargo test -p codex-core sanitize manage_context -- --nocapture`
    - `cd codex-rs && cargo build -p codex-core`

## Riscos

- quebra de integrações existentes que ainda enviam payload legado
- latência/custo de full-context + chunk calls
- risco de atingir janela no global pass

## Rollback

- política de fallback apenas por branch/release (não por compat runtime escondida)
- rollback não destrutivo
