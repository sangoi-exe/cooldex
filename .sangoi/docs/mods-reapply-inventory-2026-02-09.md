# Inventário de mods para reaplicação manual (2026-02-09)

## Baselines

- Source: `backup/master-before-upstream-sync-20260207-003149` (`feca75b2fa272c1c42f776017bb4fc86187fc74e`)
- Target: `upstream/main` (`284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb`)
- Merge-base: `33dc93e4d2913ba940213ede693b84ebaf80b3f6`

## 1) `manage_context`

### Arquivos principais

- `codex-rs/core/src/tools/handlers/manage_context.rs`
- `docs/manage_context.md`
- `docs/manage_context_cheatsheet.md`
- `docs/manage_context_model.md`
- `.sangoi/plans/plan-manage_context-rollout.md`
- `.sangoi/plans/plan-manage_context-replace-vs-exclude.md`

### Commits âncora

- `469d185a329b67ee97c8d92cdf3fad55ef4354f8` Improve context management and indicators
- `d494632a9cbb3d4a9aea2066766f0df3c6e6d1ce` refactor(manage_context): simplify retrieve surface
- `e0725aad2f23ed755ab3eb4fe2c75ef14f7e465c` feat: context hygiene + manage_context v2
- `90e074e3bdb25b4a642bc6788ce299980abe2826` fix(core): post-rebase manage_context + sanitize
- `e9382d66694238b6be633d73d637048d33461704` Fix resume after manage_context snapshots

## 2) `/accounts` + multi-account auth

### Arquivos principais

- `codex-rs/core/src/auth.rs`
- `codex-rs/core/src/auth/storage.rs`
- `codex-rs/tui/src/chatwidget.rs`
- `codex-rs/tui/src/status/account.rs`
- `codex-rs/tui/src/bottom_pane/chatgpt_add_account_view.rs`
- `codex-rs/tui/src/slash_command.rs`
- `docs/authentication.md`
- `docs/slash_commands.md`
- `docs/multi-account-auth-plan.md`

### Commits âncora

- `945364e2b95a01c626d34b7c39810085cfab37d4` feat: multi-account ChatGPT auth + auto-switch
- `39077a9e5f327a81c4846e0914f672af4659896c` feat(tui): richer multi-account popups
- `ee8f56eb09f1e0783c63407b078155e5c2e3bf35` feat(auth): melhorar consistência do auth store
- `4e0947af75fb0ae5b9a49095258d79f3726a6bdc` fix(core): prevent auth reload switching accounts
- `3d6d15f43b4c95d2f00b31a56dfe1be59f6b5a1e` fix(tui): adapt auth/account UI after sync

## 3) Sub-agents (status, wait, instruções dedicadas)

### Arquivos principais

- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/agent/status.rs`
- `codex-rs/core/src/tools/spec.rs`
- `codex-rs/core/src/tools/handlers/collab.rs`
- `.sangoi/sangoi_subagent_instructions.md`
- `.sangoi/sangoi_base_instructions.md`
- `.sangoi/plans/plan-subagents-wait-status-2026-02-05.md`

### Commits âncora

- `97a61352fca493a77d142e3b3f3d41537da0b6a8` feat(core): add agent_run tool (experimental)
- `b9931bb31e55bb0d7ad673f984c498fda40fab6b` feat(core): background agents + workspace lock
- `40f1147195983fad273bd48d7af0ef28df302634` fix(core): share workspace lock fallback
- `160060202bbc8e11803680553afd69b3eb29804b` refactor(core): simplify agent tool registration logic
- `cf2e3580fe202cecd164abe1901bfc28635a53ce` fix(core): harden agent result schema validation
- `90e074e3bdb25b4a642bc6788ce299980abe2826` fix(core): post-rebase manage_context + sanitize

## 4) `/sanitize` (estilo `/review`)

### Arquivos principais

- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/sanitize_prompt.md`
- `codex-rs/tui/src/slash_command.rs`
- `docs/slash_commands.md`

### Commits âncora

- `e0725aad2f23ed755ab3eb4fe2c75ef14f7e465c` feat: context hygiene + manage_context v2
- `90e074e3bdb25b4a642bc6788ce299980abe2826` fix(core): post-rebase manage_context + sanitize
- `a6bd8b3666f474d8825b6cd85d1a682cda4921a5` fix(core): stabilize tool outputs under pruning
- `3d6d15f43b4c95d2f00b31a56dfe1be59f6b5a1e` fix(tui): adapt auth/account UI after sync

## Observações de escopo

- Commits de merge (`0ca310a9d`, `c987d0ddc`, `1b523ecc7`) servem apenas como contexto histórico, não como fonte direta de reaplicação.
- Reaplicar por contrato funcional e diff de arquivo, não por cherry-pick cego.
- Commits compartilhados entre mods devem ser aplicados por ordem de arquivo (não repetir patch já aplicado). A ordem oficial estará no guia `guide-reapply-mods-order.md`.

## Gate fail-loud (inventário)

Antes de partir para os guias, validar:

1) cada commit âncora resolve para objeto válido no repo local;
2) cada commit âncora toca ao menos um arquivo listado no bloco do mod.

Se qualquer validação falhar, interromper e corrigir o inventário antes da reaplicação.
