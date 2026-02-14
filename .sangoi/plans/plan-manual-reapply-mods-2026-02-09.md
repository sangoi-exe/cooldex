# Plan — reaplicar mods locais sobre `upstream/main` (2026-02-09)

Plan level: **hard**.

## Objetivo

Documentar e preparar a reaplicação manual dos mods críticos (`manage_context`, `/accounts`, sub-agents, `/sanitize`) sobre uma branch limpa baseada em `upstream/main`, sem merge automático de histórico quebrado.

## Baselines fixados

- Source (backup recuperado): `backup/master-before-upstream-sync-20260207-003149` (`feca75b2fa272c1c42f776017bb4fc86187fc74e`)
- Target (oficial): `upstream/main` (`284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb`)
- Merge-base source↔target: `33dc93e4d2913ba940213ede693b84ebaf80b3f6`

## Checklist

- [x] Mapear commits e arquivos por mod.
- [x] Escrever guia `manage_context`.
- [x] Escrever guia `/accounts`.
- [x] Escrever guia `sub-agents`.
- [x] Escrever guia `/sanitize`.
- [x] Escrever guia de ordem e estratégia de reaplicação.
- [x] Validar paths/comandos/referências cruzadas dos guias.

## Critério de pronto (docs)

- Cada guia contém:
  - comportamento-alvo (contrato funcional),
  - arquivos-fonte no backup,
  - sequência de reaplicação manual em branch limpa,
  - validações de “green”,
  - rollback/fail-loud.
- Guia de ordem define etapas e gates para evitar drift durante reaplicação.

## Gate de baseline (fail-loud)

Antes de qualquer reaplicação, validar os 3 SHAs e abortar se houver mismatch:

```bash
git rev-parse --verify feca75b2fa272c1c42f776017bb4fc86187fc74e^{commit}
git rev-parse --verify 284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb^{commit}
git rev-parse --verify 33dc93e4d2913ba940213ede693b84ebaf80b3f6^{commit}
test "$(git merge-base feca75b2fa272c1c42f776017bb4fc86187fc74e 284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb)" = "33dc93e4d2913ba940213ede693b84ebaf80b3f6"
```
