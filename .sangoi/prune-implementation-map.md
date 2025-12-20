Prune Implementation Map

Context
- Date: 2025-10-17
- Area: Prune flow (TUI) and native rollout persistence
- Key files: `codex-rs/tui/src/chatwidget.rs`, `codex-rs/core/src/{codex.rs,rollout/{policy.rs,recorder.rs},tools/spec.rs}`, `codex-rs/protocol/src/protocol.rs`

Problem
- Abrir o menu de prune e abortar (Esc/back) podia levar a persistência/desincronização indevida; e o antigo fluxo de `.bak` podia gerar backup 0‑byte em falhas.

Final Design (current state)
- Native persistence (core): rollout now records `RolloutItem::ContextInclusion { included_indices, deleted_indices }` on prune actions.
- Resume: core applies ContextInclusion snapshots while reconstructing history — no file rewrite.
- TUI: removed `.bak` and any rollout rewrite. Prune confirmation closes the menu and shows a summary toast; cancel truly cancels.

User-visible behavior
- Manual prune: after confirm, shows “Pruned {category}: ~{pct}% freed.”
- Advanced prune: after confirm, shows “Applied advanced prune: include +N, exclude -M, delete ×K (freed ~P%).”
- Esc/back from any prune view does not persist or reopen menus unexpectedly.
- No `.bak` is created; “Restore full context” entry removed from prune menu.

Implementation highlights
- Protocol: added `ContextInclusionItem` + `RolloutItem::ContextInclusion`.
- Persist policy/recorder: ContextInclusion is persisted and parsed like Compacted.
- Core ops emission: `Op::SetContextInclusion` and `Op::PruneContextByIndices` append ContextInclusion snapshots.
- Resume path: `reconstruct_history_from_rollout()` applies ContextInclusion to rebuild effective history.
- TUI interaction:
  - Advanced list → Confirm: confirm popup uses `on_complete_event: None` to avoid reabrir menus; Apply triggers ops and closes.
  - Manual list → Confirm: confirm popup uses `on_complete_event: None`; Apply triggers prune op and closes.
  - Menu/root: no `.bak` restore entry.

Tests
- `codex-rs/tui/src/chatwidget/tests.rs: prune_cancel_clears_toggles`
- `codex-rs/tui/src/chatwidget/tests.rs: prune_root_open_close_without_changes_leaves_no_toggles`

Notes
- All prune persistence is native in the rollout; no shutdown work is required.

Crash Fix – UTF‑8 boundary in previews (2025‑10‑18)
- Symptom: qualquer tentativa de abrir/usar `/prune` crashava com
  `assertion failed: self.is_char_boundary(new_len)` em `core/src/state/session.rs:265`.

- Root cause
  - O preview dos itens do contexto fazia `String::truncate(MAX)` sobre texto potencialmente não‑ASCII.
  - Quando `MAX` cortava no meio de um codepoint UTF‑8 (acentos/emoji), o `truncate` assertava boundary e o binário panicava.

- Fix (core)
  - Implementado util grapheme/UTF‑8‑safe:
    - `truncate_grapheme_head(&str, max_bytes) -> String` em `core/src/truncate.rs`.
    - Mantido `truncate_middle` para casos de corte no meio (sem mudança).
  - `preview_for(..)` agora:
    - Formata a string completa do preview (incluindo prefixos como `role:` / `tool output:`).
    - Aplica cap uniforme via `truncate_grapheme_head` (nunca corta no meio de codepoint/cluster).
  - Harden extras:
    - `prune_by_indices` agora `sort_unstable` + `dedup` + reverse (desc) para evitar remoções erradas com índices duplicados.

- Fix (TUI – fluxo avançado)
  - Após confirmar “Yes, apply prune”: limpa toggles locais, faz `Op::GetContextItems` para sincronizar, e retorna ao composer (não reabre o Advanced).
  - Adicionado marcador `[!]` para deleções potencialmente arriscadas (se o próximo item incluído é mensagem do assistente).

- Files tocados
  - `codex-rs/core/src/state/session.rs` (preview seguro + dedup/ordenação em `prune_by_indices`).
  - `codex-rs/core/src/truncate.rs` (novo `truncate_grapheme_head`, testes, docstring).
  - `codex-rs/core/Cargo.toml` (workspace `unicode-segmentation`).
  - `codex-rs/tui/src/chatwidget.rs` (pós‑apply retorna ao composer; refresh/clear; marca `[!]`).

- Validação
  - `cargo check -p codex-core` e `-p codex-tui` ok.
  - Testes unitários: `truncate_head_honors_char_boundaries` e `truncate_grapheme_head_preserves_clusters`.
  - Manual: mensagens com emojis/acentos no histórico → abrir `/prune` não panica; previews estáveis.

