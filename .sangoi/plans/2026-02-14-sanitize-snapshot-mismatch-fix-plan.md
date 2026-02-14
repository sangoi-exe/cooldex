# Plan — Stabilize `/sanitize` regressions (difficulty: hard)

## Objective
Corrigir o cenário reportado com três sintomas combinados:
1) `/sanitize` entra em loop/bug,
2) modelo pode terminar sem resposta útil,
3) contexto não libera para compactar.

## Success Criteria (user-visible)
- `/sanitize` sempre encerra com mensagem final (sucesso ou falha acionável), nunca silencioso.
- Sem churn repetitivo de `snapshot mismatch` em follow-ups.
- O fluxo deixa o estado utilizável para `/compact` quando necessário.

## Scope
- `codex-rs/core/src/tasks/sanitize.rs`
- `codex-rs/core/src/tools/parallel.rs` (apenas se interceptação em runtime for necessária)
- Testes focados em `codex-rs/core/src/tasks/sanitize.rs` e `manage_context` relacionado.

## Lanes
- Lane A (evidência/reprodução): timeline do rollout + hipótese validada.
- Lane B (fix): patch mínimo de causa raiz no fluxo sanitize.
- Lane C (validação): testes focados + build escopado.
- Fan-in: só finalizar com Lane C verde e revisão adversarial sem blocker.

## Checklist
- [ ] 1) Fixar contrato de falha com evidência do rollout
  - Done criteria: timeline confirma causalidade de mismatch/loop e impacto nos 3 sintomas.
  - Verification: inspeção estruturada do rollout fornecido.
- [ ] 2) Implementar correção de causa raiz
  - Done criteria: sanitize não reaproveita `snapshot_id` stale no seed/follow-up.
  - Files: `codex-rs/core/src/tasks/sanitize.rs` (preferencial).
- [ ] 3) Garantir comportamento fail-loud terminal
  - Done criteria: caminho terminal de sanitize retorna mensagem final não vazia.
  - Verification: testes existentes + ajustes de teste se necessário.
- [ ] 4) Testes focados de regressão
  - Done criteria: teste(s) cobrem payload compactado sem campos replay-prone e loop guard.
  - Verification command: `cargo test -p codex-core sanitize -- --nocapture`.
- [ ] 5) Validação de segurança escopada
  - Done criteria: build do crate passa.
  - Verification command: `cargo build -p codex-core`.
- [ ] 6) Revisão final adversarial
  - Done criteria: Senior Code Reviewer sem blocker (`READY` ou `READY_WITH_NITS`).
