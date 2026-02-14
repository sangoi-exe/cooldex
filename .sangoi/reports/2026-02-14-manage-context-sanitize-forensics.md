# Forense: `manage_context` + `/sanitize` (2026-02-14)

## 1) Escopo e estado analisado

- Repositório: `/home/lucas/work/codex`
- Branch analisada: `reapply/accounts-20260209`
- Commit atual da branch: `8418fe04901ca1b3e40d4a3add87a535f2fdd68a`
- Binário/sessão problemática reportada pelo usuário:
  - `/home/lucas/.codex/sessions/2026/02/14/rollout-2026-02-14T01-55-23-019c5a80-e6c9-7683-8ad0-169d86a3f164.jsonl`
  - `/home/lucas/.codex/sessions/2026/02/13/rollout-2026-02-13T11-12-43-019c5758-cc75-7003-b176-ea3af4a31248.jsonl`

Objetivo desta nota: preservar as evidências completas do que quebra hoje, por que não remove nada do prompt em alguns ciclos, e quais regressões entraram na reaplicação.

Nota de temporalidade:

- As seções 2-10 descrevem o estado problemático observado nos rollouts.
- A seção 11 registra o estado pós-fix aplicado depois desta análise.

## 2) Evidência direta dos rollouts

### 2.1 Rollout `2026-02-14T01:55:23...` (v2)

Extraído via JSONL (`jq`):

- `2026-02-14T18:06:30.407Z`
  - `manage_context.retrieve`
  - payload: `{"mode":"retrieve","policy_id":"sanitize_prompt"}`
- `2026-02-14T18:07:10.152Z`
  - `manage_context.apply`
  - payload inclui `policy_id + plan_id + state_hash + chunk_summaries` (v2 correto)
- `2026-02-14T18:07:10.197Z`
  - output de `manage_context.apply`:
  - `{"stop_reason":"state_hash_mismatch","message":"state_hash mismatch (expected e37e..., got 52458...)"}`
- `2026-02-14T18:07:10.226Z`
  - `task_complete`:
  - `"/sanitize stopped because manage_context retrieve signatures are stalled for 0 repeats ..."`

Conclusão factual desse ciclo:

1. O `apply` falhou por anti-drift (`state_hash_mismatch`).
2. Sem `apply` bem-sucedido, não há `applied_events`; então não houve sanitização efetiva.
3. A mensagem final de stall (`0 repeats`) é inconsistente com o erro reportado imediatamente antes.

### 2.2 Rollout `2026-02-13T11:12:43...` (legado)

Extraído via JSONL (`jq`): há chamadas `manage_context` com contrato antigo:

- `retrieve` com `max_top_items`, `include_prompt_preview`
- `apply` com `snapshot_id`, `ops`, `allow_recent`

Exemplo real de mismatch legado:

- `snapshot mismatch (expected retrieve_1, got 2df6c7...)`
- `snapshot mismatch (expected retrieve_2, got 8a9f14...)`
- `snapshot mismatch (expected retrieve_3, got cdbcd2...)`

`task_complete` observado no fim desse loop:

- `"/sanitize stopped after 16 follow-up requests to avoid a retrieve-only loop. Run /sanitize again or use /compact."`

Conclusão factual desse rollout:

1. Era fluxo legado (`snapshot_id/ops`), não v2.
2. Havia loop de retrieve/apply com mismatches repetidos até o guard de follow-up.

## 3) Delta funcional: versão antiga (funcionando) vs reaplicação atual

Base antiga usada para contraste: `5a0a45bd0` (código com contrato legado + guard de snapshot estável).

| Área | Antigo (`5a0a45bd0`) | Atual pré-fix (`8418fe049`) | Impacto |
|---|---|---|---|
| Contrato handler | `snapshot_id + ops (+allow_recent...)` | `policy_id + plan_id + state_hash + chunk_summaries` | Hard-break esperado (sem compat) |
| Parser args | legado aceitava campos antigos | `serde(deny_unknown_fields)` em `ManageContextToolArgs` | payload legado agora falha loud |
| Antidrift hash | `snapshot_id_for_context` ignorava itens após último user (`last_user_idx`) | `state_hash_for_context` não ignora append pós-user | aumenta chance de mismatch intra-turn |
| Sanitize seed | `collect_recent_manage_context_items(..., MAX=10)` | `collect_manage_context_seed_items` sem cap arbitrário | melhor contexto para modelo |
| Loop sanitize | sem rastreador avançado | `SanitizeStagnationTracker` (fixed-point + stall + erro) | deveria diagnosticar melhor stalls |
| Prompt sanitize | sem contrato v2 estrito | prompt v2 em `codex-rs/core/sanitize_prompt.md` | modelo passou a chamar v2 |

## 4) Ponto regressivo crítico encontrado

### 4.1 Regressão do anti-drift no hash

Na versão antiga (`5a0a45bd0`), em `snapshot_id_for_context` havia este comportamento:

- ignorar itens adicionados depois da última mensagem de usuário (`last_user_idx`), com comentário explícito de evitar mismatch imediato durante o mesmo turn.

