# Plano — sincronizar `main` com `upstream/main` e garantir `master` atualizada sem perder mods

> Nota: este arquivo é histórico (2026-01-28) e contém opções que hoje estão banidas (ex.: stash).
> Use o runbook oficial em `.sangoi/docs/guia-sync-upstream-main-master.md`.

Data: 2026-01-28  
Repo: `/home/lucas/work/codex`

Status: concluído; `origin/master` já atualizado (push de `origin/main` opcional).

## Estado atual (recon)

- Branch atual: `master` (`8a521cbb6`, merge de `main`)
- `main` == `upstream/main` (`19d8f71a9`)
- `origin/main` está atrás de `main` (4 commits)
- `master` contém `upstream/main` (`upstream/main...master`: behind 0, ahead 32)
- `origin/master` == `master` (`8a521cbb6`)
- Working tree **não** está limpa:
  - Modificados: 7 arquivos em `codex-rs/core/...`
  - Não rastreados: `log-session.txt`, `plan-resume-manage_context.md`
  - Existe stash: `stash@{0}` (`WIP before syncing upstream/main into master (20260127-215558)`)

## Objetivo

1) Garantir que `main` esteja alinhada com `upstream/main` (fetch + ff-only).  
2) Garantir que `master` contém `main` **sem reescrever histórico**.  
3) (Opcional) Publicar as atualizações no fork (`origin`).

## Checklist (passo a passo)

### 0) Proteger o WIP (pra não perder nada)

Escolher **uma** opção:

**A) Stash (rápido; mantém tudo local)**

- [x] `git stash push -u -m "WIP before syncing upstream/main into master (20260127-215558)"`

**B) Commit em branch WIP (mais limpo; recomendado se pretende continuar esse trabalho)**

- [ ] `git switch -c wip/<nome-curto>`
- [ ] `git add -A`
- [ ] `git commit -m "WIP: <descrição curta>"`
- [ ] `git switch master`

### 1) Backup dos ponteiros (segurança)

- [x] `git branch backup/master-before-upstream-sync-20260127-215558 master`
- [x] `git branch backup/main-before-upstream-sync-20260127-215558 main`

### 2) Atualizar refs e alinhar `main`

- [x] `git fetch --prune upstream`
- [x] `git fetch --prune origin`
- [x] `git switch main`
- [x] `git merge --ff-only upstream/main`
- [x] Verificar: `git rev-parse main upstream/main`

### 3) Atualizar `master` usando `main` (sem reescrever)

- [x] `git switch master`
- [x] `git merge main`
- [x] Verificar:
  - [x] `git merge-base --is-ancestor upstream/main master`
  - [x] `git rev-list --left-right --count upstream/main...master` (deu `0\t32`)

### 4) (Opcional) Push para o fork

- [ ] `git push origin main`
- [x] `git push origin master`

### 5) Restaurar WIP (se usou stash)

- [x] `git stash apply stash@{0}` (mantém a stash como backup)

## Perguntas para confirmar antes de executar

1) Você quer também dar `push` para `origin/main` (pra deixar seu fork alinhado com `upstream/main`)?
2) Quer que eu mantenha a `stash@{0}` e os branches `backup/*` por enquanto, ou já posso limpar depois?
