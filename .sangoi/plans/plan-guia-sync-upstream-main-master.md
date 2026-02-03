# Plan: guia padronizado — `upstream/main` → `main` → `master`

Plan level: **low** (somente documentação).  
Se tu quiser automatizar com script/`just`, isso vira **medium**.

## Objetivo

Ter **um único guia** (curto, previsível e “fail loud”) pra eu executar sempre o mesmo procedimento:

1) Atualizar `main` pra ficar **idêntica** a `upstream/main`.  
2) Trazer os updates de `main` pra `master` **sem perder teus mods** (com rollback simples).  
3) (Opcional) Publicar em `origin` (`origin/main` e/ou `origin/master`).

## Recon (já feito)

- Já existe um rascunho bem perto do que tu quer em `.sangoi/docs/plan-sync-upstream-main-master.md`.
- Também existe um “log de execução” em `.sangoi/task-logs/plan-upstream-sync-2026-01-29.md` que dá pra usar como referência do que funcionou.

## Checklist

### 1) Confirmar decisões (antes de mexer em qualquer coisa)

- [ ] `master` deve ser atualizada via:
  - [x] **merge** (recomendado; não reescreve histórico), ou
  - [ ] **rebase** (histórico linear; exige `push --force-with-lease`).
- [ ] `main` deve ser atualizada via:
  - [ ] **ff-only** (recomendado; falha alto se divergir), ou
  - [x] **reset --hard** (força igualdade com upstream; destrutivo se alguém commitou em `main`).
- [x] Push em `origin/main` e `origin/master`.
- [ ] Como tu prefere salvar WIP quando a working tree não está limpa:
  - [x] **branch WIP + commit** (recomendado).
  - [ ] **stash -u** (banido neste repo).

### 2) Decidir onde fica o “guia oficial”

- [x] Guia oficial: `.sangoi/docs/guia-sync-upstream-main-master.md`.

### 3) Escrever/ajustar o guia

- [ ] Pré‑requisitos (remotes, branches, working tree limpa).
- [ ] Passo a passo com comandos copy/paste.
- [ ] “Checagens fail loud” (ex.: `rev-parse`, `merge-base`, `rev-list --count`).
- [ ] Como resolver conflitos (merge/rebase) e como abortar.
- [ ] Rollback (via `backup/*` e/ou `git reflog`).
- [ ] Cleanup opcional (apagar `backup/*` e/ou dropar stash quando tudo estiver ok).

### 4) Verificar o guia (sem executar o sync)

- [ ] Rodar validações **somente leitura** (`git remote -v`, `git branch --show-current`, `git show-ref`, etc.) pra garantir que o guia bate com o layout real do repo.
- [ ] Revisar o texto pra ficar curto, sem ambiguidade, e com defaults claros.

## Fan-out → fan-in (sub-agents)

Como o plano está **low**, não vale abrir sub-agents. Se tu pedir script/`just` (vira **medium**), eu faço:

- 1× patch auditor antes/depois do patch (diff‑review).
