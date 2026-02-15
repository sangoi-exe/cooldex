# Plan — medium — Recall from current session rollout

## Objective
Adicionar uma nova tool `recall` (read-only) para recuperar trechos recentes pré-compact usando **somente** o JSON da sessão atual, sem alterar `manage_context.retrieve`.

## Constraints
- Não alterar contrato `manage_context` (`retrieve/apply`) existente.
- Fonte única: rollout da sessão atual (sem `rollout_path` externo).
- Saída sem `tool_call/tool_output` por padrão.
- Comportamento fail-loud para arquivo ausente/corrompido.

## Checklist
- [ ] Recon final de pontos de integração (`tools/spec`, handler, acesso ao arquivo de sessão atual).
- [ ] Definir contrato mínimo de `recall` (args + response) com limites (itens/bytes).
- [ ] Implementar handler `recall` read-only com filtro de categorias.
- [ ] Integrar no roteamento/spec de tools.
- [ ] Cobrir testes focados:
  - [ ] boundary no último `compacted`
  - [ ] exclusão de `tool_output`
  - [ ] retorno de `reasoning` e mensagens do agente/usuário
  - [ ] erro explícito para ausência/parse inválido do log
- [ ] Atualizar docs de contrato (`docs/manage_context.md`/`docs/manage_context_model.md` se aplicável + docs da nova tool).
- [ ] Rodar validação focada de testes e registrar evidências.

## Verification commands
- `cd codex-rs && cargo test -p codex-core --lib recall -- --test-threads=1` (ou filtro equivalente se módulo novo)
- `cd codex-rs && cargo test -p codex-core --lib manage_context -- --test-threads=1`
- `cd codex-rs && just fmt`

## Done criteria
- Nova tool `recall` funcional e isolada de `manage_context`.
- Nenhuma regressão nos testes focados.
- Docs atualizadas para o novo contrato.
