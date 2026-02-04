# Plan: upstream sync (2026-02-03) — master green + push `origin/master`

Plan level: **hard** (auth cache semantics + multi-account edge-cases + verificação multi-crate).

Status: **DONE** (2026-02-04)

- `main == upstream/main == origin/main`: `33dc93e4d2913ba940213ede693b84ebaf80b3f6`
- Task log: `.sangoi/task-logs/upstream-sync-2026-02-03.md`

## DONE criteria (não-negociável)

- `git status --porcelain=v1` vazio (working tree limpa) em `master`.
- Teste regressivo fixado:
  - `cd codex-rs && cargo test -p codex-core --all-features suite::auth_refresh::unauthorized_recovery_skips_reload_on_account_mismatch -- --nocapture`
- Suite relevante verde (sem assumir “só 1 teste”):
  - `cd codex-rs && cargo test -p codex-core --all-features`
  - Para cada crate *realmente modificado* fora do core: `cargo test -p <crate>`
    - esperado neste caso: `cargo test -p codex-state`, `cargo test -p codex-tui`
  - Se houver snapshots TUI: `cargo insta pending-snapshots -p codex-tui` vazio (ou aceitos intencionalmente).
- Higiene Rust:
  - `cd codex-rs && just fmt`
  - `cd codex-rs && just fix -p codex-core` (e `-p` dos outros crates tocados se necessário)
- Task-log final existe e é factual (sem “ghosts”):
  - `.sangoi/task-logs/upstream-sync-2026-02-03.md`
- Push verificado:
  - `git push origin master`
  - `git ls-remote origin refs/heads/master` == `git rev-parse HEAD`

## Objective (1 sentence)

Finalizar o sync de `upstream/main` em `master`, corrigir os testes (principalmente `auth_refresh`), commitar com higiene, e dar push de `origin/master` (mantendo `main == upstream/main`).

## Recon (estado real quando este plano foi escrito)

- Branch atual: `master`
- `HEAD`: `1b523ecc750ad132378b273ed8333c68e2b483be` (merge `main -> master`)
- `origin/master`: `39077a9e5f327a81c4846e0914f672af4659896c` (atrás do `master`)
- `main == upstream/main == origin/main`: `33dc93e4d2913ba940213ede693b84ebaf80b3f6`
- Working tree (dirty):
  - Modificados: `AGENTS.md`, `codex-rs/core/src/auth.rs`, `codex-rs/core/src/auth/storage.rs`, `codex-rs/core/src/codex.rs`, `codex-rs/core/src/context_manager/history.rs`, `codex-rs/core/src/context_manager/history_tests.rs`, `codex-rs/core/src/context_manager/normalize.rs`, `codex-rs/core/src/shell_snapshot.rs`, `codex-rs/core/src/state/session.rs`, `codex-rs/core/src/tasks/mod.rs`, `codex-rs/core/src/tasks/sanitize.rs`, `codex-rs/core/src/tools/handlers/manage_context.rs`, `codex-rs/core/tests/suite/auth_refresh.rs`, `codex-rs/state/src/runtime.rs`, `codex-rs/tui/src/chatwidget/tests.rs`
  - Não rastreados: `.sangoi/howto/`, `.sangoi/templates/`, `.sangoi/task-logs/prompt-to-self-2026-02-03-upstream-sync.md`

## Falha atual (reproduzida)

Comando:

```bash
cd codex-rs
cargo test -p codex-core --all-features suite::auth_refresh::unauthorized_recovery_skips_reload_on_account_mismatch -- --nocapture
```

Falha:
- `cached_after_tokens` retornou os tokens do *outro* account (`disk-access-token` / `disk-refresh-token`, `account_id=other-account`)
- O teste espera que a auth **permaneça cached** no account atual e reflita os tokens recuperados (`recovered-*`, `account_id=account-id`)

Hipótese (provável root cause):
- `UnauthorizedRecovery` faz `reload_if_account_id_matches(...)` e, no caso de mismatch, escolhe “skip reload” — ok.
- Mas `AuthManager::refresh_token()` chama `self.reload()` incondicionalmente após refresh e isso re-deriva auth usando `active_account_id` do disco (que, no teste, foi trocado pra outro account), efetivamente “switchando” o account na cache.

## Checklist (execução)

### Fase 0 — “Stop the bleeding”: snapshot do estado dirty (recomendado)

Objetivo: não existir “mystery diffs” (muita coisa tocada + risco alto de spillover).

