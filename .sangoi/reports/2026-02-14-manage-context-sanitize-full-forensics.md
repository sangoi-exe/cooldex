# Forense Completa — `manage_context` + `/sanitize`

## 0) Escopo, baseline e método

- Repositório: `/home/lucas/work/codex`
- Estado funcional atual (GOOD): `ee8f56eb09f1e0783c63407b078155e5c2e3bf35` (`HEAD` detached)
- Estado regressivo comparado (BAD): `8418fe04901ca1b3e40d4a3add87a535f2fdd68a` (`reapply/accounts-20260209`)
- Objetivo: levantar tudo que toca `manage_context` e `/sanitize`, explicar o funcionamento end-to-end no GOOD, e provar por que o BAD quebrava.
- Método: inventário estático + fluxo runtime + contrato formal + diff GOOD↔BAD + evidência de rollout.

## 1) Resumo Executivo (direto ao ponto)

1. No GOOD, `/sanitize` funciona porque o contrato é coeso:
- `sanitize_prompt.md` pede `retrieve/apply` com `snapshot_id` + `ops`.
- `tasks/sanitize.rs` alimenta o modelo com o prompt de sanitize + pares recentes de `manage_context`.
- `manage_context` aplica mudanças com validação de `snapshot_id` e usa hash estável (`snapshot_id_for_context`) que ignora ruído pós-último `user`.

2. No BAD, houve troca de contrato para chunk-based (`policy_id/plan_id/state_hash/chunk_summaries`) e o anti-drift ficou potencialmente instável:
- `state_hash_for_context` não ignorava append pós-`user`.
- `handle_apply` passou a falhar duro em `state_hash_mismatch`/`plan_id_invalid`.
- Resultado observado no caso crítico de 2026-02-14 18:06–18:07 UTC: `apply` falhou com `state_hash_mismatch`.

3. Evidência direta de falha no BAD rollout:
- `2026-02-14T18:07:10.197Z`: `state_hash_mismatch`.
- `2026-02-14T18:07:10.226Z`: task finalizou com stall.

## 2) Inventário Total — Relação com `manage_context` e `/sanitize`

### 2.1 Núcleo direto (runtime)

- `codex-rs/core/src/tools/spec.rs:936`
- `create_manage_context_tool` (schema do tool no GOOD).

- `codex-rs/core/src/tools/handlers/manage_context.rs:38`
- `ManageContextToolArgs` (`mode`, `max_top_items`, `snapshot_id`, `ops`, `include_prompt_preview`, `allow_recent`).

- `codex-rs/core/src/tools/handlers/manage_context.rs:120`
- `handle_manage_context` (`retrieve|apply`).

- `codex-rs/core/src/tools/handlers/manage_context.rs:142`
- `handle_retrieve` (snapshot + breakdown + token_usage + header).

- `codex-rs/core/src/tools/handlers/manage_context.rs:233`
- `handle_apply` (validação + resolução de ops + persistência overlay/include mask).

- `codex-rs/core/src/tools/handlers/manage_context.rs:404`
- `snapshot_id_for_context` (anti-drift estável).

- `codex-rs/core/src/tools/handlers/manage_context.rs:1284`
- `resolve_ops` (parser semântico das operações).

- `codex-rs/core/src/tools/handlers/manage_context.rs:2456`
- `apply_resolved_ops` (efeitos concretos no estado e sumário de `applied.*`).

- `codex-rs/core/src/tasks/sanitize.rs:25`
- `MAX_MANAGE_CONTEXT_CALLS_IN_PROMPT=10`.

- `codex-rs/core/src/tasks/sanitize.rs:42`
- loop principal de `SanitizeTask`.

- `codex-rs/core/src/tasks/sanitize.rs:116`
- `collect_recent_manage_context_items` (seed de histórico pro sub-agent).

- `codex-rs/core/src/client_common.rs:19`
- `SANITIZE_PROMPT` embutido via `include_str!("../sanitize_prompt.md")`.

