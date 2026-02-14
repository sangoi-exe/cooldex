# Guia de execução — ordem de reaplicação dos mods

## Baselines fixos

- Source (backup): `feca75b2fa272c1c42f776017bb4fc86187fc74e`
- Target (oficial): `284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb`
- Merge-base: `33dc93e4d2913ba940213ede693b84ebaf80b3f6`

## Setup de branch limpa

```bash
git rev-parse --verify feca75b2fa272c1c42f776017bb4fc86187fc74e^{commit}
git rev-parse --verify 284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb^{commit}
git rev-parse --verify 33dc93e4d2913ba940213ede693b84ebaf80b3f6^{commit}
git fetch upstream --prune
git switch --create reapply/mods-2026-02-09 upstream/main
```

Gate fail-loud inicial:

```bash
test "$(git rev-parse HEAD)" = "284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb"
test "$(git merge-base feca75b2fa272c1c42f776017bb4fc86187fc74e 284c03ceabe0fc52fdff8e24657d1fa4ddf5fcdb)" = "33dc93e4d2913ba940213ede693b84ebaf80b3f6"
```

## Ordem recomendada

1. `/accounts` + auth store (`guide-reapply-accounts.md`)
2. `manage_context` (`guide-reapply-manage_context.md`)
3. sub-agents (`guide-reapply-subagents.md`)
4. `/sanitize` (`guide-reapply-sanitize.md`)

Justificativa:

- `/accounts` é feature crítica reportada como quebrada.
- `manage_context` estabiliza higiene e reduz drift para as próximas etapas.
- sub-agents dependem de estado/supervisão consistente após base estável.
- `/sanitize` vem por último pois encosta em slash routing compartilhado.

## Regra operacional por etapa

Para cada etapa:

1. aplicar mudanças manualmente;
2. rodar validações do guia correspondente;
3. criar checkpoint de commit;
4. só então avançar para a próxima.

Exemplo de checkpoint:

```bash
git add -p
git commit -m "reapply(<mod>): port from backup baseline"
```

## Gate de avanço (green obrigatório)

- Não aceitar execução sem `cargo build` green nos crates impactados de cada etapa.
- Se qualquer comando de validação falhar, parar e corrigir antes de seguir.
- Se aparecer conflito sem contrato claro, voltar ao checkpoint anterior.
- Se falha vier de pré-requisito de harness de integração (ex.: binário de teste ausente),
  usar gate de build + checagens estruturais (`rg`) da etapa e registrar explicitamente o gap de ambiente.
- Não avançar com erro de compilação estrutural (`E0063`/campos obrigatórios novos em fixtures de teste):
  sincronizar literais de `Config` antes de continuar.

Gate adicional obrigatório após etapa `/accounts` (antes de avançar para próximo mod):

```bash
cd codex-rs
cargo build -p codex-tui
cd ..
rg -n "StartOpenAccountsPopup|OpenAccountsPopup|SetActiveAccount|RemoveAccount|LogoutAllAccounts|StartChatGptAddAccount|ChatGptAddAccountFinished" codex-rs/tui/src/app_event.rs codex-rs/tui/src/app.rs codex-rs/tui/src/chatwidget.rs
rg -n "update_auth_store|persist_tokens_async|openai_api_key = api_key" codex-rs/login/src/server.rs
rg -n "task_availability_preserves_existing_commands|SlashCommand::Accounts|SlashCommand::DebugConfig|SlashCommand::Clean" codex-rs/tui/src/slash_command.rs
```

Somente avançar quando esses três contratos estiverem íntegros:
- wiring cross-file da TUI;
- persistência login por upsert (sem overwrite de store);
- disponibilidade de slash validada por checagem estrutural (`rg`) e, quando possível, teste focado.

## Rollback rápido (preferir não destrutivo)

```bash
# criar branch de recuperação a partir do último checkpoint válido
git switch --detach <checkpoint-sha>
git switch -c reapply/recover-from-<checkpoint-sha>

# somente se você decidir descartar mudanças locais explicitamente:
# git reset --hard <checkpoint-sha>
```

## Referências

- `.sangoi/docs/mods-reapply-inventory-2026-02-09.md`
- `.sangoi/docs/guide-reapply-accounts.md`
- `.sangoi/docs/guide-reapply-manage_context.md`
- `.sangoi/docs/guide-reapply-subagents.md`
- `.sangoi/docs/guide-reapply-sanitize.md`
- `.sangoi/plans/plan-manual-reapply-mods-2026-02-09.md`
