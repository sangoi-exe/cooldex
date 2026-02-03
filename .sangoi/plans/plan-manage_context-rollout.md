# Plano: analisar rollout + melhorar `manage_context` (e instruções)

## Contexto (o que você reportou)

- Mesmo usando `manage_context`, a sessão estourou a janela de contexto do modelo.
- Você quer: (1) encontrar pontos de falha no uso do `manage_context` no rollout; (2) melhorar o `manage_context`; (3) melhorar as instruções em `~/.codex/config.toml`; (4) melhorar os “headers”/mensagens retornadas pelo `manage_context`.

## Artefatos

- Rollout: `/home/lucas/.codex/sessions/2026/01/27/rollout-2026-01-27T17-52-37-019c013a-d122-71e1-adce-a0fc24115dea.jsonl`
- Transcript auxiliar: `log-session.txt`

## Hipóteses iniciais (do rollout)

- `auto_sanitize` estava **desligado** (logo, tool output + reasoning acumulam por muito mais tempo).
- Houve pelo menos 1 `snapshot mismatch` por uso de `snapshot_id` antigo (apply falha → nada é podado).
- `consolidate_reasoning` pode gerar um retorno enorme (`extracted.reasoning.items`), que precisa ser podado logo em seguida (senão você remove reasoning de um lugar e recoloca no tool output).
- O “estouro” aparece como tentativas abortadas (sem token_usage válido), e nem sempre há recuperação automática via hygiene pass.

## Objetivo

- Evitar “dead-end” por falta de contexto (principalmente após `resume`/sessões longas).
- Tornar o fluxo `retrieve → apply → retrieve` mais robusto e mais difícil de usar errado.
- Melhorar a orientação (config + headers) para reduzir churn e evitar outputs grandes persistirem no prompt.

## Checklist (execução)

- [ ] Gerar um relatório curto do rollout (timeline: picos, `snapshot mismatch`, pontos onde faltou hygiene).
- [ ] Melhorar o `manage_context` no `codex-rs/core`:
  - [ ] Erros (ex.: `snapshot mismatch`) retornarem **JSON estruturado** (`ok=false`, `error`, `expected_snapshot_id`, `current_snapshot_id`), em vez de string solta.
  - [ ] Header: reforçar explicitamente o passo “após `consolidate_reasoning`, substituir/excluir o output do próprio `manage_context`”.
  - [ ] Header: mencionar `auto_sanitize` como recomendação padrão para sessões longas.
- [ ] Melhorar instruções em `~/.codex/config.toml`:
  - [ ] Ajustar a seção de hygiene para incluir o “gotcha” do `consolidate_reasoning` + cleanup do tool output.
  - [ ] (Opcional) Ativar `auto_sanitize = true` e calibrar `model_auto_compact_token_limit` (se você quiser uma postura mais automática).
- [ ] (Opcional) Melhorar fallback quando a janela estoura:
  - [ ] Antes de abortar em definitivo, rodar um hygiene pass seguro (ou instruir explicitamente `/sanitize`) para recuperação.
- [ ] Verificar:
  - [ ] `just fmt` em `codex-rs`
  - [ ] `cargo test -p codex-core`

## Perguntas (pra eu não adivinhar)

1) Você quer só **análise + recomendações** (sem patch), ou quer que eu **implemente** as mudanças no `codex-rs`?
2) Posso **editar seu** `~/.codex/config.toml` (ativar `auto_sanitize` e ajustar instruções), ou prefere só um diff sugerido?
3) O “resume bug” (replay do include-mask) entra no escopo desta rodada, ou tratamos separado?

**Recomendação:** implementar (a) erro JSON no `manage_context` + (b) header com o “cleanup após consolidate_reasoning” + (c) ativar `auto_sanitize` (se você topar). Isso costuma ser o maior impacto contra estouro de contexto em sessões longas.

---

Is this what you meant?