- `codex-rs/core/sanitize_prompt.md:1`
- contrato/instruções do sub-agent `/sanitize` no GOOD.

- `codex-rs/core/src/codex.rs:2582`
- dispatch de `Op::Sanitize`.

- `codex-rs/core/src/codex.rs:3142`
- handler `sanitize(...)`.

- `codex-rs/core/src/codex.rs:3390`
- `spawn_sanitize_task` (configuração de tools/effort/features).

- `codex-rs/core/src/tasks/mod.rs:235`
- auto-higiene pós-turno (`auto_sanitize`).

### 2.2 Entrada/UX (TUI)

- `codex-rs/tui/src/slash_command.rs:28`
- `SlashCommand::Sanitize`.

- `codex-rs/tui/src/slash_command.rs:58`
- descrição do comando.

- `codex-rs/tui/src/bottom_pane/prompt_args.rs:67`
- `parse_slash_name` (`/sanitize` parser).

- `codex-rs/tui/src/bottom_pane/chat_composer.rs:2012`
- dispatch de slash built-in sem args.

- `codex-rs/tui/src/chatwidget.rs:2636`
- `SlashCommand::Sanitize => AppEvent::CodexOp(Op::Sanitize)`.

### 2.3 Configuração e persistência

- `codex-rs/core/src/config/mod.rs:100`
- `AUTO_SANITIZE_CONFIG_KEY`.

- `codex-rs/core/src/config/mod.rs:101`
- `SANITIZE_REASONING_EFFORT_CONFIG_KEY`.

- `codex-rs/core/src/config/mod.rs:103`
- `auto_sanitize_enabled`.

- `codex-rs/core/src/config/mod.rs:135`
- `sanitize_reasoning_effort`.

- `codex-rs/core/src/config/edit.rs:31`
- `ConfigEdit::SetAutoSanitize`.

- `codex-rs/core/src/config/edit.rs:33`
- `ConfigEdit::SetSanitizeReasoningEffort`.

- `codex-rs/core/src/config/edit.rs:286`
- aplicação de `SetAutoSanitize`.

- `codex-rs/core/src/config/edit.rs:289`
- aplicação de `SetSanitizeReasoningEffort`.

### 2.4 Documentação direta

- `docs/manage_context.md:1`
- overview operacional.

- `docs/manage_context_model.md:1`
- playbook longo para agentes.

- `docs/manage_context_cheatsheet.md:1`
- checklist curto.

- `docs/slash_commands.md:22`
- definição de `/sanitize`.

- `docs/local-config.toml:22`
- seção de orientação para tool `manage_context`.

## 3) Fluxo Runtime Completo de `/sanitize` no GOOD

1. O usuário digita `/sanitize`.
- Parse em `parse_slash_name`.
- Referência: `codex-rs/tui/src/bottom_pane/prompt_args.rs:67`.

2. O composer reconhece built-in slash command.
- Referência: `codex-rs/tui/src/bottom_pane/chat_composer.rs:2012`.

3. O chatwidget despacha `Op::Sanitize`.
- Referência: `codex-rs/tui/src/chatwidget.rs:2636`.

4. O core roteia `Op::Sanitize` para handler `sanitize`.
- Referência: `codex-rs/core/src/codex.rs:2582`, `codex-rs/core/src/codex.rs:3142`.

5. O core cria sub-turn de sanitize (`spawn_sanitize_task`).
- Desabilita Shell/UnifiedExec/ApplyPatch/WebSearch.
- Ajusta `sanitize_reasoning_effort` e força `web_search_mode=Disabled`.
- Referência: `codex-rs/core/src/codex.rs:3398` até `codex-rs/core/src/codex.rs:3432`.

6. O core sobe `SanitizeTask`.
- Referência: `codex-rs/core/src/codex.rs:3474`.

