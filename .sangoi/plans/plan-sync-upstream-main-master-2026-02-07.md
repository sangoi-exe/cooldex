# Plan — sync `upstream/main` → `main` → `master` (2026-02-07)

Plan level: **hard** (working tree suja + `force-with-lease` em `origin/main` + merge/cherry-pick).

## Objetivo

Executar o guia oficial `.sangoi/docs/guia-sync-upstream-main-master.md` sem perder modificações locais.

## Checklist

- [ ] Confirmar pré-requisitos (`origin`/`upstream`, branch atual, estado sujo).
- [ ] Criar backups (`backup/main-before-upstream-sync-*`, `backup/master-before-upstream-sync-*`).
- [ ] Salvar WIP em branch dedicada com commit.
- [ ] Atualizar `main` para refletir `upstream/main` e publicar `origin/main`.
- [ ] Mesclar `main` em `master` e validar ancestrais/divergência.
- [ ] Reaplicar WIP via `cherry-pick` em `master`.
- [ ] Rodar `just fmt` + testes dos crates afetados.
- [ ] Publicar `origin/master`.
- [ ] Registrar validações finais e estado do sync.

## Fan-out / fan-in

- Senior Plan Advisor: revisão implacável do plano antes da execução.
- Senior Code Reviewer: revisão implacável por item concluído que alterar código/config/docs/testes.
