# Plano: bug no `resume` com sessões grandes + `manage_context`

## Hipótese (o que encontrei no código)

Quando você usa `manage_context` no meio de uma sessão, o Codex grava no rollout (`*.jsonl`) um snapshot de inclusão (`ContextInclusion`) com os itens **existentes naquele momento** (ids/índices incluídos).

No `resume`, o histórico inteiro é reconstruído primeiro e **só depois** esse snapshot é aplicado. Resultado: tudo o que aconteceu **depois** do `manage_context` pode ficar **fora do include-mask** (não entra no prompt do modelo). A UI ainda pode mostrar a conversa completa, mas o modelo “não vê” o fim e volta a tarefas antigas.

Isso fica mais frequente quanto maior o rollout (ex.: 15MB) e quanto mais cedo você roda `manage_context` e depois continua trabalhando por muito tempo.

## Objetivo

No `resume`/`fork`, restaurar corretamente o estado de `manage_context` **sem perder os itens adicionados depois**, garantindo que o prompt do modelo contenha o “final da conversa” e não reabra tarefas já concluídas.

## Checklist de execução

- [x] Reproduzir em teste: `codex-rs/core/src/codex.rs` (`resumed_history_preserves_items_after_manage_context_snapshot`).
- [x] Corrigir o `resume`/`fork`: replay do rollout na ordem (aplica `ContextInclusion`/`ContextOverlay` antes de registrar itens posteriores).
- [x] Cobrir casos colaterais: deleções (`deleted_ids`) e múltiplos snapshots (replay na ordem).
- [x] Rodar `just fmt` em `codex-rs`.
- [~] Rodar testes focados: `cargo test -p codex-core` (unit tests ok; há falhas em alguns integration tests não relacionadas ao resume).

## Critério de pronto

- Com `manage_context` no meio da sessão, depois de `resume` o prompt enviado ao modelo inclui as mensagens/turnos que vieram depois.
- Teste novo falha antes da correção e passa depois.

---

Is this what you meant?