7. `SanitizeTask::run` monta prompt + seed.
- `sanitize_prompt` vira uma `ResponseItem::Message` com `SANITIZE_PROMPT`.
- Referência: `codex-rs/core/src/tasks/sanitize.rs:56`.

8. Em loop, coleta pares recentes de `manage_context` e chama sampling.
- Referência: `codex-rs/core/src/tasks/sanitize.rs:75` a `codex-rs/core/src/tasks/sanitize.rs:94`.

9. Se `needs_follow_up=false`, retorna; se `true`, continua.
- Referência: `codex-rs/core/src/tasks/sanitize.rs:109`.

10. Ao terminar, emite `TurnComplete` com mensagem final.
- Referência: `codex-rs/core/src/tasks/mod.rs:230`.

## 4) Contrato Formal do `manage_context` no GOOD

### 4.1 Schema publicado

- `mode` (obrigatório)
- `max_top_items` (retrieve)
- `snapshot_id` (apply anti-drift)
- `ops` (apply)
- `include_prompt_preview`
- `allow_recent`
- Referência: `codex-rs/core/src/tools/spec.rs:992` a `codex-rs/core/src/tools/spec.rs:1047`.

### 4.2 Parsing/validação de payload

- `deny_unknown_fields` em args/op/targets.
- Referências:
- `codex-rs/core/src/tools/handlers/manage_context.rs:39`
- `codex-rs/core/src/tools/handlers/manage_context.rs:58`
- `codex-rs/core/src/tools/handlers/manage_context.rs:72`

### 4.3 `retrieve`

Retorna:
- `mode="retrieve"`
- `snapshot_id`
- `token_usage`
- `header` (inclui `suggested_apply` quando há reasoning incluído)
- `breakdown`
- `notes`

Referência:
- `codex-rs/core/src/tools/handlers/manage_context.rs:221` a `codex-rs/core/src/tools/handlers/manage_context.rs:229`.

### 4.4 `apply`

Pré-condições:
- `ops` não vazio.
- se `snapshot_id` veio, precisa bater com snapshot atual.

Referências:
- `codex-rs/core/src/tools/handlers/manage_context.rs:238`
- `codex-rs/core/src/tools/handlers/manage_context.rs:254`.

Pós-condições (resposta):
- `mode="apply"`
- `ok`
- `applied` (contadores + affected/missing ids + call_ids)
- `new_snapshot_id`
- `extracted` (quando `consolidate_reasoning`)
- `token_usage`
- `prompt_preview` opcional

Referências:
- `codex-rs/core/src/tools/handlers/manage_context.rs:331` a `codex-rs/core/src/tools/handlers/manage_context.rs:345`
- `codex-rs/core/src/tools/handlers/manage_context.rs:2606`.

### 4.5 Operações suportadas e guardrails

Ops:
- `include`, `exclude`, `delete`, `replace`, `clear_replace`, `clear_replace_all`, `include_all`, `consolidate_reasoning`, `add_note`, `remove_note`, `clear_notes`.
- Referência: `codex-rs/core/src/tools/handlers/manage_context.rs:1298` a `codex-rs/core/src/tools/handlers/manage_context.rs:1444`.

Guardrails:
- não pode tocar protected (`environment_context`, `user_instructions`).
- `exclude/delete` de mensagens recentes exige `allow_recent=true`.
- `replace` só em tool output e reasoning.
- Referências:
- `codex-rs/core/src/tools/handlers/manage_context.rs:1317`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1512`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1351`.

### 4.6 Anti-drift (ponto crítico de estabilidade)

No GOOD, `snapshot_id_for_context` ignora:
- itens após último `user` (`last_user_idx` cutoff)
- calls/outputs do próprio `manage_context`

Referência:
- `codex-rs/core/src/tools/handlers/manage_context.rs:404` a `codex-rs/core/src/tools/handlers/manage_context.rs:451`.

## 5) Por que o GOOD funciona

