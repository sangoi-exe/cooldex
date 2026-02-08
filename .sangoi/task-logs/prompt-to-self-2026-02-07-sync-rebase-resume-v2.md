# Prompt to self — clean-session resume (sync upstream)

```text
You are Codex CLI working in the repo `codex`.
Continue from:
- CWD: /home/lucas/work/codex
- Branch: sync/rebase-master-20260207-014939
- Last commit: 4cc8268e6
- Date (UTC-3): 2026-02-07T14:45:56-03:00

Objective (1 sentence)
- Finalizar a recuperação do sync upstream (main espelho + master de trabalho) e deixar o core compilando/testando sem regressão funcional.

State
- Done:
  - `main` está alinhada com `upstream/main` (base 5d2702f6b) e o fluxo de sync foi revisado para evitar `-X ours`.
  - Rebase de trabalho foi concluído na branch `sync/rebase-master-20260207-014939`.
  - Commit `4cc8268e6` reverteu a remoção de ferramentas multi-agent, restaurando superfícies importantes.
- In progress:
  - Há mudanças locais não commitadas em arquivos core/state/handlers para fechar incompatibilidades de API pós-rebase.
- Blocked / risks:
  - Build/test do `codex-core` ainda pode quebrar por diferenças de contrato (`FunctionCallOutputPayload`, `TurnContext`, rollout/context items).
  - Publicação final para `master` depende de validação verde; se exigir force-push, executar com cuidado.

Decisions / constraints (locked)
- `origin/main` deve ser espelho literal de `upstream/main`.
- Branch de trabalho é `master`; reescrita de histórico é aceitável se melhorar qualidade/sync.
- Preservar funcionalidade das features já criadas é obrigatório (implementação pode mudar).
- Evitar merge automático com `-X ours/-X theirs`; resolver conflitos de forma explícita.
- Respeitar regras do repo: sem `git clean`, sem stash, sem `git checkout -- <path>`/`git restore`.

Follow-up (ordered)
1. Revisar e concluir as mudanças locais abertas no core/state.
2. Rodar `just fmt` e garantir diff limpo de formatação.
3. Rodar testes relevantes dos crates tocados (`codex-core`, `codex-state`, e os que forem impactados).
4. Se verde, preparar atualização de `master` a partir desta branch e push seguro.
5. Atualizar logs/plano `.sangoi` com status final e comandos executados.

Next immediate step (do this first)
- Confirmar estado atual e atacar os erros de compilação do `codex-core` primeiro.
Commands:
`git status -sb`
`git diff -- codex-rs/core/src/codex.rs codex-rs/core/src/state/session.rs codex-rs/core/src/tools/handlers/manage_context.rs`
`cargo check -p codex-core --all-features --tests --quiet`

Files
- Changed files (last relevant commits):
  - `README.md`
  - `codex-rs/core/agent_run_prompt.md`
  - `codex-rs/core/config.schema.json`
  - `codex-rs/core/src/client_common.rs`
  - `codex-rs/core/src/codex.rs`
  - `codex-rs/core/src/context_manager/history.rs`
  - `codex-rs/core/src/features.rs`
  - `codex-rs/core/src/lib.rs`
  - `codex-rs/core/src/state/service.rs`
  - `codex-rs/core/src/state/session.rs`
  - `codex-rs/core/src/subagent_runner.rs`
  - `codex-rs/core/src/tools/handlers/agent_background.rs`
  - `codex-rs/core/src/tools/handlers/agent_run.rs`
  - `codex-rs/core/src/tools/handlers/mod.rs`
  - `codex-rs/core/src/tools/spec.rs`
  - `codex-rs/state/src/extract.rs`
  - `codex-rs/state/src/runtime.rs`
  - `docs/local-config.toml`
  - `docs/manage_context.md`
  - `docs/multi_agent_mvp_plan.md`
- Focus files to open first:
  - `codex-rs/core/src/tools/handlers/manage_context.rs` — migração de payload/tool-output e estimativas.
  - `codex-rs/core/src/state/session.rs` — snapshots/history e parsing de `ResponseItem`.
  - `codex-rs/core/src/codex.rs` — wiring de sampling/turn metadata/context restore.
  - `codex-rs/core/src/tools/handlers/shell.rs` — assinatura/args de `run_exec_like`.
  - `codex-rs/state/src/runtime.rs` — persistência dinâmica e casos de rollout novos.

Validation (what “green” looks like)
- `just fmt`  # expected: sem erro e sem mudanças adicionais de estilo.
- `cargo test -p codex-core --all-features -- --quiet`  # expected: suíte do core passa.
- `cargo test -p codex-state -- --quiet`  # expected: suíte do state passa.
- `git diff --check`  # expected: sem whitespace/conflict marker residual.

References (read before coding)
- `.sangoi/docs/guia-sync-upstream-main-master.md`
- `.sangoi/plans/plan-sync-upstream-main-master-2026-02-07.md`
- `.sangoi/task-logs/upstream-sync-2026-02-03.md`
- `.sangoi/task-logs/prompt-to-self-2026-02-04-upstream-sync-resume.md`
- `.sangoi/PROMPT_GUIDE.md`
- `.sangoi/PROMPT_TO_SELF_TEMPLATE.md`
```
