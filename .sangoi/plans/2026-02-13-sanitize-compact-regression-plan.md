# Plan — revisão unstaged + correção `/sanitize` sem resposta e contexto para `/compact` (label: hard)

## Scope
- Target: `codex-rs` (foco em `core`; `tui/docs` apenas se comportamento externo exigir).
- Goal:
  - revisar as mudanças `unstaged` relacionadas ao bug;
  - corrigir causa raiz de `/sanitize` sem resposta útil;
  - garantir que o estado pós-sanitize seja elegível para `/compact` quando houver redução de contexto.
- Out of scope: mudanças de auth/account e outros temas não ligados ao fluxo sanitize/manage_context/compact.

## Lanes (fan-out → fan-in)
- Lane A (evidência + contrato): mapear unstaged relevante e reproduzir separadamente os 2 sintomas.
- Lane B (causa raiz): corrigir fluxo de término/resposta do `/sanitize` e materialização de contexto.
- Lane C (segurança/regressão): validar invariantes de histórico (`/undo`, call/output pairing) e integração com `/compact`.
- Fan-in: patch mínimo em arquivos causalmente necessários; sem churn em áreas não relacionadas.

## Checklist
- [ ] 1) Gate 0 — Revisar `unstaged` relevante + contrato
  - Done criteria:
    - lista de arquivos/hunks diretamente ligados ao bug;
    - contrato explícito aceito para este patch:
      - `/sanitize` sempre encerra turno com saída (mensagem final ou erro explícito);
      - estado pós-`/sanitize` é materializado e recálculo de tokens ocorre;
      - `/compact` passa a usar esse snapshot materializado (sem estado stale).
  - Verification commands:
    - `git diff --name-only`
    - `git diff -- codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/codex.rs codex-rs/core/src/compact_remote.rs codex-rs/core/src/state/session.rs codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/tui/src/chatwidget.rs codex-rs/tui/src/slash_command.rs`

- [ ] 2) Gate 1 — Repro determinístico dos sintomas
  - Done criteria:
    - evidência separada de:
      - (A) `/sanitize` sem resposta;
      - (B) `/sanitize` concluindo mas sem liberar contexto para `/compact`.
  - Verification commands:
    - `rg -n "Sanitize|sanitize|manage_context|compact|needs_follow_up|TurnComplete|ContextWindowExceeded|InvalidRequest" codex-rs/core/src codex-rs/tui/src -g '*.rs'`
    - `cd codex-rs && cargo test -p codex-core sanitize::tests:: -- --nocapture`

- [ ] 3) Gate 2 — Corrigir causa raiz com patch mínimo
  - Done criteria:
    - fluxo `/sanitize` não termina em silêncio para erros recuperáveis/esperados;
    - término do task mantém semântica fail-loud (erro claro em vez de sumiço);
    - materialização/replacement history permanece consistente com contexto efetivo.
  - Verification commands:
    - `rg -n "run_sampling_request\(|allowed_tool_names|needs_follow_up|persist_rollout_items|recompute_token_usage" codex-rs/core/src -g '*.rs'`

- [ ] 4) Gate 3 — Compatibilidade com `/compact` + invariantes
  - Done criteria:
    - compactação remota usa snapshot materializado pós-sanitize/manage_context;
    - invariantes preservadas: ghost snapshots para `/undo`, e pairing call/output.
  - Verification commands:
    - `rg -n "prompt_snapshot_for_model|ghost_snapshots|compact_remote|replacement_history|ensure_call_outputs_present_lenient|remove_orphan_outputs_lenient" codex-rs/core/src -g '*.rs'`

- [ ] 5) Gate 4 — Validação técnica e docs
  - Done criteria:
    - formatação, build e testes focados passam;
    - docs atualizadas apenas se comportamento externo mudou.
  - Verification commands:
    - `cd codex-rs && just fmt`
    - `cd codex-rs && cargo build -p codex-core`
    - `cd codex-rs && cargo test -p codex-core sanitize::tests::`

- [ ] 6) Gate 5 — Revisão final do diff + handoff
  - Done criteria:
    - diff final restrito ao escopo;
    - cada hunk mapeado ao sintoma/contrato;
    - riscos residuais explícitos.
  - Verification commands:
    - `git status --short`
    - `git diff -- codex-rs/core/src/tasks/sanitize.rs codex-rs/core/src/codex.rs codex-rs/core/src/compact_remote.rs codex-rs/core/src/state/session.rs codex-rs/core/src/tools/handlers/manage_context.rs codex-rs/tui/src/chatwidget.rs codex-rs/tui/src/slash_command.rs docs/slash_commands.md`