1. Prompt de sanitize e handler falam o mesmo contrato (`snapshot_id` + `ops`).
- `codex-rs/core/sanitize_prompt.md:23`
- `codex-rs/core/src/tools/handlers/manage_context.rs:40`.

2. O loop é simples e robusto.
- Sem tracker complexo de estagnação; depende do próprio follow-up padrão.
- `codex-rs/core/src/tasks/sanitize.rs:70`.

3. O anti-drift é estável intra-turn.
- cutoff pós-`user` evita mismatch espúrio entre `retrieve` e `apply` dentro do mesmo ciclo.
- `codex-rs/core/src/tools/handlers/manage_context.rs:426`.

4. Falhas são loud e localizadas.
- mensagens claras (`snapshot mismatch`, etc.) e sem shim silencioso.
- `codex-rs/core/src/tools/handlers/manage_context.rs:258`.

## 6) Delta GOOD → BAD (o que mudou de verdade)

### 6.1 Troca de contrato do tool

GOOD schema:
- `mode`, `max_top_items`, `snapshot_id`, `ops`, `include_prompt_preview`, `allow_recent`.
- `codex-rs/core/src/tools/spec.rs:992`.

BAD schema:
- `mode`, `policy_id`, `plan_id`, `state_hash`, `chunk_summaries`.
- `8418fe049:codex-rs/core/src/tools/spec.rs:1111`.

### 6.2 Troca do prompt `/sanitize`

GOOD prompt:
- orienta `retrieve(mode+max_top_items)` e `apply(snapshot_id+ops)`.
- `codex-rs/core/sanitize_prompt.md:23` a `codex-rs/core/sanitize_prompt.md:33`.

BAD prompt:
- força contrato chunk-based com `policy_id/plan_id/state_hash/chunk_summaries`.
- `8418fe049:codex-rs/core/sanitize_prompt.md:18` a `8418fe049:codex-rs/core/sanitize_prompt.md:65`.

### 6.3 Troca do loop `/sanitize`

GOOD:
- loop simples com `collect_recent_manage_context_items`.
- `codex-rs/core/src/tasks/sanitize.rs:75`.

BAD:
- loop com policy runtime, allowlist explícita de tool, tracker de estagnação, materialização de history.
- `8418fe049:codex-rs/core/src/tasks/sanitize.rs:314` a `8418fe049:codex-rs/core/src/tasks/sanitize.rs:407`.

### 6.4 Troca do anti-drift

GOOD:
- `snapshot_id_for_context` com corte pós-último user.
- `codex-rs/core/src/tools/handlers/manage_context.rs:426`.

BAD:
- `state_hash_for_context` sem corte pós-último user.
- `8418fe049:codex-rs/core/src/tools/handlers/manage_context.rs:711` a `8418fe049:codex-rs/core/src/tools/handlers/manage_context.rs:751`.

### 6.5 Validação apply mais rígida no BAD

- `state_hash` mismatch bloqueia apply.
- `plan_id` mismatch bloqueia apply.
- `8418fe049:codex-rs/core/src/tools/handlers/manage_context.rs:252` a `8418fe049:codex-rs/core/src/tools/handlers/manage_context.rs:270`.

## 7) Causa Raiz da Falha no BAD (hipótese principal, com evidência parcial)

1. O BAD passou a exigir `state_hash` estável entre `retrieve` e `apply`.
2. Ao mesmo tempo, o hash passou a incluir itens append-only pós-`user`.
3. No ciclo real de `/sanitize`, esses itens podem mudar entre retrieve/apply.
4. Resultado observado no caso crítico: `apply` falha com `state_hash_mismatch`.
5. A ligação causal direta “append pós-user -> mismatch” é mecanicamente coerente com o código, mas depende de evidência temporal completa para prova matemática em cada rollout.

No rollout de 2026-02-14 18:06–18:07 UTC, a conclusão “não sanitizou nessa tentativa” é suportada (retrieve ok seguido de apply com `state_hash_mismatch` e stop).

