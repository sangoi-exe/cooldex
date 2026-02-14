# Plano — Warning Pós Hard Auto-Compact (Concluído)

Complexidade: medium

Status: concluído em 2026-02-14 após lock de contrato e validação focada.

## Objetivo

Adicionar warning no contexto do modelo após **auto-compact por limiar hard** para forçar recon operacional antes de continuar o fluxo.

## Escopo

- In-scope:
  - `codex-rs/core/src/codex.rs`
  - testes focados de compactação automática em `codex-rs/core/tests/suite/compact*.rs`
- Out-of-scope:
  - qualquer mudança em `manage_context`/`sanitize`
  - transformar `compact` em tool
  - alterar contrato de `/compact` manual

## Gate 0 — Contract Lock (bloqueante)

Confirmar explicitamente antes de codar:

1. Trigger: somente **auto-compact hard** (não `/compact` manual).
2. Persistência: warning **persistente** no histórico modelável ou **one-shot**.
3. Repetição: repetir em todo hard compact ou deduplicar.
4. Visibilidade: warning interno ao contexto do modelo (não user-facing).
5. Texto final canônico.

Decisão final aplicada:
- `hard-only`: sim (não `/compact` manual)
- persistência: `one-shot`
- dedupe: não aplicável (one-shot)
- visibilidade: interna ao contexto do modelo
- texto canônico: confirmado pelo usuário

## Recon atual (já feito)

- Auto-compact usa `run_auto_compact(...)` em `codex-rs/core/src/codex.rs`.
- Manual `/compact` segue por `Op::Compact -> handlers::compact -> CompactTask`.
- Testes existentes de auto-compact remoto/local estão em `codex-rs/core/tests/suite/compact_remote.rs` e `codex-rs/core/tests/suite/compact.rs`.

## Lanes (fan-out -> fan-in)

### Lane A — Core injection (owner: agent principal)
- Arquivo alvo: `codex-rs/core/src/codex.rs`
- Entrega:
  - inserir warning apenas após sucesso de hard auto-compact;
  - excluir caminho manual `/compact`.

### Lane B — Test coverage (owner: agent principal)
- Arquivos alvo:
  - `codex-rs/core/tests/suite/compact_remote.rs`
  - `codex-rs/core/tests/suite/compact.rs` (se necessário para garantir exclusão do manual/local)
- Entrega:
  - hard auto-compact sucesso => warning presente no request seguinte;
  - falha de compact => warning ausente;
  - manual `/compact` => warning ausente.

### Fan-in
- Ordem: A -> B
- Gate: testes focados + build do crate.

## Plano executável (checklist)

- [x] **Step 1 — Fechar Gate 0 com contrato final**
  - Done criteria:
    - 5 itens do Gate 0 confirmados.
  - Verificação:
    - decisões registradas nesta plan file antes do patch.

- [x] **Step 2 — Recon técnico final de sinal hard + pontos de teste**
  - Done criteria:
    - mapeamento de sinal técnico de hard auto-compact e cenários negativos.
  - Verificação:
    - `rg -n "run_pre_sampling_compact|run_auto_compact|Op::Compact|handlers::compact" codex-rs/core/src/codex.rs -S`
    - `rg -n "remote_compact_runs_automatically|manual_compact|compact failed|Error running remote compact task" codex-rs/core/tests/suite/compact*.rs -S`

- [x] **Step 3 — Implementar warning pós hard auto-compact**
  - Done criteria:
    - warning inserido somente em hard auto-compact bem-sucedido;
    - warning ausente em falha e no manual.
  - Verificação:
    - `rg -n "record_model_warning|auto-compaction|run_auto_compact" codex-rs/core/src/codex.rs -S`

- [x] **Step 4 — Ajustar testes focados**
  - Done criteria:
    - asserts cobrindo presença/ausência conforme contrato.
  - Verificação:
    - `cd codex-rs && cargo test -p codex-core remote_compact_runs_automatically -- --nocapture`
    - `cd codex-rs && cargo test -p codex-core auto_remote_compact_failure_stops_agent_loop -- --nocapture`
    - `cd codex-rs && cargo test -p codex-core manual_compact_uses_custom_prompt -- --nocapture`
    - `cd codex-rs && cargo test -p codex-core remote_auto_compact_warning_is_one_shot -- --nocapture`
    - `cd codex-rs && cargo test -p codex-core local_auto_compact_warning_is_one_shot -- --nocapture`

- [x] **Step 5 — Gate final**
  - Done criteria:
    - formatação e build do crate sem regressão.
  - Verificação:
    - `cd codex-rs && just fmt`
    - `cd codex-rs && cargo build -p codex-core`

## Riscos

- warning acumulando contexto em sessões longas (se persistente);
- matcher de teste frágil por texto canônico;
- worktree suja contaminando diff.

## Mitigação

- texto curto e estável;
- asserts por substring específica;
- diff cirúrgico e checagem de arquivos tocados;
- bloquear execução até worktree liberar.

## Handoff esperado

- lista de arquivos alterados;
- comandos + saídas de validação;
- confirmação explícita: sem toque em `manage_context`/`sanitize`.