- [x] Criar branch WIP + commit **do estado atual** (backup local; não precisa ir pra `master`):
  - `stamp="$(date +%Y%m%d-%H%M%S)"`
  - `git switch -c "wip/upstream-sync-20260203-${stamp}"`
  - `git add -u`
  - `git add .sangoi/howto/PROMPT_GUIDE.md .sangoi/templates/PROMPT_TO_SELF_TEMPLATE.md .sangoi/task-logs/prompt-to-self-2026-02-03-upstream-sync.md .sangoi/plans/plan-upstream-sync-2026-02-03.md`
  - `git commit -m "WIP: snapshot dirty upstream sync (${stamp})"`

### Fase 1 — Baseline: provar a falha e medir o blast radius

- [x] Repro do teste específico (historicamente falhava; agora deve passar):
  - `cd codex-rs && cargo test -p codex-core --all-features suite::auth_refresh::unauthorized_recovery_skips_reload_on_account_mismatch -- --nocapture`
- [x] Rodar `codex-core` completo pra saber se tem mais falhas além do `auth_refresh`:
  - `cd codex-rs && cargo test -p codex-core --all-features`

### Fase 2 — Semântica explícita (spec curta antes de codar)

Cenário:
- Refresh inicia pro account **A** (processo atual).
- `auth.json` no disco muda `active_account_id` pra **B** (outro processo / outra instância).

Regra proposta (recomendação):
- O processo **não** deve trocar silenciosamente de account (**A → B**) só porque o disco mudou.
- Se o refresh foi pro account A, a cache deve continuar “pinned” em **A** e observar os tokens recém-refreshados.
- Persistência: continua salvando os tokens refreshados no store do account **A** (mesmo se o `active_account_id` do disco estiver em **B**).

- [x] Confirmar se essa regra vale:
  - [ ] somente em `UnauthorizedRecovery`, ou
  - [x] para **todos** os caminhos de refresh (`refresh_token`, `refresh_if_stale`, etc.) (recomendado).

### Fase 3 — Implementação + teste

- [x] Corrigir `AuthManager::refresh_token()` pra não trocar de account na cache:
  - Se `reload_if_account_id_matches(expected_account_id)` **recarregar**, ok.
  - Se for **Skipped** (mismatch / sem account id), **não** chamar `reload()`; atualizar a cache do account atual com os tokens recém-refreshados.
- [x] (Se aplicável) alinhar `refresh_if_stale()` com a mesma regra, pra evitar account switch em refresh automático.
- [x] Re-rodar o teste alvo até passar:
  - `cd codex-rs && cargo test -p codex-core --all-features suite::auth_refresh::unauthorized_recovery_skips_reload_on_account_mismatch -- --nocapture`

### Fase 4 — Higiene + verificação (ordem importa)

- [x] `cd codex-rs && just fmt`
- [x] `cd codex-rs && just fix -p codex-core`
- [x] `cd codex-rs && just fmt` (normalizar pós-fix)
- [x] `cd codex-rs && cargo test -p codex-core --all-features`
- [x] Se `codex-rs/state` mudou: `cd codex-rs && cargo test -p codex-state`
- [x] Se `codex-rs/tui` mudou: `cd codex-rs && cargo test -p codex-tui`
- [x] Se snapshots mudarem (somente se intencional): (não houve mudanças; nada a aceitar)
  - `cd codex-rs && cargo insta pending-snapshots -p codex-tui`
  - aceitar apenas se for esperado: `cd codex-rs && cargo insta accept -p codex-tui`

### Fase 5 — Task-log final (sync 2026-02-03)

- [x] Criar `.sangoi/task-logs/upstream-sync-2026-02-03.md` com:
  - refs (`upstream/main`, `main`, `master`, `origin/*`) antes/depois
  - comandos executados (inclui testes/higiene)
  - resultados (o que ficou verde, o que foi `ignored`)
  - commits finais (mensagens + hashes)

### Fase 6 — Commits (sem spillover; sem `git add -A`)

Preferir lista explícita por commit (ou `git add -p`) ao invés de `git add -u` “cego”.

- [x] Commits finais (código + docs; sem `git add -A`) — ver `.sangoi/task-logs/upstream-sync-2026-02-03.md`.

### Fase 7 — Push + verificação remota

- [x] `git switch master`
- [x] Integrar as mudanças (merge/cherry-pick/ff-only conforme combinado)
- [x] `git status --porcelain=v1` (tem que estar vazio)
- [x] `git push origin master`
- [x] `git ls-remote origin refs/heads/master` == `git rev-parse HEAD`

## Fan-out → fan-in (sub-agents)

Obrigatórios (1× cada):
- Senior Plan Advisor: revisar este plano.
- Senior Code Reviewer: revisar cada item que altere código/docs antes de marcar como “done”.

Lane opcional (se começar a aparecer drift):
- Auditoria: revisar o porquê de `AGENTS.md` + `codex-rs/state` + `codex-rs/tui` estarem modificados e decidir: manter vs reverter vs split commits.
