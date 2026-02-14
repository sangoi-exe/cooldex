# Guia de reaplicação — `/accounts` + multi-account auth

## Objetivo funcional

Reaplicar o fluxo multi-conta de ChatGPT no CLI/TUI:

- comando `/accounts` funcional;
- popup com contas disponíveis, seleção e logout;
- persistência consistente de conta ativa e metadados;
- troca automática de conta quando limites/rate-limit exigirem;
- sem troca incorreta de conta em reload/refresh.

## Fonte (backup)

- Branch: `backup/reapply-state-20260209-125806`
- Core:
  - `codex-rs/core/src/auth.rs`
  - `codex-rs/core/src/auth/storage.rs`
  - `codex-rs/core/tests/suite/auth_refresh.rs`
  - `codex-rs/login/src/server.rs`
- TUI:
  - `codex-rs/tui/src/app_event.rs`
  - `codex-rs/tui/src/app.rs`
  - `codex-rs/tui/src/chatwidget.rs`
  - `codex-rs/tui/src/status/account.rs`
  - `codex-rs/tui/src/status/helpers.rs`
  - `codex-rs/tui/src/status/card.rs`
  - `codex-rs/tui/src/bottom_pane/chatgpt_add_account_view.rs`
  - `codex-rs/tui/src/bottom_pane/mod.rs`
  - `codex-rs/tui/src/slash_command.rs`
  - snapshots de `accounts_popup` / `logout_popup`
- Exec:
  - `codex-rs/exec/src/event_processor_with_human_output.rs` (status completed sem truncar)
- Docs:
  - `docs/authentication.md`
  - `docs/slash_commands.md`
  - `docs/multi-account-auth-plan.md`

## Contratos cross-file obrigatórios (`/accounts`)

Sem estes contratos alinhados, o port compila parcialmente e quebra em runtime/compile:

- Eventos do fluxo:
  - `AppEvent::StartOpenAccountsPopup`
  - `AppEvent::OpenAccountsPopup`
  - `AppEvent::SetActiveAccount`
  - `AppEvent::RemoveAccount`
  - `AppEvent::LogoutAllAccounts`
  - `AppEvent::StartChatGptAddAccount`
  - `AppEvent::ChatGptAddAccountFinished`
- Wiring obrigatório:
  - `tui/src/app_event.rs` (definição)
  - `tui/src/app.rs` (handlers)
  - `tui/src/chatwidget.rs` (dispatch/UI popups)
  - `tui/src/bottom_pane/mod.rs` (reexport da `chatgpt_add_account_view`)
- Compat de status:
  - `StatusAccountDisplay::ChatGpt { label, email, plan }` precisa estar alinhado entre
    `status/account.rs`, `status/helpers.rs` e `status/card.rs`.
- Compat de slash:
  - preservar comportamento de `SlashCommand::DebugConfig` e `SlashCommand::Clean`
    durante task; adicionar `/accounts` sem regressão.

## Reaplicação manual (ordem)

1. Portar modelo/persistência (`AuthStore`, `active_account_id`, `accounts`, rate-limits).
2. Portar fluxo de seleção/troca/logout no core (`auth.rs`) sem quebrar refresh.
3. Portar persistência de login sem overwrite:
   - usar `update_auth_store` com upsert (não usar `AuthStore::from_legacy + save_auth` para substituir store inteiro).
   - limpar `openai_api_key` stale quando login novo vier sem API key.
4. Portar wiring do `/accounts` no TUI (eventos + handlers + UI):
   - `app_event.rs` + `app.rs` + `chatwidget.rs` + `bottom_pane/mod.rs`.
5. Portar popups e estado visual de conta/rate-limit.
6. Portar ajustes pós-sync que evitam troca indevida em reload (`auth_refresh` contract).
7. Validar fail-safe do logout:
   - `LogoutAllAccounts` só pode encerrar sessão se logout retornar sucesso.
8. Atualizar snapshots e docs.

## Invariantes fail-loud