## 8) Evidência de Rollout (dois casos)

### 8.1 BAD rollout com falha por hash

Arquivo:
- `/home/lucas/.codex/sessions/2026/02/14/rollout-2026-02-14T01-55-23-019c5a80-e6c9-7683-8ad0-169d86a3f164.jsonl`

Evidência:
- `2026-02-14T18:06:30.407Z` `manage_context retrieve` (v2 com `policy_id`).
- `2026-02-14T18:07:10.152Z` `manage_context apply` (`plan_id/state_hash/chunk_summaries`).
- `2026-02-14T18:07:10.197Z` output: `state_hash_mismatch`.
- `2026-02-14T18:07:10.226Z` task complete:
  - `"/sanitize stopped because manage_context retrieve signatures are stalled for 0 repeats ..."`

Linha extraída:
- `2026-02-14T18:07:10.197Z ... state_hash mismatch (expected ..., got ...)`.

### 8.2 Rollout legado em loop

Arquivo:
- `/home/lucas/.codex/sessions/2026/02/13/rollout-2026-02-13T11-12-43-019c5758-cc75-7003-b176-ea3af4a31248.jsonl`

Evidência:
- padrão repetido de `retrieve/apply` com contrato `snapshot_id/ops`.
- task complete:
- `2026-02-14T01:51:04.343Z /sanitize stopped after 16 follow-up requests ...`
- havia `apply ok:true` repetidos antes dos mismatches finais (ou seja, houve sanitização parcial antes do stop final).
- mismatches explícitos no mesmo rollout:
  - `2026-02-14T01:50:11.844Z`: `snapshot mismatch (expected retrieve_1, got ...)`
  - `2026-02-14T01:50:25.056Z`: `snapshot mismatch (expected retrieve_2, got ...)`
  - `2026-02-14T01:50:49.885Z`: `snapshot mismatch (expected retrieve_3, got ...)`

## 9) Testes e Cobertura (o que protege hoje)

### 9.1 `manage_context` (unit)

- parse/mode:
- `manage_context_no_args_requires_mode`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1771`

- retrieve/apply contracts:
- `manage_context_retrieve_includes_reasoning_hint_when_reasoning_included`
- `manage_context_apply_returns_token_usage`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1788`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1840`

- guardrails (`allow_recent`, consolidate reasoning):
- `manage_context_refuses_excluding_recent_without_allow_recent`
- `manage_context_consolidate_reasoning_excludes_reasoning_and_returns_extracted`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1893`
- `codex-rs/core/src/tools/handlers/manage_context.rs:1930`

- anti-drift invariants:
- `snapshot_id_ignores_manage_context_call_and_output`
- `snapshot_id_ignores_manage_context_custom_call_and_output`
- `snapshot_id_ignores_items_after_last_user_message`
- `codex-rs/core/src/tools/handlers/manage_context.rs:2022`
- `codex-rs/core/src/tools/handlers/manage_context.rs:2270`
- `codex-rs/core/src/tools/handlers/manage_context.rs:2342`

### 9.2 Integração com resume/histórico

- `resumed_history_preserves_items_after_manage_context_snapshot`
- `codex-rs/core/src/codex.rs:4674`

### 9.3 `sanitize`

- há cobertura de `sanitize_first_turn_reasoning` (não do loop completo `SanitizeTask`).
- `codex-rs/core/src/codex.rs:5052`

### 9.4 Exposição de toolset

- `manage_context` presente no conjunto de tools por modelo.
- `codex-rs/core/tests/suite/model_tools.rs:56`
- `codex-rs/core/tests/suite/prompt_caching.rs:134`

## 10) Lacunas que permitiram regressão BAD

1. Falta de teste E2E explícito de `/sanitize` com contrato novo e mutação de history intra-turn (`retrieve -> append pós-user -> apply`).

