# Plano: ajustar heurísticas `replace` vs `exclude` (menos bloat no rollout `.jsonl`)

## Problema (o que você levantou)

- Hoje (e historicamente) a higiene de contexto recomenda muito `replace` (masking) e, às vezes, isso:
  - deixa *muito texto* em `ContextOverlay` (replacements/notes), e
  - gera vários snapshots repetidos no rollout (`context_overlay`/`context_inclusion`),
  - o que pode “abarrotar” o arquivo `.jsonl` ao longo de sessões longas.

> Observação importante: o rollout `.jsonl` é **append-only**; `exclude`/`replace` não encolhem o arquivo (os outputs originais continuam lá). O que dá pra fazer é **evitar piorar** com overlays enormes e muitos snapshots, e/ou criar um mecanismo de “vacuum/compaction” que gere um novo rollout compacto.

## Objetivo

- Reduzir crescimento de rollout causado por higiene (overlays/snapshots).
- Tornar a orientação mais clara e consistente: quando **excluir**, quando **substituir**, quando **deletar**.
- Manter o prompt pequeno sem “esconder lixo” em forma de replacements gigantes.

## Proposta (mudanças de comportamento + instruções)

### 1) Heurística recomendada

- **Preferir `exclude`** para outputs re-geráveis e barulhentos (logs, dumps de arquivo, builds), e manter no prompt **apenas** um note curto com:
  - comando rodado + resultado/erro + próximos passos.
- **Usar `replace`** só quando:
  - você quer que o item continue no prompt, mas *bem curto*, e
  - o replacement é **bem menor** que o original (regra prática: ≤ 3 linhas ou ≤ ~200–300 chars).
- **Evitar `replace` longo** (ele vira “novo log” e ainda replica em snapshots de overlay).
- **Evitar churn** (múltiplos applys pequenos): fazer 1 apply grande por “fase”.

### 2) Ajustes nas instruções (docs + headers)

- Atualizar:
  - `codex-rs/core/sanitize_prompt.md`
  - headers do `manage_context.retrieve`
  - `docs/local-config.toml`
  - `/home/lucas/.codex/config.toml` (apenas instruções, não compact)
- Explicar explicitamente:
  - **`exclude`/`replace` não reduzem o tamanho do rollout** (só o prompt).
  - para reduzir **disco**, precisamos de compaction/vacuum (nova feature).

### 3) (Opcional) Feature: “rollout vacuum”

- Criar um comando/fluxo que:
  - lê o rollout atual,
  - aplica `ContextInclusion` + `ContextOverlay`,
  - e grava um **novo** rollout compacto (sem itens excluídos, sem outputs gigantes, com notas finais).
- Isso é a única forma de **realmente** reduzir o arquivo no disco.

## Checklist

- [ ] Confirmar foco: reduzir bloat “da higiene” vs reduzir tamanho total no disco.
- [ ] Ajustar a documentação e headers (`replace` curto, `exclude` por padrão).
- [ ] (Se aprovado) Implementar “rollout vacuum” (novo comando) com compatibilidade.
- [ ] Rodar `just fmt` e testes focados.

## Perguntas (pra eu não adivinhar)

1) O foco é **A)** só melhorar instruções/heurísticas (sem mexer em formato/armazenamento) ou **B)** também implementar um “rollout vacuum” que gera um novo `.jsonl` menor?
2) Você quer uma regra dura tipo: “**não use `replace` para logs**; sempre `exclude` + note curto”, ou quer manter exceções?

---

Is this what you meant?
