# Guia — sync `upstream/main` → `main` → `master` (sem perder mods)

Este é o **procedimento padrão** pra eu executar sempre que tu pedir pra:

1) Atualizar `main` pra ser **idêntica** a `upstream/main` (mirror).
2) Trazer esses updates pra `master` (tua branch), **sem perder teus mods**.
3) Dar push organizado em `origin/main` e `origin/master`.

## Defaults (fixos)

- `main` é espelho: **ponteiro forçado pra `upstream/main`**.
- `master` recebe upstream via **merge**: **`git merge main`** (sem reescrever histórico).
- WIP: **branch WIP + commit** (sem stash; sem revert/delete).
- Push: **`origin/main` e `origin/master`**.

## Pré‑requisitos (fail loud)

1) Remotes existem:

```bash
git remote -v
```

Tem que ter `origin` e `upstream`.

2) Tu tá no repo certo:

```bash
git rev-parse --show-toplevel
```

## Procedimento

### 0) Preparar timestamp + backups

```bash
stamp="$(date +%Y%m%d-%H%M%S)"
```

Backup dos ponteiros (rollback fácil):

```bash
git branch "backup/main-before-upstream-sync-${stamp}" main
git branch "backup/master-before-upstream-sync-${stamp}" master
```

### 1) Se `master` estiver suja: salvar WIP com commit

```bash
git switch master
git status --porcelain=v1
```

Se aparecer qualquer coisa, faz WIP **em branch separada**:

```bash
git switch -c "wip/master-${stamp}"

# Stage sem pegar “o mundo todo” por acidente:
git add -u
# Se tiver arquivo novo (??), adiciona explicitamente:
# git add path/to/file

git commit -m "WIP (master): ${stamp}"
git switch master
```

### 2) Atualizar `main` pra ficar idêntica a `upstream/main`

```bash
git fetch --all --prune
git branch -f main upstream/main
git rev-parse main upstream/main
```

Push do espelho pro teu fork:

```bash
git push --force-with-lease origin main
```

Se isso falhar por proteção de branch em `origin/main`, não tem milagre: ou tu libera force‑push nessa branch, ou tu aceita que `origin/main` não vai ser espelho (e daí pula esse push).

### 3) Trazer `main` pra `master` (merge, sem reescrever histórico)

```bash
git switch master
git merge --no-edit main
```

Checagens “fail loud”:

```bash
git merge-base --is-ancestor upstream/main master
git rev-list --left-right --count upstream/main...master
```

### 4) Re-aplicar teu WIP (se foi criado na etapa 1)

Se tu criou `wip/master-${stamp}`, reaplica **depois** do merge:

```bash
git cherry-pick "wip/master-${stamp}"
```

### 5) Formatar + testar (quando houver mudanças em Rust)

```bash
cd codex-rs
just fmt
```

Depois roda os testes do(s) crate(s) afetado(s). Exemplos comuns:

```bash
cargo test -p codex-core --all-features -- --quiet
cargo test -p codex-tui -- --quiet
```

### 6) Push da tua branch

```bash
git push origin master
```

## Rollback (se der ruim)

Tu sempre tem os `backup/*` gerados na etapa 0.

Exemplos (destrutivo; usa só se tu entende o impacto):

```bash
git branch -f main "backup/main-before-upstream-sync-${stamp}"

git switch master
git reset --hard "backup/master-before-upstream-sync-${stamp}"
```

## Observações

- `git clean` é proibido nesse workflow (é assim que se perde coisa sem querer).
- `main` não é lugar de trabalho; qualquer commit “tua mão” ali vira problema — por isso o `git branch -f` + `--force-with-lease`.