2. Falta de invariantes acopladas entre:
- fronteira de hash anti-drift
- fronteira de manifesto/plano
- lógica de loop de sanitize

3. Mudança de contrato em três pontos ao mesmo tempo (spec/prompt/handler) sem teste de integração transversal com rollout real.

## 11) O que manter fixo para não quebrar de novo

1. Hash/plano devem usar o mesmo recorte de contexto.

2. Sempre testar:
- `retrieve -> append pós-user -> apply` (deve funcionar no que for definido como contrato).
- loop `/sanitize` até convergência com histórico mutável.

3. Troca de contrato deve ser atomicamente versionada (spec + prompt + task + handler + testes + docs no mesmo patch).

4. Mensagem de stall deve refletir a causa real final (`error` vs `retrieve-signature stall`), sem ambiguidade.

## 12) Nota sobre nomenclatura/confusão de “v2”

No histórico analisado, há duas fases chamadas informalmente de “v2”:
- fase GOOD atual: contrato `snapshot_id + ops` (docs atuais chamam de v2 em alguns pontos)
- fase BAD testada: contrato chunk-based (`policy_id/plan_id/state_hash/chunk_summaries`) também tratado como v2

Isso gerou ambiguidade semântica em reapply/migração. É recomendado nomear explicitamente por shape de payload.

## 13) Comandos-base usados na forense

- `rg -n "\bmanage_context\b|/sanitize\b|\bsanitize\b" docs codex-rs/core/src codex-rs/core/tests codex-rs/tui/src`
- `nl -ba <arquivo> | sed -n '<faixa>'`
- `git show <sha>:<path> | nl -ba | sed -n '<faixa>'`
- `git diff ee8f56eb0..8418fe049 -- codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/tools/spec.rs codex-rs/core/sanitize_prompt.md`
- `jq -r '...' <rollout>.jsonl` para `function_call`, `function_call_output`, `task_complete`

## 14) Segunda Passagem (8 lanes) — achados adicionais

1. Cobertura de referências:
- diretas encontradas: 145
- adjacentes encontradas: 36
- diretas não explicitadas na primeira versão do relatório: 122

2. Timeline revisada dos rollouts:
- BAD (`2026-02-14T18:06:30` -> `18:07:10`) mostra sequência limpa `retrieve ok` -> `apply state_hash_mismatch` -> `task_complete` com stall.
- rollout legado (`2026-02-14T01:49` -> `01:51`) mostra vários `apply ok:true` antes de três `snapshot mismatch` seguidos e stop por loop.

3. Drift de documentação confirmado:
- `docs/manage_context.md:8` sugere higiene “por turno”, mas implementação de `run_context_hygiene_pass` consolida reasoning incluído da sessão (`codex-rs/core/src/tasks/mod.rs:344`).
- `docs/manage_context_model.md:28` não explicita a nuance do hash ignorar pós-último `user` e itens de `manage_context` (`codex-rs/core/src/tools/handlers/manage_context.rs:404`).
- `docs/manage_context.md:41` pode induzir leitura de `affected_ids`/`missing_ids` top-level, embora os campos estejam em `applied.*` (`codex-rs/core/src/tools/handlers/manage_context.rs:331`).

4. Lacunas de teste priorizadas (segunda passagem):
- falta teste explícito de `replace` rejeitando mensagens user/assistant.
- falta teste explícito de bloqueio em categorias protegidas (`environment_context`, `user_instructions`) para include/exclude/delete.
- falta teste explícito de `delete` removendo pares call/output de forma atômica.
- falta teste específico do seed de `/sanitize` (`collect_recent_manage_context_items`) garantindo seleção/limite esperado.
- falta teste explícito garantindo que `SanitizeTask` rode somente com toolset permitido (sem shell/web/apply_patch).
- falta teste explícito do fallback de `sanitize_reasoning_effort` (None/Minimal -> fallback válido).
