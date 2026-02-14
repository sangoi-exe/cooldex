# Prompt para reiniciar sessão (contesto limpo) — 2026-02-11

Use exatamente este prompt na nova sessão:

```md
Você está trabalhando no repo `codex`.

## Repo identity
- CWD: `/home/lucas/work/codex`
- Branch: `reapply/accounts-20260209`
- Last commit: `54b401aa5fb2f2a7dec3ae13ac2a93a0cbc7bb9a` (`Deflake mixed parallel tools timing test (#11193)`)

## Objetivo
Reaplicar a feature `/accounts` (multi-account auth + UI TUI) sobre `upstream/main`, preservando as funcionalidades locais, sem reintroduzir drift de sync/rebase.

## Status atual
- **Done**
  - Estratégia mudou para reaplicação manual por módulo (não por sync cego).
  - Branches locais/remotas já foram higienizadas e backup criado.
  - Parte do port `/accounts` já está no working tree (arquivos abaixo).
  - `just fmt` já rodou durante a implementação.
  - `cargo test -p codex-core auth_refresh -- --quiet` já passou.
- **In progress**
  - Consolidar port `/accounts` sem quebrar compat com upstream atual.
  - Integrar também mod nova de sub-agents sem truncar `completed` status message.
- **Blocked/riscos**
  - Tentativa anterior de “copiar arquivos inteiros do backup” causou quebras de compatibilidade; seguir só com port cirúrgico.
  - Working tree está parcialmente staged + parcialmente unstaged.

## Decisões travadas
- Não fazer sync/rebase para recuperar mods: reaplicar manualmente.
- Prioridade: `/accounts` primeiro.
- Mod adicional obrigatória: não truncar resposta de sub-agent em `completed` no formatter de `codex-exec`.
- Fail loud: não aceitar “green” com 0 testes.

## Working tree atual (arquivos alterados)
- `codex-rs/core/src/auth.rs`
- `codex-rs/core/src/auth/storage.rs`
- `codex-rs/core/tests/suite/auth_refresh.rs`
- `codex-rs/login/src/server.rs`
- `codex-rs/tui/src/chatwidget.rs`
- `codex-rs/tui/src/slash_command.rs`
- `codex-rs/tui/src/status/account.rs`
- `codex-rs/tui/src/bottom_pane/chatgpt_add_account_view.rs` (novo)
- `codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__accounts_popup.snap` (novo)
- `codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__logout_popup.snap` (novo)
- `docs/authentication.md`
- `docs/multi-account-auth-plan.md` (novo)
- `docs/slash_commands.md`
- `codex-rs/exec/src/event_processor_with_human_output.rs` (mod de sub-agent completed sem truncate)

## Focus files (abrir primeiro)
1. `codex-rs/core/src/auth.rs` — contrato de auth mode/store (impacta core inteiro).
2. `codex-rs/login/src/server.rs` — compat do `save_auth` com `AuthStore`.
3. `codex-rs/tui/src/slash_command.rs` — presença de `/accounts`.
4. `codex-rs/tui/src/chatwidget.rs` — popup/accounts/logout e fluxo de UI.
5. `codex-rs/tui/src/status/account.rs` — labels/status de conta/rate-limit.
6. `codex-rs/exec/src/event_processor_with_human_output.rs` — completed sem truncamento + testes novos.

## Próximos passos (ordenados)
1. Auditar staged/unstaged e limpar port acidental fora do escopo `/accounts` + mod sub-agent.
2. Fechar compat de compile sem copiar arquivo inteiro do backup.
3. Validar `/accounts` no core + TUI.
4. Validar mod de sub-agent completed message (sem truncar).
5. Só depois preparar commit.

## Next immediate step (executar primeiro)
```bash
cd /home/lucas/work/codex
git status --short
git diff -- codex-rs/core/src/auth.rs codex-rs/login/src/server.rs codex-rs/tui/src/slash_command.rs codex-rs/tui/src/chatwidget.rs codex-rs/exec/src/event_processor_with_human_output.rs
```

## Validação (green criteria)
```bash
cd /home/lucas/work/codex/codex-rs
just fmt
cargo test -p codex-core auth_refresh -- --quiet
cargo test -p codex-tui accounts_popup -- --quiet
cargo test -p codex-tui logout_popup -- --quiet
cargo test -p codex-tui slash_command -- --quiet
cargo test -p codex-exec completed_status_message -- --quiet
```

Green = todos os comandos acima passam com testes > 0.

## Gotchas
- `cargo test -p codex-exec` completo pode falhar em sandbox (`PermissionError` em teste de lock); tratar como limitação de ambiente se o target test passar.
- Evitar overwrite total de arquivos vindos do backup; reaplicar por comportamento.
- Se precisar do contexto de docs/plans de reaplicação que ficaram fora desta branch, consultar via:
  - `git show backup/reapply-state-20260209-125806:.sangoi/docs/guide-reapply-accounts.md`
  - `git show backup/reapply-state-20260209-125806:.sangoi/docs/guide-reapply-mods-order.md`
  - `git show backup/reapply-state-20260209-125806:.sangoi/docs/mods-reapply-inventory-2026-02-09.md`

## Referências .sangoi relevantes nesta branch
- `.sangoi/local/prompt-to-self-2026-02-09-accounts-reapply.md`
- `.sangoi/local/prompt-to-self-2026-02-10-instructions-audit.md`
```
