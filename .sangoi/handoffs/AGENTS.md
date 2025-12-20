# .sangoi/handoffs — AGENTS

Purpose
- Guardar handoffs semanais/com base em tarefas concluídas, seguindo o template lean.
- Garantir rastreabilidade entre plano (`.sangoi/planning/`), task-logs e quem assumirá na próxima etapa.

Key files
- `.sangoi/handoffs/HANDOFF_TEMPLATE.md` — modelo obrigatório.
- `.sangoi/handoffs/HANDOFF_2025-11-10-dashboard-attend-media.md` — dashboard Home simplificada + upload/download de mídia e fila de atendimento.
- `.sangoi/handoffs/HANDOFF_2025-11-12-css-dashboard.md` — estado atual do plano CSS + donuts.
- `.sangoi/handoffs/HANDOFF_LOG.md` — índice cronológico para QA/revisão.
- `.sangoi/handoffs/HANDOFF_2025-12-05-suiteapp-astenbot-only.md` — SuiteApp consolidada em backend AstenBot único (sem greenfield, paths centralizados, dev settings alinhadas).
- `.sangoi/handoffs/HANDOFF_2025-12-12-sandbox-filecabinet-and-templates.md` — diagnóstico do bundle no sandbox + alinhamento de envios PROVIDER_GS e espelhamento completo de templates.
- `.sangoi/handoffs/HANDOFF_2025-12-16-buttons-whatsapp-report-endpoints.md` — hotfix: `customdeploy_{{AB_TAG}}_buttons_ue` importando `AB/endpoints` para corrigir `endpoints is not defined` no relatório de WhatsApp.
- `.sangoi/handoffs/HANDOFF_2025-12-17-suitelet-cache-dashboard-skeleton.md` — cache global do `/dashboard` + polling em background e skeleton global mais sutil (sem sheen).
- `.sangoi/handoffs/HANDOFF_2025-12-19_ab-tag-placeholders.md` — repo templated com `{{AB_TAG}}` + deploy via builds renderizadas (ab/sb) em `build/sdf-variants/<tag>`.

Notes / Decisions
- Sempre criar/update handoff no mesmo dia em que tocar a pasta ou concluir uma fase relevante.
- Handoffs devem citar anchors reais (planning/task-log) e listar comandos executados.
- Atualize `.sangoi/index/AGENTS-INDEX.md` quando novos handoffs ou subpastas entrarem.
- 2025-12-12: correção de “spikes” de largura no `/attend` registrada via task-log e entrada no `HANDOFF_LOG.md`.
- 2025-12-16: hotfix no `customdeploy_{{AB_TAG}}_buttons_ue` (`endpoints is not defined`) registrado em task-log + handoff e adicionado no topo do `HANDOFF_LOG.md`.

Last Review
- 2025-12-19