- Riscos remanescentes / follow‑ups
  - Grapheme‑safe tem custo pequeno adicional (irrelevante para MAX=80).
  - Para detecção de deleções “arriscadas” mais inteligente, considerar heurísticas que identifiquem dependências não‑adjacentes.

Rollout Replay Hardening – Stable RIDs (2025‑10‑20)
- Problema: `ContextInclusion` gravava apenas índices pós-prune. No resume, esses índices divergiam dos originais e itens deletados reapareciam.
- Solução estrutural:
  - Cada `ResponseItem` agora recebe um RID (`r{u64}`) monotônico em `SessionState`.
- `ContextItemSummary` expõe `id` para UI/testes; `ContextInclusionItem` persiste `included_ids`/`deleted_ids` além dos índices brutos.
  - `id` segue o formato `r{u64}` e é preenchido pelo core sempre que um item entra na conversa.
- Replay (`reconstruct_history_from_rollout`) reaplica prune/set-inclusion pelos RIDs, com fallback para índices quando arquivos antigos não tiverem IDs.
  - Durante o replay, cada `RolloutItem::ResponseItem` passa por `ReplayRidTracker::next()` para garantir ordem determinística.
  - Eventos `Compacted` chamam `assign_compacted_rids` para gerar sequência estável com a mesma lógica utilizada em tempo de execução.
- Emissão:
- `Op::SetContextInclusion` e `Op::PruneContextByIndices` coletam `included_ids` dos itens atuais e mapeiam `deleted_ids` antes da mutação.
  - A coleta é feita via `session.history_rids` para evitar race contra o rebuild parcial do contexto.
- `PruneContextByIndices` grava os RIDs deletados no snapshot para reconstrução determinística.
  - Também grava `included_indices`/`deleted_indices` originais para compatibilidade com rollouts antigos.
- Estado:
  - `SessionState` rastreia `history_rids` e redistribui IDs em `replace_history`/`prune_by_indices`.
  - Helper `apply_include_mask_from_ids` reconstrói a máscara pós-replay mapeando RID → índice atual.
- Testes:
  - Novos testes unitários em `codex.rs` cobrem replay com IDs e fallback apenas por índices.
  - Inclui caso híbrido manual+advanced para garantir que máscaras se acumulam corretamente.
- Manual prune:
  - `Op::PruneContext` persiste snapshots `ContextInclusion` (com `included_ids`) logo após aplicar categorias, garantindo que podas manuais sobrevivam ao resume.
  - O TUI permanece emitindo apenas categorias; toda a expansão para RIDs acontece no core.
- riscos:
  - RIDs são monotônicos globais; sessões extremamente longas podem aproximar `u64::MAX`, mas risco teórico.

Prune UX Refinements – Esc Close & Manual Snapshot (2025-10-20)
- Problema: `Esc` em submenus de prune voltava ao menu raiz em vez de encerrar o fluxo; e o menu manual abria sem métricas até alguém visitar o advanced (único caminho que disparava `GetContextItems`).
- Comportamento novo:
  - `Esc` em qualquer submenu (manual ou advanced) sai do `/prune` de vez; nada de reabrir menu raiz automaticamente.
  - Manual reaproveita o mesmo caminho de snapshot do advanced. Ao abrir, envia `Op::GetContextItems`, marca a lista como ativa e atualiza descrições assim que o evento chega.
- Implementação:
  - `ChatWidget` ganhou rastreio `manual_menu_active` + `manual_menu_entries` e helper `refresh_manual_menu_view()` para recalcular descrições via RIDs recém-persistidos.
  - Manual e advanced compartilham a rotina de fechamento (`clear_manual_menu_tracking`, `reset_advanced_prune_state`) para evitar lixo no resume.
  - `ListSelectionView` passou a expor `update_description_at_index` para atualizar descrições in-place sem recriar a view.
- Testes:
  - `manual_prune_esc_closes_flow` garante que `Esc` derruba o fluxo.
  - `manual_prune_menu_requests_snapshot_and_updates_counts` cobre a recomputação dinâmica das porcentagens assim que `ContextItems` chega.

Smart Context Overlay – `manage_context` (2025-12-20)
- Motivação: sessões longas acumulam tool outputs/reasoning; `/compact` é útil mas disruptivo.
- Solução: adicionar uma camada de overlay (replacements + notes) persistida no rollout e aplicada ao montar o prompt, permitindo “destilar” outputs grandes para texto curto e manter notas fixas.
- Segurança: ao deletar tool calls, deletar também os outputs correspondentes; e, como cinto de segurança, normalizar pares call/output na construção do prompt para nunca deixar outputs órfãos quebrarem a sessão.
- Referência: `.sangoi/reference/manage_context.md`.