No código analisado antes do patch (`codex-rs/core/src/tools/handlers/manage_context.rs:711`), `state_hash_for_context`:

- ignora chamadas/outputs de `manage_context`, mas
- **não** ignora append-only pós-user.

Resultado prático:

- `retrieve` devolve `state_hash`.
- algum append no histórico dentro do mesmo ciclo (não necessariamente manage_context) altera hash base.
- `apply` subsequente falha com `state_hash_mismatch`.

Isso bate exatamente com o rollout de 2026-02-14 18:06:30 -> 18:07:10.

## 5) Por que “nada era removido do prompt”

Causa direta no ciclo observado:

1. `apply` retornou erro (`state_hash_mismatch`).
2. Sem sucesso de `apply`, não existe mutação de contexto válida (`excluded/replaced/notes` daquele apply).
3. Então o histórico efetivo não foi saneado naquele passo.

Em termos de contrato, isso é fail-loud correto; o problema é a frequência indevida do mismatch e a recuperação ruim do loop.

## 6) Por que a mensagem final ficou errada/confusa

No rollout de 2026-02-14, após erro `state_hash_mismatch`, a task terminou com:

- `"retrieve signatures are stalled for 0 repeats"`

Isso é sintoma de diagnóstico incorreto no loop de `/sanitize` naquela execução específica (ou binário não alinhado com o fonte atual), porque o evento imediatamente anterior era erro de anti-drift.

No fonte atual, o rastreador possui caminho explícito para erro (`ManageContextFollowUpEvent::Error`) em `codex-rs/core/src/tasks/sanitize.rs:114` e mensagem dedicada em `codex-rs/core/src/tasks/sanitize.rs:165`.

## 7) Referências de código (estado pré-fix)

- Contrato v2 da tool: `codex-rs/core/src/tools/spec.rs:1081`
- Rejeição de campos legados no schema test: `codex-rs/core/src/tools/spec.rs:1741`
- Args v2 com `deny_unknown_fields`: `codex-rs/core/src/tools/handlers/manage_context.rs:29`
- Validação anti-drift (`state_hash`): `codex-rs/core/src/tools/handlers/manage_context.rs:252`
- Hash no estado pré-fix sem corte pós-user: `codex-rs/core/src/tools/handlers/manage_context.rs:711`
- Loop `/sanitize` (request -> follow-up parsing): `codex-rs/core/src/tasks/sanitize.rs:324`
- Parse delta de follow-up por assinatura: `codex-rs/core/src/tasks/sanitize.rs:499`
- Mensagens de stall: `codex-rs/core/src/tasks/sanitize.rs:163`
- Prompt v2 de `/sanitize`: `codex-rs/core/sanitize_prompt.md:1`

## 8) Referências de código (baseline antigo usado no contraste)

- Hash antigo com corte pós-user: `5a0a45bd0:codex-rs/core/src/tools/handlers/manage_context.rs:404`
- Comentário explícito justificando esse corte: `5a0a45bd0:codex-rs/core/src/tools/handlers/manage_context.rs:426`
- `/sanitize` antigo (seed limitado, sem tracker atual): `5a0a45bd0:codex-rs/core/src/tasks/sanitize.rs:23`

## 9) Diagnóstico consolidado

1. A migração para v2 (sem compat silenciosa) está correta no contrato.
2. A regressão funcional principal está no anti-drift hash: remoção do corte pós-user que estabilizava retrieve->apply no mesmo turn.
3. O rollout comprova que `apply` falhou por `state_hash_mismatch`, logo não houve sanitização efetiva naquele passo.
4. A mensagem final de stall no rollout é inconsistente com o erro observado e precisa ser tratada como bug de diagnóstico/fluxo da task naquela build.

## 10) Próxima ação recomendada (da análise original)

1. Reintroduzir no hash v2 a semântica de estabilidade intra-turn (ignorar append-only após último user), mantendo exclusão de itens de `manage_context`.
2. Adicionar teste de regressão explícito: `retrieve` seguido de append pós-user no mesmo ciclo e `apply` ainda aceito.
3. Endurecer `/sanitize` para nunca encerrar com mensagem de retrieve-stall quando o último follow-up real foi erro `manage_context`.

## 11) Update pós-fix aplicado nesta branch

Após a análise acima, o patch foi aplicado no código desta branch (working tree):

1. `state_hash_for_context` voltou a ignorar append-only após o último `user`.
2. `chunk_manifest/top_offenders` também passou a ignorar append-only após o último `user`, estabilizando `plan_id`.
3. Testes novos cobrem:
   - estabilidade de `retrieve` com append pós-user;
   - sucesso de `apply` com `plan_id/state_hash` antigos após append pós-user;
   - estabilidade de hash com append pós-user.

Referências pós-fix:

- `codex-rs/core/src/tools/handlers/manage_context.rs:486`
- `codex-rs/core/src/tools/handlers/manage_context.rs:517`
- `codex-rs/core/src/tools/handlers/manage_context.rs:695`
- `codex-rs/core/src/tools/handlers/manage_context.rs:729`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1019`