- Se `/accounts` depender só de `chatwidget.rs` sem `app_event.rs`/`app.rs`, bloquear merge.
- Se persistência de login substituir store inteiro (perda de contas), bloquear merge.
- Se “logout all” encerrar em erro de logout, bloquear merge.
- Se `/accounts` quebrar disponibilidade de `DebugConfig/Clean`, bloquear merge.
- Se `ConnectorsLoaded`/API de menções divergir de HEAD, bloquear merge antes de seguir.

## Status reaplicado (2026-02-12)

- Fluxo multi-conta reaplicado no core (`auth.rs`, `auth/storage.rs`, `login/server.rs`) com persistência por upsert e contrato de refresh preservado.
- Wiring completo de `/accounts` reaplicado na TUI (`app_event.rs`, `app.rs`, `chatwidget.rs`, `bottom_pane/mod.rs`) com popups dedicados.
- Compat de status e snapshots de popup reaplicados para `accounts_popup` e `logout_popup`.
- Docs de autenticação e slash commands atualizadas para refletir o comportamento da etapa.

## Status complementar (2026-02-13)

- Gate da etapa passou a exigir classificação explícita de falha de validação:
  - regressão funcional;
  - pré-requisito de ambiente/harness;
  - erro estrutural de compilação.
- Erros estruturais de compilação (ex.: `E0063` em fixtures após campo obrigatório novo) bloqueiam avanço da etapa até sincronização dos literais de teste.
- Gate primário de validação alinhado para `cargo build` por crate impactado; testes ficam como validação aprofundada opcional.

## Comandos de apoio

```bash
git show backup/reapply-state-20260209-125806:.sangoi/docs/guide-reapply-accounts.md
git show backup/reapply-state-20260209-125806:.sangoi/docs/mods-reapply-inventory-2026-02-09.md
git diff upstream/main..HEAD -- codex-rs/tui/src/app_event.rs codex-rs/tui/src/app.rs codex-rs/tui/src/chatwidget.rs codex-rs/tui/src/slash_command.rs
```

## Validação (green)

```bash
cd codex-rs
cargo build -p codex-core
cargo build -p codex-login
cargo build -p codex-tui
cargo build -p codex-exec
cd ..
rg -n "ConnectorsLoaded \\{ result, is_final \\}" codex-rs/tui/src/app_event.rs codex-rs/tui/src/app.rs codex-rs/tui/src/chatwidget.rs
rg -n "mention_bindings|take_mention_bindings|set_composer_text_with_mention_bindings" codex-rs/tui/src/chatwidget.rs codex-rs/tui/src/bottom_pane/mod.rs codex-rs/tui/src/bottom_pane/chat_composer.rs
```

Validação aprofundada (opcional, ambiente/harness permitindo):

```bash
cd codex-rs
cargo test -p codex-login --lib persist_tokens_async_ -- --quiet
cargo test -p codex-core auth_refresh -- --quiet
cargo test -p codex-tui slash_command -- --quiet
cargo test -p codex-tui accounts_popup -- --quiet
cargo test -p codex-tui logout_popup -- --quiet
cargo test -p codex-exec completed_status_message -- --quiet
cd ..
```

Se snapshots mudarem intencionalmente:

```bash
cd codex-rs
cargo test -p codex-tui
cargo insta pending-snapshots
cargo insta accept
```

## Fail-loud / rollback

- Não aceitar fallback para “conta errada mas funciona”: mismatch de conta ativa é erro.
- Se `/accounts` abrir sem dados consistentes de store, bloquear reaplicação e corrigir core primeiro.
- Se `persist_tokens_async` apagar contas existentes, bloquear reaplicação.
- Se `logout all` encerrar em erro, bloquear reaplicação.
- Se houver erro estrutural de compilação (`E0063`/campo obrigatório novo em fixtures), bloquear avanço até sincronizar testes.
- Rollback preferencial (não destrutivo):

```bash
git switch --detach <checkpoint-sha>
git switch -c reapply/recover-from-<checkpoint-sha>
```

- Apenas com decisão explícita de descarte local:
  `git reset --hard <checkpoint-sha>`.
