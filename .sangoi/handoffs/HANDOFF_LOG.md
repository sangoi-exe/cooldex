# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-12-20T13:25  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-20_manage-context-v2.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: tornar o `manage_context` realmente usável pelo agente sem fluxo interativo (2 calls: `retrieve` + `apply`).
- Delivered: v2 `mode=retrieve|apply` com `snapshot_id` anti-drift, batch atômico de ops e `dry_run` real via simulação de `SessionState`.
- Delivered: schema atualizado (mantém v1 `action` como legacy) + doc de referência + task-log.
- Fix (test stability): ajustei truncation helpers pra não devolver string vazia quando o budget não cabe 1 grapheme (e corrigi expectations dos testes).

## 1) What I did (precise and auditable)
- Implementei `manage_context.retrieve` (snapshot JSON único, bounded por `max_items`, com `snapshot_id`, token usage opcional e metadados de pairing).
- Implementei `manage_context.apply` (validação upfront, rejeita snapshot mismatch, aplica ops em ordem, persiste snapshots de inclusão/overlay em batch único).
- Atualizei docs e logs para rastreabilidade.

## 2) File‑by‑file changes (why before how)
- `codex-rs/core/src/tools/handlers/manage_context.rs`: adiciona `mode=retrieve|apply`, snapshot_id, ops batch, validação, simulação `dry_run` e summary.
- `codex-rs/core/src/tools/spec.rs`: expande schema para v2 (`mode`, flags de retrieve, `ops`, `snapshot_id`) e remove `action` como required (legacy continua aceito).
- `codex-rs/core/src/truncate.rs`: `truncate_grapheme_head` agora preserva 1 grapheme mesmo quando ele excede o budget (evita preview vazio); testes alinhados.
- `.sangoi/reference/manage_context.md`: doc atualizado com seção v2.
- `.sangoi/task-logs/2025-12-20_manage-context-v2-retrieve-apply.md`: task-log desta entrega.
- `.sangoi/CHANGELOG.md`: highlights do v2.

## 3) In progress (clear stop point)
- Ainda falta: testes focados do v2 (`retrieve` snapshot id + `apply` replace restrictions + dry_run no-mutation) pra travar o contrato.

## 4) Follow‑up (execution order)
1. Adicionar testes unitários do v2 (filtro por tool handler/session state). Expected: coverage mínima de drift + replace safety.
2. Se `snapshot_id` ficar “sensível demais” (qualquer append invalida), avaliar modo opt-in de drift append-only. Expected: agente consegue aplicar batch em sessões que avançam.

## 5) Relevant notes
- `snapshot_id` é SHA-1 opaco (não é segurança; é só drift detection).
- Nossa subset de JSON Schema não expressa `oneOf(action|mode)`; a validação fica no handler (mensagens de erro explícitas).
- Sandbox: alguns testes (`unified_exec`) falham aqui por `openpty` permission denied; não parece regressão lógica de código.

## 6) Commands I ran (with short outcome)
`cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo check -p codex-core`
Result: ✅

`cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo test -p codex-core truncate::tests::truncate_grapheme_head_preserves_clusters`
Result: ✅

`cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo test -p codex-core truncate::tests::truncate_head_honors_char_boundaries`
Result: ✅

---

Date (UTC‑3): 2025-12-20T12:43  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-20_manage-context-v2.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: dar autonomia pro agente “limpar” contexto (smart context) sem depender de `/compact` ou restart de sessão.
- Delivered: tool `manage_context` (list/status + include/exclude/delete + replace restrito a ToolOutput/Reasoning + pinned notes) + persistência no rollout (`ContextOverlay`) + replay no resume.
- Delivered: fix do bug xarope do prune (deletar tool call agora também deleta outputs correspondentes; e o prompt faz uma normalização final call/output pra nunca deixar output órfão quebrar a sessão).
- In progress: evoluir `manage_context` para um contrato **não‑interativo** estilo `apply_patch` (`retrieve` -> 1 JSON; `apply` -> batch de ops atômico).
- Next immediate step (single command): `cat .sangoi/planning/2025-12-20_manage-context-v2.md`

## 1) What I did (precise and auditable)
- Implementei `manage_context` no core e habilitei via `experimental_supported_tools`. Evidence: `.sangoi/task-logs/2025-12-20_manage-context-smart-context.md`.
- Adicionei persistência de overlays (`RolloutItem::ContextOverlay`) e replay no resume/fork. Evidence: `.sangoi/reference/manage_context.md`.
- Corrigi o caso onde prune de tool calls deixava outputs órfãos (e quebrava a sessão). Evidence: `.sangoi/reference/manage_context.md` (Safety section).
- Atualizei changelog e mapas de prune para registrar a decisão/feature. Evidence: `.sangoi/CHANGELOG.md` (2025-12-20).

## 2) File‑by‑file changes (why before how)
- `codex-rs/core/src/tools/handlers/manage_context.rs`: implementa a tool `manage_context` (mutations + persistência de snapshots). Impacto: agente pode gerir contexto sem compact.
- `codex-rs/core/src/state/session.rs`: aplica include-mask + replacements + notes ao construir prompt; prune by indices agora faz cascade call->outputs; normalize final evita outputs órfãos. Impacto: invariantes de tool calls/outputs não quebram.
- `codex-rs/protocol/src/protocol.rs`: adiciona `RolloutItem::ContextOverlay` + tipos de payload. Impacto: overlays persistem e podem ser replays.
- `codex-rs/core/src/codex.rs`: replay de `ContextOverlay` no resume/fork; persistência exposta para handlers. Impacto: comportamento consistente pós‑resume.
- `codex-rs/core/src/tools/spec.rs`: schema/registro do tool `manage_context`. Impacto: tool aparece para modelos que suportam a feature.
- `.sangoi/reference/manage_context.md`: doc de referência (contrato, persistência, safety invariants). Impacto: manutenção/continuidade.
- `.sangoi/task-logs/2025-12-20_manage-context-smart-context.md`: task-log desta entrega.
- `COMMON_MISTAKES.md`: registro do erro de `rustup` temp dir no sandbox e comando correto.

## 3) In progress (clear stop point)
- Task: v2 do `manage_context` (retrieve/apply não‑interativo)  •  Area: `.sangoi/planning/2025-12-20_manage-context-v2.md`
- What’s done: v1 funcional (status/list + ações unitárias), overlay persistido e replay no resume, prune invariants corrigidos.
- Still missing: `retrieve` único (snapshot JSON com `snapshot_id`) + `apply` batch atômico (ops) com validação upfront + `dry_run` real.
- Acceptance criteria on resume: agente consegue fazer no máximo 2 calls (`retrieve` + `apply`) para “limpar” contexto e seguir sessão longa sem restart.

## 4) Follow‑up (execution order)
1. Implementar `manage_context` v2 schema: `mode: retrieve|apply`, `ops`, `snapshot_id`, `max_items` (bounds). Expected output: uma única resposta JSON completa em `retrieve`.
2. Implementar `apply` atômico: resolve targets (ids/indices/call_ids) -> valida -> aplica -> persiste `ContextInclusion` + `ContextOverlay` juntos. Expected output: 1 chamada aplica tudo sem precisar iterar.
3. Adicionar “anti‑drift”: se `snapshot_id` vier e não bater, retornar erro claro (pedindo novo `retrieve`). Expected output: evita aplicar ops em contexto que mudou.
4. Garantir segurança: `replace` continua restrito a ToolOutput/Reasoning; `delete` com cascade call->outputs por default; manter `normalize_tool_call_pairs` como safety net. Expected output: nunca quebrar sessão por outputs órfãos.

## 5) Relevant notes
- Limitação importante: **não editar prompt base do core** (API valida prompt). Guidance do `manage_context` deve viver no schema/descrição da tool e/ou em user instructions/AGENTS.
- Trap (sandbox): `just fmt` pode falhar por permissão em `~/.rustup/tmp`; workaround registrado em `COMMON_MISTAKES.md`.
- Sessão atual estava batendo limite de contexto; este handoff existe para retomar em sessão nova sem perder o fio.

## 6) Commands I ran (with short outcome)
`cd codex-rs && just fmt`
Result: ❌ (permission denied em `~/.rustup/tmp`; ver `COMMON_MISTAKES.md`)

`cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo just fmt`
Result: ✅

`cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo cargo check -p codex-core`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-20_manage-context-smart-context.md`
- Reference: `.sangoi/reference/manage_context.md`
- Planning: `.sangoi/planning/2025-12-20_manage-context-v2.md`
- Changelog: `.sangoi/CHANGELOG.md` (2025-12-20)
- Mistakes: `COMMON_MISTAKES.md`

# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-12-19T05:05  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: criar uma SuiteApp “dev” instalável no sandbox lado a lado com a oficial, sem conflito de IDs e sem mexer no `src/` canônico.
- Delivered: gerador SDF variante + integração no `buildAndDeploy` + comando único `npm run deploy:dev`. Variantes reescrevem IDs do domínio `*_{{AB_TAG}}_*` → `*_sb_*` (scripts + objetos de dados) e isolam FileCabinet em `/SuiteApps/com.avmb.astenbotsb`.
- In progress: estabilizar o deploy “full” no sandbox (último blocker era limite de `customlist.name` 30 chars; foi tratado no gerador).
- Next immediate step (single command): `npm run deploy:dev` (no Windows/WSL).

## 1) What I did (precise and auditable)
- Criei um gerador que produz um projeto SDF derivado em `build/sdf-variants/sb` e reescreve IDs e paths (`/SuiteApps/com.avmb.astenbot` → `/SuiteApps/com.avmb.astenbotsb`). Evidence: `.sangoi/task-logs/2025-12-17_sdf-variant-scriptids.md`.
- Corrigi validação do `manifest.xml`: `projectid` da variante precisa ser `[a-z0-9]`, então o default virou `astenbotsb` (sem `_`/`-`). Evidence: `.sangoi/task-logs/2025-12-17_sdf-variant-scriptids.md`.
- Ampliei o rewrite para cobrir também IDs “sem `_{{AB_TAG}}_`” do OmniView (`custrecord_{{AB_TAG}}ov_*` → `custrecord_sbov_*`) e atualizei o FileCabinet correspondente. Evidence: `.sangoi/task-logs/2025-12-17_sdf-variant-scriptids.md`.
- Adicionei o objeto faltante `customrecord_{{AB_TAG}}_dashboard_cache` no SDF canônico para alinhar com o uso no suitelet e permitir variante isolada (`customrecord_sb_dashboard_cache`). Evidence: `.sangoi/CHANGELOG.md` (2025-12-17).
- Ajustei o gerador para encurtar `customlist.name` (limite 30 chars) no output (ex.: “Message Direction” → “Msg Dir”) para destravar deploy do sandbox. Evidence: log do deploy em 2025-12-19 04:52 PST (falha anterior) + fix no gerador.

## 2) File-by-file changes (why before how)
- `.sangoi/.tools/sdf-variant-generate.mjs`: gera projeto derivado (SuiteApp dev) e faz rewrite de IDs + paths; normaliza `manifest.projectid`; renomeia arquivos XML para bater com `scriptid`; ajusta `<name>` de `customlist` para respeitar 30 chars.
- `buildAndDeploy.js`: adiciona `--variant` (default `sb`) e faz deploy do projeto gerado em `build/sdf-variants/sb` via `suitecloud project:deploy -a`.
- `package.json`: adiciona `deploy:dev` para fazer build+geração+deploy em um comando (`node buildAndDeploy.js --variant`).
- `src/Objects/records/customrecord_{{AB_TAG}}_dashboard_cache.xml`: novo custom record usado pelo Suitelet Admin (cache de métricas/snapshot).
- `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`: checklist e critérios de aceite do deploy do dev variant.
- Docs: `.sangoi/task-logs/2025-12-17_sdf-variant-scriptids.md`, `.sangoi/CHANGELOG.md`, `.sangoi/.tools/AGENTS.md`.

## 3) In progress (clear stop point)
- Task: deploy do dev variant “full” no sandbox  •  Area: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`
- What’s done: geração consistente do projeto variante (`sb`) com rewrite amplo + fix do limite de name em custom lists.
- Still missing: reexecutar `npm run deploy:dev` após o fix de `customlist.name` e mapear o próximo erro (se houver) para adicionar normalização pontual no gerador.
- Acceptance criteria on resume: deploy completa + suitelet de UI aparece no menu e abre sem erros; tarefas MR/SS/UE criadas e executáveis.

## 4) Follow-up (execution order)
1. Rodar `npm run deploy:dev` (depende de Windows/WSL com `pwsh` + SuiteCloud CLI). Expected output: instalação completa sem erro.
2. Se falhar, capturar o primeiro “Details:” + “File:” do log e adicionar normalização no gerador (ex.: limites de nome/label/fieldtype). Expected output: deploy avançar.
3. Só se continuar instável: avaliar migração para placeholders `{{AB_TAG}}` no repo como source-of-truth (documentado no plan). Unblock by: estimar escopo e criar ferramenta de “render”.

## 5) Relevant notes
- Trap: `customlist.name` tem limite 30 chars; o gerador agora encurta automaticamente em objetos `customlist`.
- Trap: `manifest.projectid` não aceita `_`/`-`; o dev projectid default concatena `sb` (`astenbotsb`).
- Sensitive: auth/credentials do SuiteCloud são locais; não registrar tokens em logs/docs.

## 6) Commands I ran (with short outcome)
`node .sangoi/.tools/sdf-variant-generate.mjs --preset full --dry-run`
Result: ✅ (gera mapa e valida tamanhos)

`node .sangoi/.tools/sdf-variant-generate.mjs --preset full`
Result: ✅ (gera `build/sdf-variants/sb`)

## 7) Useful links (no duplication)
- Planning: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`
- Task‑log: `.sangoi/task-logs/2025-12-17_sdf-variant-scriptids.md`
- Changelog: `.sangoi/CHANGELOG.md` (2025-12-17)

Date (UTC‑3): 2025-12-17T10:46  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-17_dashboard-cache-and-skeleton-followups.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: parar o `/dashboard` de “piscar” (skeleton/loading) sempre que a view é reaberta e tornar o skeleton visualmente mais neutro/sutil.
- Delivered: store global `useSuiteletCacheStore` com polling em background (30s) e `DashboardSurface` consumindo cache (sem reativar skeleton ao voltar); skeleton global virou gradiente cinza simples com animação mais suave e sem mudar `display` do elemento.
- In progress: validação operacional em NetSuite + auditoria fina dos pontos onde ainda há “shift” do skeleton vs UI real.
- Next immediate step (single command): `npm --prefix ab_vue_ui run build:ns`

## 1) What I did (precise and auditable)
- Criei um cache global em memória (Pinia) para o `/dashboard`, com refresh em background quando stale e pausa automática com `document.visibilityState !== "visible"`. Evidence: `.sangoi/task-logs/2025-12-17_suitelet-cache-dashboard-polling.md`.
- Rewirei `DashboardSurface.vue` para não dar `loading=true` no `onMounted` e sim derivar o estado do cache (primeiro load liga skeleton; retornos não). Evidence: `.sangoi/task-logs/2025-12-17_suitelet-cache-dashboard-polling.md`.
- Simplifiquei o skeleton global: remove “reflexo” e usa gradiente cinza linear com animação mais sutil; `v-skeleton` não muda `display`. Evidence: `.sangoi/task-logs/2025-12-17_skeleton-gradient-subtle.md`.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/stores/suitelet-cache.ts`: novo store `useSuiteletCacheStore` com cache do `/dashboard` + polling (30s) e skip de update por fingerprint. Impacto: voltar para o dashboard não reativa skeleton e o conteúdo pode atualizar em background.
- `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`: remove fetches no mount e passa a renderizar a partir do store; `openConversationFromRow` usa `row.sourceId` quando existir. Impacto: navegação ida/volta preserva UI “quente” e evita perder IDs não numéricos.
- `ab_vue_ui/src/styles/dashboard-surface-base.css`: skeleton global com gradiente cinza simples e animação por `background-position` (sem pseudo-elemento “sheen”); `ab-skeletonize` preserva o display original. Impacto: skeleton mais neutro e com menos diferenças estruturais.
- Planning: `.sangoi/planning/2025-12-17_dashboard-cache-and-skeleton-followups.md` (checklist detalhado).
- Task logs: `.sangoi/task-logs/2025-12-17_suitelet-cache-dashboard-polling.md`, `.sangoi/task-logs/2025-12-17_skeleton-gradient-subtle.md`.
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-17.
- Handoff: `.sangoi/handoffs/HANDOFF_2025-12-17-suitelet-cache-dashboard-skeleton.md`.

## 3) In progress (clear stop point)
- Task: validação operacional + auditoria de skeleton  •  Area: `.sangoi/planning/2025-12-17_dashboard-cache-and-skeleton-followups.md`
- What’s done: infra de cache/polling + refactor do `DashboardSurface` + ajustes globais do skeleton.
- Still missing: validar em NetSuite (host real) os cenários “ida/volta”, “tab oculta”, “atualização sem flicker” e mapear onde ainda há “layout shift”.
- Acceptance criteria on resume: checklist A + D do planning todo marcado como ✅, ou com issues documentadas.

## 4) Follow‑up (execution order)
1. **Validar `/dashboard` sem skeleton ao voltar** (depende de deploy no NetSuite). Resultado esperado: retorno instantâneo com dados do cache e sem loader global.
2. **Validar polling com aba oculta** (sem dependências). Resultado esperado: sem chamadas enquanto oculto; ao voltar, refresh em background sem flicker.
3. **Verificar custo de rede** (depende de inspeção no DevTools/Script Execution Log). Resultado esperado: no máximo 1 refresh por 30s por key; sem duplicatas em paralelo.
4. **Decidir se dá para reduzir chamadas** (depende do custo observado). Caminhos:
   - usar `dashboard_stats.rows` como fonte primária de lista (menos endpoints), ou
   - refresh parcial (fila 30s, stats 60–90s).
5. **Auditar “layout shift” do skeleton** (sem dependências). Resultado esperado: lista de pontos com print/descrição + fix incremental por componente.
6. **Limpeza opcional** (sem dependências): avaliar se `DashboardSkeleton.vue` é usado; se estiver morto, remover e ajustar contadores (CSS usage).

## 5) Relevant notes
- O cache do dashboard hoje faz 4 chamadas por refresh (`dashboard_stats`, `conversation_stats`, `conversations_list`, `conversations_interactivity_list`); isso é intencional para manter fila + stats aquecidos, mas pode ser otimizado se necessário.
- `DashboardSurface.loading` voltou a ser `ref` (em vez de computed) para manter o provider do skeleton estável e evitar regressões em componentes que usam `useSkeleton()`.
- `analysis/css-usage.json` continua com `Unused classes detected: 78` (baseline atual do repo; não tratado aqui).

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui run build:styles`
Result: ✅

`npm --prefix ab_vue_ui run lint:css-guard`
Result: ✅

`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

`node .sangoi/.tools/css-conflict-report.mjs`
Result: ✅ (Conflicts=0, Duplicates=0)

`node .sangoi/.tools/css-usage-report.mjs`
Result: ⚠️ (Unused=78, baseline)

## 7) Useful links (no duplication)
- Planning: `.sangoi/planning/2025-12-17_dashboard-cache-and-skeleton-followups.md`
- Cache log: `.sangoi/task-logs/2025-12-17_suitelet-cache-dashboard-polling.md`
- Skeleton log: `.sangoi/task-logs/2025-12-17_skeleton-gradient-subtle.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-17
- Handoff: `.sangoi/handoffs/HANDOFF_2025-12-17-suitelet-cache-dashboard-skeleton.md`

Date (UTC‑3): 2025-12-16T15:32  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-16_buttons-whatsapp-report-endpoints-import.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: corrigir o crash no deployment `customdeploy_{{AB_TAG}}_buttons_ue` (WhatsApp sent messages report) com `endpoints is not defined`.
- Delivered: `ab_buttons_ue.js` agora importa `AB/endpoints`, então `endpoints.api.astenbotSentMessagesReports(...)` não quebra mais em runtime.
- In progress: nada.
- Next immediate step (single command): `node -e "new Function(require('fs').readFileSync('src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/ab_buttons_ue.js','utf8')); console.log('ok')"`

## 1) What I did (precise and auditable)
- Ajustei a lista de deps AMD do `ab_buttons_ue.js` para incluir `AB/endpoints` e receber o param `endpoints`.
- Atualizei o rastro em `.sangoi` (task-log + changelog) e deixei um handoff versionado.

## 2) File‑by‑file changes (why before how)
- `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/ab_buttons_ue.js`: adiciona `AB/endpoints` no `define([...])`. Expected impact: elimina `ReferenceError` no relatório de WhatsApp.
- `.sangoi/task-logs/2025-12-16_buttons-whatsapp-report-endpoints-import.md`: contexto + root cause + validação.
- `.sangoi/handoffs/HANDOFF_2025-12-16-buttons-whatsapp-report-endpoints.md`: handoff versionado desta mudança.
- `.sangoi/CHANGELOG.md`: entrada 2025-12-16 para este hotfix.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite que o log `BUTTONS:whatsapp.report.http_error` não volta a emitir `endpoints is not defined` e que o relatório carrega.

## 5) Relevant notes
- Este hotfix é só import/escopo; não altera endpoints nem autenticação.

## 6) Commands I ran (with short outcome)
`node -e "new Function(require('fs').readFileSync('src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/ab_buttons_ue.js','utf8')); console.log('parse: ok')"`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-16_buttons-whatsapp-report-endpoints-import.md`
- Handoff: `.sangoi/handoffs/HANDOFF_2025-12-16-buttons-whatsapp-report-endpoints.md`

Date (UTC‑3): 2025-12-16T10:49  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-16_attend-structure-standardize.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: alinhar a estrutura do `/attend` ao padrão das outras views Suitelet (`ab-main-grid` + `ab-panel` + `ab-card`), já que era a única “fora do padrão”.
- Delivered: `SuiteletAttend.vue` agora usa `section.ab-main-grid` com sidebars `ab-main-secondary--left/right` e painéis `ab-panel`/`ab-card`; o layout sai de `.attend-layout` e passa a ser controlado por `.attend-view .ab-main-grid` em `suitelet-dashboard.css`.
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui test`

## 1) What I did (precise and auditable)
- Comparei a estrutura das views `/templates`, `/customer-groups`, `/campaign-dashboard` e `/regua-dashboard` e repliquei o padrão no `/attend`.
- Removi o “grid do shell” (overrides em `.ab-surface__body/__content`) do Attend e passei o layout para o nível `ab-main-grid` como nas outras superfícies.
- Convertil o sidepanel (resumo/template) para painéis `ab-panel` + `ab-card` no lugar de `.attend-card*`.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/views/SuiteletAttend.vue`: reestruturação do DOM para `section.ab-main-grid` + `ab-main-secondary--left/right` + `ab-panel`/`ab-card`. Expected impact: `/attend` fica consistente com as demais views e reduz CSS “especial” de layout.
- `ab_vue_ui/src/styles/suitelet-dashboard.css`: remove o layout legacy `.attend-layout` e os overrides no shell; define grid de 3 colunas em `.attend-view .ab-main-grid` e adiciona `.attend-template-panel__body`. Expected impact: layout controlado no mesmo nível das outras views (ex.: `/campaign-dashboard`).
- `ab_vue_ui/src/styles/AGENTS.md`: atualizado para refletir o novo padrão do Attend.
- `.sangoi/task-logs/2025-12-16_attend-structure-standardize.md`: task-log do trabalho.
- `.sangoi/CHANGELOG.md`: entrada 2025-12-16 para a mudança estrutural do `/attend`.
- `.sangoi/handoffs/HANDOFF_2025-12-16-attend-structure-standardize.md`: handoff versionado desta alteração.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite (host real) a renderização/responsividade do `/attend` após a mudança estrutural (principalmente sidebars + lista com scroll).

## 5) Relevant notes
- `css-usage-report` continua apontando `Unused classes detected: 78` (preexistente; baseline em `f11a0e2` também reporta 78).

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

`npm --prefix ab_vue_ui run lint:css-guard`
Result: ✅

`node .sangoi/.tools/css-conflict-report.mjs`
Result: ✅ (Conflicts=0, Duplicates=5)

`node .sangoi/.tools/css-usage-report.mjs`
Result: ⚠️ (Unused=78, preexistente)

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-16_attend-structure-standardize.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-16

Date (UTC‑3): 2025-12-16T10:05  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-16_attend-history-name-consistency.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: corrigir/clarificar a inconsistência de nomes no histórico do `/attend` (autor do evento vs nome exibido na lista/resumo).
- Delivered: histórico agora mostra direção (Cliente/Atendente) e, em chat 1:1, eventos `IN` usam o nome da conversa para evitar divergência; em grupo, mantém autor por evento.
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui test`

## 1) What I did (precise and auditable)
- Ajustei o cabeçalho de cada evento do histórico para explicitar a direção (Cliente/Atendente).
- Padronizei o autor dos eventos `IN` em chats 1:1 para usar o nome da conversa (mesmo da lista/resumo), evitando “Angelina” no histórico vs “Nathalie” no resumo.
- Mantive o comportamento antigo em chats de grupo para não perder identificação por participante.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/views/SuiteletAttend.vue`: adiciona pill de direção e ajusta `eventAuthor` para chats 1:1. Expected impact: menos confusão visual; mantém compatibilidade com grupos.
- `.sangoi/task-logs/2025-12-16_attend-history-name-consistency.md`: log deste ajuste com hipótese/decisão e comandos.
- `.sangoi/CHANGELOG.md`: entrada 2025-12-16 para o ajuste de naming no histórico.
- `.sangoi/handoffs/HANDOFF_2025-12-16-attend-history-name-consistency.md`: handoff versionado desta alteração.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite (host real) o caso reportado e confirmar que o histórico agora alinha com lista/resumo em chat 1:1.

## 5) Relevant notes
- Se o backend estiver retornando `profileName` “histórico” (ou por participante) em chat 1:1, a UI passa a preferir o “nome da conversa” para consistência.

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-16_attend-history-name-consistency.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-16

Date (UTC‑3): 2025-12-16T09:46  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-16_attend-background-polling.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: fazer o polling do `/attend` rodar “em background” e evitar atualizar o DOM quando não existe mudança relevante.
- Delivered: timer em `setTimeout` (stop via `visibilitychange`, sem `focus/blur`), fingerprint da lista para evitar `conversations.value = …` quando nada mudou e refresh de detalhe sem ligar `detailLoading`.
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui test`

## 1) What I did (precise and auditable)
- Refatorei o polling do `/attend` para não depender de `window.focus/blur` e não “piscar” loaders durante auto-refresh.
- Adicionei detecção de “lista mudou?” via fingerprint para não reatribuir a lista quando os dados são idênticos.
- Ajustei refresh de detalhe em polling para rodar sem `detailLoading` e só substituir `selectedConversation` quando há mudança relevante.
- Atualizei planning/task-logs/changelog para manter a trilha rastreável.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/views/SuiteletAttend.vue`: polling em background (timer por `setTimeout`), stop/resume via `visibilitychange`, skip de update quando fingerprint não muda e refresh de detalhe sem ligar `detailLoading`. Expected impact: menos re-render/flicker e menos “thrash” quando não há mensagens novas.
- `.sangoi/planning/2025-12-16_attend-followups.md`: item de polling marcado como entregue e backlog opcional registrado. Impacto: checklist reflete o estado real.
- `.sangoi/task-logs/2025-12-16_attend-background-polling.md`: log do trabalho com decisões e comandos. Impacto: auditoria/replay.
- `.sangoi/task-logs/2025-12-16_attend-history-polling.md`: follow-up atualizado para apontar para o log novo. Impacto: contexto não fica órfão.
- `.sangoi/CHANGELOG.md`: nova entrada 2025-12-16 para a melhoria de polling. Impacto: mudança visível para maintainers.
- `.sangoi/handoffs/HANDOFF_2025-12-16-attend-background-polling.md`: handoff versionado desta alteração.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite (host real) o fluxo do `/attend` com a view aberta por alguns minutos e confirmar: sem flicker, sem reload quando nada mudou, e atualização quando chega mensagem nova.
2. Opcional: extrair polling/diff para um composable e/ou cache Map por conversa se o merge/dedup virar gargalo.

## 5) Relevant notes
- Stop/resume do auto-refresh agora depende apenas de `document.visibilityState` (sem `focus/blur`).
- O fingerprint foi desenhado para cobrir campos que impactam UI/ordenação; se aparecer drift, ajustar a composição do fingerprint antes de inventar cache novo.

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-16_attend-background-polling.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-16
- Planning: `.sangoi/planning/2025-12-16_attend-followups.md`

Date (UTC‑3): 2025-12-16T02:24  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-16_attend-followups.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: registrar follow-up explícito para evoluir o polling do `/attend` para “background + update only on change”.
- Delivered: checklist de análise adicionada ao planning e linkada no task-log do fix anterior.
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui test`

## 1) What I did (precise and auditable)
- Atualizei o planning do Attend para incluir uma análise estruturada de pooling em background e atualização do DOM apenas quando houver novas mensagens.
- Linkei o follow-up no task-log do fix de histórico/polling para não perder a intenção.

## 2) File‑by‑file changes (why before how)
- `.sangoi/planning/2025-12-16_attend-followups.md`: novo item de checklist “(Análise) pooling em background + DOM update on change”. Impacto: próxima sessão tem trilho claro.
- `.sangoi/task-logs/2025-12-16_attend-history-polling.md`: adicionada nota de follow‑up apontando para o planning. Impacto: contexto fica rastreável.
- `.sangoi/handoffs/HANDOFF_2025-12-16-attend-polling-background-followup.md`: handoff versionado desta atualização de docs.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Executar a análise do checklist e propor desenho final (cursor/hash/cache + merge/dedup de events).
2. Se aprovado, implementar a refatoração (provavelmente extraindo polling para um composable/store e reduzindo re-render na lista/timeline).

## 5) Commands I ran (with short outcome)
- Nenhum (apenas docs).

## 6) Useful links (no duplication)
- Planning: `.sangoi/planning/2025-12-16_attend-followups.md`
- Task‑log anterior: `.sangoi/task-logs/2025-12-16_attend-history-polling.md`

Date (UTC‑3): 2025-12-16T02:16  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-16_attend-history-polling.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: parar de misturar histórico entre atendimentos no `/attend` e reduzir o “thrash” de polling/loading.
- Delivered: `conversation_detail` agora aceita `protocol` e parseia `/messages/protocol/{protocol}`; `/attend` passa protocolo no detalhe, evita duplo load e ignora respostas fora de ordem; refresh de detalhe só quando há nova atividade.
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui run build:ns`

## 1) What I did (precise and auditable)
- Corrigi o caminho do backend que sempre caía em fallback por telefone por tratar `protocol_history` como JSON já parseado; agora parseia `response.body` com `parseJsonSafe` e usa `protocol` vindo do cliente quando disponível.
- Refatorei o `/attend` para: (a) enviar `protocol` ao carregar detalhe, (b) paralelizar o refresh da lista, (c) eliminar duplo load (watch + refresh), (d) proteger contra respostas fora de ordem e (e) reduzir reload de detalhe no polling.

## 2) File‑by‑file changes (why before how)
- `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/admin/ab_admin_dashboard_sl.js`: `fetchConversationDetail` aceita `protocol` (query), tenta histórico por protocolo primeiro e faz parse seguro da resposta HTTP de `protocol_history` (antes lia `messages/data` direto e vinha vazio). Impacto: histórico do atendimento humano deixa de misturar conversas do mesmo telefone quando há protocolo.
- `ab_vue_ui/src/services/api.ts`: `getConversationDetail` aceita `protocol?: string` e envia na query. Impacto: contrato para o backend suportar histórico por protocolo sem depender de lookup de fila.
- `ab_vue_ui/src/views/SuiteletAttend.vue`: passa `protocol` do metadata, detecta `protocol`/`protocolo` no merge/dedup, paraleliza refresh, remove timer global, evita duplo load e ignora respostas fora de ordem. Impacto: troca de conversas e auto-refresh não “embaralham” a timeline e reduzem chamadas desnecessárias.
- `.sangoi/task-logs/2025-12-16_attend-history-polling.md`: diagnóstico + alterações + comandos.
- `.sangoi/planning/2025-12-16_attend-followups.md`: checklist atualizado.
- `.sangoi/CHANGELOG.md`: entrada 2025-12-16.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite: abrir dois atendimentos distintos do mesmo telefone (protocolos diferentes) e confirmar que o histórico não mistura eventos. Expected: timeline muda corretamente ao alternar e mantém eventos do protocolo selecionado.
2. Validar polling: deixar `/attend` aberto por alguns minutos e confirmar que o detalhe só recarrega quando há nova atividade (sem piscadas/loads duplicados).

## 5) Relevant notes
- O backend ainda tem fallback por telefone quando não existe `protocol` (ou quando `/messages/protocol` vem vazio). Com este patch, o `/attend` passa `protocol` sempre que estiver no metadata para reduzir esse caminho.

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

`npm --prefix ab_vue_ui run lint:css-guard`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-16_attend-history-polling.md`
- Planning: `.sangoi/planning/2025-12-16_attend-followups.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-16

Date (UTC‑3): 2025-12-16T01:27  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-15_attend-whatsapp-upload-and-attachments.md#o-que-foi-feito`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: alinhar o `/attend` ao legado WhatsApp (sem CB/chatbot), com envio manual + anexos e layout que comporte todas as ações.
- Delivered: upload base64 via `/upload`, envio via `/api/whatsapp-gupshup/direct` (payload `ResponseMessaging`), parser de tokens `image/file/audio/video`, e grid ajustado (filtros full-width + sidepanel maior).
- In progress: nada.
- Next immediate step (single command): `npm --prefix ab_vue_ui run build:ns`

## 1) What I did (precise and auditable)
- Troquei o upload do Attend do proxy v2 para o upload legado via integration-app (`POST /upload` com `Internal: CIntegration`). Evidence: `.sangoi/task-logs/2025-12-15_attend-whatsapp-upload-and-attachments.md#o-que-foi-feito`.
- Reescrevi o envio manual para falar com o endpoint legado do provedor (`POST /api/whatsapp-gupshup/direct`) usando payload `ResponseMessaging`, suportando texto + anexos por tokens. Evidence: `.sangoi/task-logs/2025-12-15_attend-whatsapp-upload-and-attachments.md#o-que-foi-feito`.
- Ajustei o layout do `/attend` para caber filtros completos e o card de template/preview no sidepanel sem quebrar ações. Evidence: `.sangoi/task-logs/2025-12-15_attend-layout-accommodate-features.md#mudanças`.

## 2) File‑by‑file changes (why before how)
- `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/admin/ab_admin_dashboard_sl.js`: `conversation_manual_upload` + `conversation_manual_message` agora usam endpoints legados e payload compatível com o provedor; `userId` resolve via `/auth/user`. Impacto: anexos + envio manual funcionam no legado.
- `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/api/endpoints.js`: novo helper `upload_base64()` para `/upload` com bypass `Internal: CIntegration`. Impacto: upload sem depender de v2.
- `ab_vue_ui/src/services/api.ts`: upload lê `File` → base64 e chama action `conversation_manual_upload` (JSON). Impacto: sem multipart/v2.
- `ab_vue_ui/src/views/SuiteletAttend.vue`: filtros `Conta/Agente` expostos; grid e ações ajustados; anexos enviados como `url` e timeline interpreta tokens. Impacto: UI suporta fluxo completo sem apertos.
- `ab_vue_ui/src/styles/suitelet-dashboard.css`: `attend-layout` com filtros full-width e sidepanel dimensionado; ações com wrap; preview com altura/scroll. Impacto: layout mais robusto.
- `.sangoi/task-logs/2025-12-15_attend-whatsapp-upload-and-attachments.md` e `.sangoi/task-logs/2025-12-15_attend-layout-accommodate-features.md`: logs detalhados.
- `.sangoi/CHANGELOG.md`: entradas 2025-12-15 atualizadas.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar no NetSuite (ambiente real) envio de: (a) texto, (b) imagem+caption, (c) pdf, (d) áudio/vídeo. Expected output: mensagens aparecem no histórico do legado e no `/attend`.
2. Se aparecer “Permission denied”/`Invalid token`, revisar credenciais do legado e roles (`Interactivity:Direct:Send`) do usuário do token de sessão. Expected output: `/api/whatsapp-gupshup/direct` retornando `{ messageId }`.
3. (Backlog) Reduzir `analysis/css-usage.json` (`Unused classes detected`) — fora do escopo desta rodada; rastrear no índice de CSS/UI. Unblock by: varrer views/estilos e remover seletores mortos com evidência.

## 5) Relevant notes
- O envio via provedor exige `conversationId` numérico (DisabledUser id). Conversas do modo interatividade (id não numérico) retornam erro explícito.
- Guardrails: `css-conflicts=0`, mas `css-usage` ainda aponta classes não usadas preexistentes (não mexi nisso aqui).

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run build:ns`
Result: ✅

`npm --prefix ab_vue_ui run lint:css-guard`
Result: ✅

`node .sangoi/.tools/css-conflict-report.mjs`
Result: ✅ (Conflicts: 0)

## 7) Useful links (no duplication)
- Task‑log (upload/anexos): `.sangoi/task-logs/2025-12-15_attend-whatsapp-upload-and-attachments.md`
- Task‑log (layout): `.sangoi/task-logs/2025-12-15_attend-layout-accommodate-features.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-15
- Backlog CSS/UI: `.sangoi/open-tasks-index.md#4-atendimentos-atendimentos-viewpng`

Date (UTC‑3): 2025-12-12T13:10  •  Author: Codex  •  Anchor: `.sangoi/task-logs/2025-12-12_attend-width-spikes.md#mudança-aplicada`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: eliminar “spikes” de largura entre colunas central/direita no `/attend`.
- Delivered: estabilização do grid (min-width/min-content + wrap de tokens), `.attend-column--list` aplicado e remoção do último `:style` inline no Attend.
- In progress: nada.
- Next immediate step (single command): `git pull`

## 1) What I did (precise and auditable)
- Identifiquei que o reflow vinha de min-content/min-width e tokens longos no painel direito, somado a auto-placement no grid. Evidence: `.sangoi/task-logs/2025-12-12_attend-width-spikes.md#causa-raiz`.
- Ajustei CSS de `.attend-layout`/filhos para permitir encolhimento e quebra de tokens longos (`overflow-wrap`). Evidence: `.sangoi/task-logs/2025-12-12_attend-width-spikes.md#mudança-aplicada`.
- Removi `:style` inline remanescente no botão de filtros e amarrei a lista ao `grid-area: list` via `.attend-column--list`. Evidence: `.sangoi/task-logs/2025-12-12_attend-width-spikes.md#mudança-aplicada`.
- Validei com build/test no bundle. Evidence: comandos abaixo.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/styles/suitelet-dashboard.css`: garantido encolhimento do grid e quebra de tokens para impedir que o painel direito force expansão/reflow. Impacto: colunas central/direita estáveis ao trocar conversa.
- `ab_vue_ui/src/views/SuiteletAttend.vue`: aplicado `.attend-column--list` + removido `:style` de `--field-span`. Impacto: grid-area consistente e zero inline styles.
- `.sangoi/task-logs/2025-12-12_attend-width-spikes.md`: log completo do diagnóstico + patch.
- `.sangoi/open-tasks-index.md`: anotado progresso no item de Attend.
- `.sangoi/CHANGELOG.md`: item 2025-12-12 atualizado para o fix do Attend.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Validar visualmente no NetSuite (ambiente real) com conversas de conteúdo longo. Expected output: sem “pulos” entre colunas.
2. Se ainda houver drift de proporção, seguir o checklist de alinhamento do Attend em `.sangoi/open-tasks-index.md#4-atendimentos-atendimentos-viewpng`.

## 5) Relevant notes
- Fix limitado ao Attend (CSS + classe); não alterei tokens globais nem outras views.
- Commit funcional já em `master`: `d29234b`.

## 6) Commands I ran (with short outcome)
`npm --prefix ab_vue_ui test`
Result: ✅

`npm --prefix ab_vue_ui run lint:css-guard`
Result: ✅

`npm --prefix ab_vue_ui run build`
Result: ✅

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-12_attend-width-spikes.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-12
- Planning (QA): `.sangoi/planning/2025-12-12_attend-layout-stability.md`
- Backlog: `.sangoi/open-tasks-index.md#4-atendimentos-atendimentos-viewpng`

## 2025-12-12 — Codex (Sandbox bundle + templates PROVIDER_GS)
Resumo
- Handoff em `.sangoi/handoffs/HANDOFF_2025-12-12-sandbox-filecabinet-and-templates.md` cobre o diagnóstico do bundle ausente/deletado no sandbox e as mudanças para rastrear a falta via UE.
- Envio de templates WhatsApp foi alinhado ao contrato legado: `send_single_template`/`send_multi_template` usam `/api/messages/multi` com `messages[]`, números sem `+`, e logs completos de payload para depurar 500.
- Sync de templates agora também inativa registros locais removidos do provedor e reativa quando voltam; listagens/envios filtram `isinactive=F`.

## 2025-12-05 — Codex (Suitelet Attend — filtros e layout)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-12-05-suitelet-attend-filters-and-layout.md` descrevendo a refatoração estrutural da view `/attend`: filtros em card dedicado (`attend-filters`), lista de conversas em card próprio (`attend-column--list`), painel central mantendo histórico/composer e sidepanel com resumo+template.
- O layout da aba passou a ser controlado exclusivamente por `attend-layout` (CSS Grid em 3 colunas), sem `ab-main-grid` intermediário, alinhando o comportamento visual ao resto da suitelet sem introduzir um novo shell paralelo. Ficou registrado follow‑up explícito para validar o comportamento em NetSuite (grid real + estilos do host) e investigar o bug intermitente de “card entrando por baixo” reportado no ambiente do usuário.

## 2025-12-05 — Codex (SuiteApp AstenBot only)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-12-05-suiteapp-astenbot-only.md` documentando a remoção do modo greenfield no SuiteApp (Suitelet Admin, MR de envio em lote e Vue) e a consolidação de todos os calls HTTP no backend AstenBot via `endpoints.js`/`legacy_api.js`.
- Suitelet Admin deixou de montar paths de API na mão (`/api/messages/single`, `/disabled/user/**`, `/contacts/**`, `/whatsapp/provider/**`) e agora usa apenas helpers `endpoints.api.*`/`endpoints.send_single_template`; o frontend deixou de tentar `/v2/*` e a UI de Dev não oferece mais toggle entre backends, só HMR/validação relaxada.

## 2025-12-03 — Codex (Campaign Dashboard & Buttons)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-12-03-suitelet-campaign-dashboard-and-buttons.md` cobrindo o fix do botão de voltar na dashboard de campanhas (`SuiteletCampaignDashboard.vue`) e o alinhamento do sistema de botões da suitelet (`Button`/`ab-btn` + tokens `--ab-button-*`) com o padrão já usado pela fila (danger-fill/primary-alt).
- Botões de Config (`AdminApiTab.vue`) e Régua (`SuiteletRegua.vue` + `ReguaEditor.vue`) passaram a usar o componente `<Button>` com variants consistentes e tokens ajustados para idle suave, hover laranja e active com `scale(1.05)` + borda laranja, mantendo foco acessível e comportamento uniforme entre views.

## 2025-12-04 — Codex (Suitelet dashboards → ab-main-grid)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-12-04-suitelet-dashboards-ab-main-grid.md` documentando a migração das views `SuiteletCampaignDashboard.vue` e `SuiteletReguaDashboard.vue` para o layout base do DashboardSurface (`<section class="ab-main-grid">` com `ab-panel--queue` + `ab-main-secondary`).
- As colunas laterais que antes usavam `rg-layout`/`rg-aside` foram convertidas em painéis `ab-panel--summary` dentro de `div.ab-main-secondary`, reaproveitando os mesmos componentes/cards (`rg-card`, `rg-automation`, `rg-totals`) sem alterar contratos de API.

## 2025-12-03 — Codex (Dashboard fila = Attend)
Resumo
- A fila da home agora usa o mesmo merge do `/attend`: `listConversations` + `listInteractivityConversations` com dedup por protocolo ou telefone+canal, priorizando a fonte “queue”.
- Cards da fila mostram `profileName`/`name` quando houver, status a partir de `handoffState`/`statusRaw` e canal real (não expõem sessionId como telefone).
- `open-tasks-index` ganhou item para implementar cache local do dashboard conforme o plano em `.sangoi/task-logs/2025-12-02-admin-dashboard-cache-design-and-impl.md`.

# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-12-03T15:10  •  Author: Codex  •  Anchor: `.sangoi/open-tasks-index.md#backend--infra-de-dashboard`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: alinhar a fila do dashboard com a lista do `/attend` e acabar com duplicatas/nomes errados.
- Delivered: merge e dedup da fila na home, cards com nome/status/canal corretos, e tarefa aberta para cache local.
- In progress: nada em andamento.
- Next immediate step (single command): `node buildAndDeploy.js --fast-bundle` (ainda não rodado; repo sujo).

## 1) What I did (precise and auditable)
- Alinhei o merge da fila na dashboard com o `/attend` (queue + interactivity, dedup por protocolo/telefone). Evidence: `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`.
- Ajustei cards da fila para priorizar `profileName`/`name`, status real e canal real. Evidence: `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`.
- Removi duplicatas na lista do `/attend` (mesma chave de protocolo/telefone). Evidence: `ab_vue_ui/src/views/SuiteletAttend.vue`.
- Abri tarefa para cache local do dashboard no índice. Evidence: `.sangoi/open-tasks-index.md#backend--infra-de-dashboard`.

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`: fila da home agora consome queue+interactivity com dedup; cards usam nome/status/canal corretos; IDs usam `DisabledUser.id` quando disponível. Impacto: dashboard mostra as mesmas conversas do `/attend` sem duplicatas.
- `ab_vue_ui/src/views/SuiteletAttend.vue`: merge fila+interatividade com chave protocolo/telefone, priorizando fila. Impacto: remove conversas duplicadas na coluna esquerda.
- `.sangoi/open-tasks-index.md`: adicionada tarefa para implementar cache local do dashboard, referenciando plano existente. Impacto: backlog rastreável.

## 3) In progress (clear stop point)
- Nenhum item em andamento.

## 4) Follow‑up (execution order)
1. Rodar `node buildAndDeploy.js --fast-bundle` (depende de resolver a árvore suja) para validar build. Esperado: sem TS errors.
2. Implementar cache local conforme `.sangoi/task-logs/2025-12-02-admin-dashboard-cache-design-and-impl.md`. Esperado: redução de chamadas ao legado com TTL e invalidação pós-handoff.
3. Se ainda houver divergência de contagens, alinhar `QueuePanel`/`QueueHealthCard` para usar status mapeado (Aguardando/Em atendimento/Concluído) da nova fila.

## 5) Relevant notes
- Repo está com muitas mudanças pré-existentes (git status sujo); não limpei nada além dos arquivos tocados.
- Build anterior falhou por função não usada; função removida, mas build não foi reexecutado.

## 6) Commands I ran (with short outcome)
Nenhum comando relevante nesta rodada (apenas edições).

## 7) Useful links (no duplication)
- Tarefa de cache: `.sangoi/open-tasks-index.md#backend--infra-de-dashboard`
- Plano de cache: `.sangoi/task-logs/2025-12-02-admin-dashboard-cache-design-and-impl.md`

## 2025-12-02 — Codex (Suitelet Config — API & OmniView)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-12-02-suitelet-config-api-omniview.md` consolidando a reorganização da aba Config: shell em grid com abas (“API & OmniView”, “Ferramentas”, “Dev”), cards `ab-card` para credenciais e registros, e dois painéis lado a lado na aba principal (Configurações de API + Registros OmniView).
- A aba “Registros” foi aposentada; Registros OmniView agora vivem como card dentro de um painel próprio em `API & OmniView`. Broker de IA e Verificação rápida saíram da UI (lógica mantida no script), e Tools/Dev passaram a usar `ab-panel__stack` + `ab-card` para manter o mesmo vocabulário visual da dashboard.

## 2025-11-27 — Codex (WhatsApp tab, templates e shell das views)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-11-27-whatsapp-and-dashboard-ui.md` consolidando as mudanças da aba WhatsApp (status textual, descrição baseada no texto da mensagem, remoção da coluna de variáveis), o fallback de secret (`custsecret_as_api_aes_key` → `custsecret_{{AB_TAG}}_api_aes_key`), o sync automático de templates com o provedor e os ajustes de shell das views (breadcrumb, bordas, campo de destinatários).
- `ab_buttons_ue.js` agora usa o relatório `/reports/messages/sentmessages` para popular a aba WhatsApp com data/hora, ID, número de origem, template, status e descrição (texto real), enquanto `ab_admin_dashboard_sl.js` garante que `get_templates`/`sync_templates` sincronizam `customrecord_{{AB_TAG}}_wpp_templates` com `/whatsapp/provider/template/byaccount/{accountId}` em qualquer `apiMode`, com logs `ADMIN:syncProviderTemplates.*`/`ADMIN:get_templates.sync_summary`.
- A UI (Vue) passou a mostrar “Carregando templates…” e a desabilitar selects de template até o fim de `getTemplates()`, alinhou o card `.ab-surface` ao `.ab-container` (mesmas bordas/sombra/padding) e simplificou o campo de destinatários com um único wrapper de input + chips.

## 2025-11-26 — Codex (SuiteApp mensagem avulsa & templates)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-11-26-suiteapp-message-template-send.md` documentando o fluxo de envio de template WhatsApp (Gupshup) pela SuiteApp, alinhado aos endpoints legado (`/api/messages/single`, `/api/messages/multi`) e à configuração de número de origem em `customrecord_{{AB_TAG}}_api_info`.
- Configuração centralizada de credenciais + número de origem (`waSourceNumber`) via Admin, com leitura única em `AB/auth` e reuso por Suitelet, helpers `AB/endpoints` e Map/Reduce de envio em lote.
- Suitelet de mensagem avulsa passou a logar payloads/respostas de envio (`ADMIN:send_single_template.*`), usar `sendMethod: "PROVIDER_GS"` e path `/api/messages/single`, e deixou de exigir variáveis em templates que não precisam de `{{1}}`/`{{2}}`.

## 2025-11-24 — Codex (SuiteApp legacy interactivity conversations)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-11-24-suiteapp-legacy-interactivity-conversations.md` detalhando como a SuiteApp deve expor um “modo histórico” de conversas usando apenas a API legacy (`/api/interactivity/messages` e opcionalmente `/api/interactivity/history`), sem depender de `/v2`.
- O documento define o contrato `ConversationState` para esse modo, descreve os helpers esperados em `AB/endpoints` (`interactivityMessages`, `interactivityHistory`), a ação GET `conversations_interactivity_list` no Suitelet `ab_admin_dashboard_sl.js` e o helper Vue `listInteractivityConversations` em `ab_vue_ui/src/services/api.ts`.
- Referências de payloads/autenticação e o inventário completo de endpoints legado continuam living no backend (`~/work/ab-greenfield/.sangoi/legacy/docs/backend-endpoints-used-by-suiteapp.md`, `backend-endpoints-inventory.md`, `endpoints-legacy-all-manifest.json`), e o handoff amarra essas fontes para o time NetSuite.

# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-11-19T16:45  •  Author: Codex  •  Anchor: `.sangoi/CHANGELOG.md#2025-11-19`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: deixar o atalho de atendimentos acessível pela dashboard e simplificar o painel lateral do SuiteletAttend.
- Delivered: quick card “Atendimentos”, card único de “Resumo do atendimento” no aside e reescala de todas as medidas em `rem` (0.8×) nos CSS + rebuild do inline styles.
- In progress: validar o preview da fila (logs/ fallback por telefone) e acertar o deploy `pwsh.exe` bloqueado.
- Next immediate step (single command): `npm run build:deploy`

## 1) What I did (precise and auditable)
- Criei o handler `handleNavigateAttend()` e um quick card específico em `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`, permitindo abrir `/attend` direto da home.
- Removi o bloco “Metadados/JSON completo”, cards de RAG e guardrails do SuiteletAttend e adicionei o card “Resumo do atendimento”, alimentado por `summaryCard` (cliente, telefone, status, fila, canal, protocolo, última atualização).
- Rodei um script Python que multiplica todas as ocorrências de `rem` em `ab_vue_ui/src/**/*.css` por 0.8, gerei novamente `inline-styles.generated.ts` e atualizei o bundle via `npm run build:deploy` (build ok, deploy bloqueado por `pwsh.exe`).

## 2) File‑by‑file changes (why before how)
- `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`: adicionei `handleNavigateAttend` e um novo `<article>` nas quick cards porque o time pediu um acesso direto à view de atendimento.
- `ab_vue_ui/src/views/SuiteletAttend.vue`: troquei os cards RAG/guardrail pelo novo card “Resumo do atendimento” e removi o bloco “Ver JSON completo” para evitar ruído; agora o aside mostra apenas os dados essenciais da conversa ativa.
- `ab_vue_ui/src/assets/app.css` + `ab_vue_ui/src/styles/**/*.css` + `inline-styles.generated.ts` + `SuiteScripts/admin/overlay-shell.css`: todas as medidas em `rem` foram reescaladas em 0.8× e o módulo inline foi regenerado para manter o bundle em sincronia.

## 3) In progress (clear stop point)
- Task: validar preview da fila com fallback por telefone  •  Area: `ab_admin_dashboard_sl.js` + `DashboardSurface.vue`
- What’s done: logs `[QueuePreview] …` no front, `ADMIN_DASHBOARD:queue_preview:*` no Suitelet e fallback automático para `/disabled/user/history` por `phoneNumber` quando o `disabledUserId` não traz histórico.
- Still missing: coletar uma resposta real (Network tab) para confirmar o shape e ajustar o parser caso `data` não seja array.
- Acceptance criteria on resume: hover da fila mostrando as últimas mensagens (cliente/agente/bot) com dados reais e sem ficar preso em “Sem mensagens recentes”.

## 4) Follow‑up (execution order)
1. Executar `npm run build:deploy` em ambiente com `pwsh.exe` disponível (ou trocar o runner por `pwsh` no WSL). Expected output: bundle publicado no NetSuite.
2. Revisar os logs `[QueuePreview]`/`ADMIN_DASHBOARD:queue_preview:*` após o QA (com hovering real) e, se `data` ainda vier vazio, capturar o payload e ajustar o frontend. Expected output: preview confiável.
3. Se necessário, documentar o novo card e o reescalonamento de rem em `.sangoi/CHANGELOG.md` (sessão UI) ou abrir um follow-up para ajustes finos de layout. Expected output: registro visível para o time de design.

## 5) Relevant notes
- Deploy continua travando no `pwsh.exe` chamado pelo `buildAndDeploy.js`; precisa rodar fora do WSL ou instalar o PowerShell CLI.
- O script de reescala processou todos os CSS em `ab_vue_ui/src`; se alguém editar `rem` manualmente, precisa reexecutá-lo para manter consistência.

## 6) Commands I ran (with short outcome)
`python - <<'PY' …` (script que multiplica `rem` por 0.8)
Result: ✅ — arquivos listados em stdout (`app.css`, `styles/*.css`, `overlay-shell.css`).

`node ../.sangoi/.tools/build-inline-styles.mjs`
Result: ✅ — reescreveu `ab_vue_ui/src/bootstrap/inline-styles.generated.ts`.

`npm run build:deploy`
Result: ❌ — build ok, mas deploy falhou com `spawnSync pwsh.exe EACCES` (permissão no PowerShell).

## 7) Useful links (no duplication)
- Commits: `feat(attend): add attend shortcut and summary card`, `style(css): scale rem dimensions by 0.8`
- Changelog: `.sangoi/CHANGELOG.md` (entradas 2025-11-19)
- Arquivos tocados: `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`, `ab_vue_ui/src/views/SuiteletAttend.vue`, `ab_vue_ui/src/styles/*.css`


# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-11-19T09:10  •  Author: Codex (assistente)  •  Anchor: `.sangoi/task-logs/2025-11-19_dashboard-queue-row-tuning.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: alinhar a dashboard com os endpoints legados do Asten Bot, limpando a fila e conectando o preview ao histórico real.
- Delivered: `buildQueueRows` usando unidade/departamento em “Atividade”, dashboard sem coluna “Última mensagem”, preview alimentado por `disabled_user/history` e doc de endpoints (`*-live-checks`) com respostas reais da conta 72.
- In progress: refinamento do autor do preview (client/agent/bot) e possíveis filtros adicionais na dashboard/Attend.
- Next immediate step (single command): `sed -n '1,260p' .sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md`

## 1) What I did (precise and auditable)
- Ajustei a montagem de linhas da fila em `ab_admin_dashboard_sl.js` para priorizar unidade/departamento em `atividade` e tratar “Mensagem Avulsa”/`wa-*` como ruído. Evidence: `.sangoi/task-logs/2025-11-19_dashboard-queue-row-tuning.md`.
- Sincronizei o preview da fila com o legado via nova ação `queue_preview`, que chama `endpoints.api.disabledUserHistory` e devolve `ConversationMessage[]` consumido por `DashboardSurface.fetchRowPreview`. Evidence: mesmo task‑log, seção “Suitelet Admin + Dashboard preview”.
- Atualizei `.sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md` com exemplos reais (200 OK) para fila, histórico, contatos e templates (`byrange`, `history/messages`, `contacts/byphone`, `whatsapp/provider/pages`, `whatsapp/provider/template`).

## 2) File‑by‑file changes (why before how)
- `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/admin/ab_admin_dashboard_sl.js`: refinei `buildQueueRows` (cliente, número, atividade, status, canal/fila) e adicionei a ação `queue_preview` em `handlePost`, que mapeia `DisabledUserFilter` → `disabledUserHistory` →
`ConversationMessage[]`. Expected impact: fila mais legível e preview atrelado ao histórico real, sem depender do backend novo.
- `ab_vue_ui/src/features/dashboard/components/QueuePanel.vue`: removi a coluna “Última mensagem”, mantive hover em Cliente/Número/Atividade/Status e acrescentei `onContextMenu`/menu contextual leve. Expected impact: tabela mais enxuta, preview concentrando o texto e menu com
ações (“Abrir no atendimento”, “Ver detalhes rápidos”, “Copiar número/protocolo”).
- `ab_vue_ui/src/features/dashboard/DashboardSurface.vue`: passei `rowsState` para `ConversationQueueRow`, implementei `fetchRowPreview` via `getQueuePreview`, acrescentei estado do context menu/modal de detalhes e alimentei `pendingConversation` para o `SuiteletAttend`.
Expected impact: home do dashboard continua leve, mas a navegação para o Attend e o preview de mensagens agora usam o mesmo contexto que vem da fila.
- `ab_vue_ui/src/services/api.ts`: criei `getQueuePreview(suiteletUrl, { disabledUserId, phoneNumber, chatType })` e tipagem de `DashboardStats` continua coerente com o payload novo. Expected impact: ponto de entrada único no front para o preview legado.
- `.sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md`: adicionei seções para `/disabled/user/messages`, `/disabled/user/history` por telefone, `/contacts/byphone`, `/whatsapp/provider/pages/byaccount`, `/whatsapp/provider/template/byaccount` com exemplos reais
(conta 72). Expected impact: referência rápida para payloads/respostas no host legado `sms-main-module/rest`.
- `.sangoi/task-logs/2025-11-19_dashboard-queue-row-tuning.md` + `.sangoi/CHANGELOG.md`: registrei as mudanças da fila, preview e docs para que o histórico fique visível para o time.

## 3) In progress (clear stop point)
- Task: afinamento do autor do preview (client/agent/bot)  •  Area: `ab_admin_dashboard_sl.js` + `DashboardSurface.vue`
- What’s done: já capturamos `ReplyDataPassive` de `/disabled/user/history` e mapeamos `message/messageCategory/receiveTime` para `ConversationMessage`.
- Still missing: inspecionar campos que diferenciem cliente/agente/bot no payload legado e ajustar `author/name` no preview sem quebrar o Attend.
- Acceptance criteria on resume: hover da fila mostra claramente se a mensagem veio do cliente, do atendente ou do bot, usando apenas dados do legado (sem depender do backend v2).

## 4) Follow‑up (execution order)
1. Inspecionar `ReplyDataPassive` em mais cenários (open/attending/disabled) e identificar o campo correto para direção/autoria. Expected output: mini‑ADR ou nota em `.sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md` documentando o mapeamento.
2. Ajustar `queue_preview` para setar `author`/`name` com base na direção (cliente/agente/bot) e, se viável, incluir um flag leve de erro (ex.: quando `messageStatus` indica falha). Expected output: preview da fila mais expressivo, sem mudar a estrutura de
`ConversationMessage`.
3. Se o SuiteletAttend precisar, expor um endpoint leve para “últimas mensagens por protocolo/phone” reaproveitando o mesmo shape, evitando duplicação de lógica entre dashboard e Attend. Expected output: Attend consumindo o mesmo contrato de histórico que o preview.

## 5) Relevant notes
- Informal decision: dashboard home continua só como lista/overview (fila + gráficos); SuiteletAttend permanece como lugar certo para “vista clínica” de atendimento. Pré-visualizações e detalhes rápidos na home são intencionais, mas não substituem a tela dedicada. Logged in
`.sangoi/task-logs/2025-11-19_dashboard-queue-row-tuning.md`.
- Trap: `/service/config/byaccount/` no host legado responde com erro de Jackson se o DTO não bater exatamente com o que o backend espera; não documentei esse endpoint em `*-live-checks` para não poluir a referência com casos 4xx/5xx.
- Sensitive: credenciais usadas para login curto (`integracaonetsuite@astentech.com.br` / senha) permanecem fora dos arquivos de código; apenas exemplos de payloads/respostas foram registrados em `.sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md`.

## 6) Commands I ran (with short outcome)
`curl -sS https://avmb.../rest/api/auth/login -d '{"email":"integracaonetsuite@astentech.com.br","password":"…"}'`
Result: ✅ obteve `Token: <session-token>` usado em todas as chamadas de legado.

`curl -sS https://avmb.../rest/disabled/user/byrange -H "Token: $TOKEN" -d '{"accountId":72,"status":"ENABLED","initialRange":0,"finalRange":49}'`
Result: ✅ retornou 6 atendimentos reais (conta 72), base para fila e exemplos de docs.

`curl -sS https://avmb.../rest/disabled/user/history -H "Token: $TOKEN" -d '{"account":{"@type":"Company","id":72},"phoneNumber":"5549999616563","chatType":"PB","startDate":"2020-01-01T00:00:00-03:00","endDate":"2025-12-31T23:59:59-03:00","orders":
["receiveTime"],"orderType":"desc"}'`
Result: ✅ retornou 9 itens `ReplyDataPassive` com mensagens reais, usados para o design do preview.

`curl -sS https://avmb.../rest/whatsapp/provider/template/byaccount/72 -H "Token: $TOKEN"`
Result: ✅ retornou 13 templates (`WhatsAppProviderTemplate`), um deles documentado com texto completo no live‑checks.

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-11-19_dashboard-queue-row-tuning.md`
- Changelog: `.sangoi/CHANGELOG.md` (entrada “Dashboard fila (dashboard_stats)” em 2025-11-19)
- Live endpoints: `.sangoi/reference/api/astenbot-endpoints-live-checks-2025-11-19.md`
- Admin AGENTS: `src/FileCabinet/SuiteApps/com.avmb.astenbot/SuiteScripts/admin/AGENTS.md`
- Dashboard AGENTS: `ab_vue_ui/src/features/dashboard/AGENTS.md`, `ab_vue_ui/src/features/dashboard/components/AGENTS.md`

## 2025-11-09 — Codex (WA SLA watchdog validation)
Resumo
- Adicionei a migration `20251109_003_conversation_sla_real.sql` para transformar `agent_sla_minutes` em `REAL` e reexportei o Bull worker (copiando os tipos + `withTenantById` com `search_path`) para destravar o build.
- Resetei o banco (`npm run db:reset`) e subi `docker compose` com backend/bull/slack-mock; injetei um inbound `metadata.sla_minutes=0.05` via `/v2/internal/whatsapp/inbound` e confirmei `agent_sla_due_at` preenchido.
- O watchdog `wa-handoff-sla` rodou em ~3 s, marcou `agent_sla_breached_at/notified_at` e publicou no Slack mock (`tenant=demo conversa=05e6bbb6-f172-45f4-bb05-191d5ef24bae`), fechando a pendência da Fase E.

Evidências
- Handoff dedicado: `docs/handoff/HANDOFF_2025-11-09-wa-sla.md`.
- Migração nova: `apps/postgres/migrations/20251109_003_conversation_sla_real.sql`.
- Worker fix: `apps/bull/src/db.ts`, `apps/bull/Dockerfile`.
- Slack mock: `docker compose logs --since 1m slack-mock` → payload com `:rotating_light: SLA aguardando agente excedido (0.05 min)`.
- SQL: `docker compose exec postgres psql -U asten -d astenbot -c "SET app.tenant='demo'; SELECT id, agent_sla_minutes, agent_sla_due_at, agent_sla_breached_at, agent_sla_notified_at FROM conversations;"`.

Pendências
- Seed automático de `t_demo.replies`/`t_demo.gateways` (hoje preciso inserir manualmente antes do `curl` e o worker ainda loga avisos pela ausência de gateways).
- Cobrir o watchdog com um teste e2e (fake timers) para não depender de Docker + Slack mock.

## 2025-11-09 — Codex (Docs organization + E2E manual send)
Resumo
- Organização de docs finalizada (índice único, templates, padrões de nomes e tooling) e rename `official-api/` validado por link‑check.
- Ledgers alinhados (backend) e novos ledgers criados (whatsapp, ops) com checklists apontando para evidência.
- E2E adicionado para `/v2/messages/manual` com `agentId`/`queue`/`tags`; marcamos nos ledgers como **Verified**.
- Novos E2E (WhatsApp): `test/e2e/wa.webhook.idempotency.e2e.spec.ts` (assinatura HMAC + dedupe/idempotência) e `test/e2e/wa.metrics.e2e.spec.ts` (exposição Prometheus de filas + métricas WA). Ledger `whatsapp-ledger.md` marcado como “In progress” nesses tópicos.
- Teste adicional de filas (stubbed): `test/wa.queues.workers.spec.ts` valida publicação em `wa-inbound:<tenant>`/`wa-outbound:<tenant>` com TTL/trace; consumo real via Worker ficará para próxima etapa (BullMQ restringe `:` no nome em runtime deste sandbox).
- ADR aceita: `docs/adr/ADR_20251109_bullmq_queue_separator.md` — adotado `WA_QUEUE_SEP=__` e atualização no backend/worker para compor `<prefix><SEP><tenantSlug>`.
- Integração Worker+Redis adicionada (gated): `test/integration/wa.queues.workers.integration.spec.ts` — executa com `IT_DOCKER=true` quando o Docker puder puxar imagens.
- Implementação: follow‑up pós‑handoff (retorno ao bot). Novo serviço `WhatsappHandoffFollowUpService` agenda texto ou template com delay opcional; `conversations/:id/handoff/complete` passa a disparar follow‑up quando `returnToBot=true`. Variáveis `.env` adicionadas.
- Gatilhos/score configuráveis: `WA_HANDOFF_HUMAN_KEYWORDS`/`WA_HANDOFF_KEYWORDS_MAP` para keywords por tenant; pesos de prioridade em `WA_HANDOFF_SCORE_*`. Router usa esses pesos para calcular `priority` e registrar métricas existentes.
- Novo teste `test/e2e/wa.rate-limit.per-number.e2e.spec.ts` garante o throttling por destinatário (`wa_recipient_delay_ms`, `wa_outbound_recipient_wait_*`). Fase A (ingestão/rate) considerada concluída.
- Novo fluxo `test/e2e/wa.webhook.bot-flow.e2e.spec.ts`: exercita webhook Meta → fila inbound → bot (`WhatsappBotService`) → resposta com enqueue outbound. Item correspondente da Fase B (persistência/transacional) marcado como done.
- `test/whatsapp.bot.service.spec.ts` ganhou cenários para `WA_HANDOFF_HUMAN_KEYWORDS`/`WA_HANDOFF_KEYWORDS_MAP` e `conversations.controller.spec.ts` cobre follow-up + métricas → itens de gatilho/pausa (Fase C) atualizados no checklist/ledger.
- Ajustei `conversation-state.service.spec.ts` + `conversations.repository.ts` para limpar `fail_count`/`unread_count` ao retornar ao bot; `wa.webhook.bot-flow.e2e.spec.ts` agora cobre o `POST /conversations/:id/handoff/complete` (follow-up + métricas). Fase E (fluxo de encerramento) marcada como done no checklist.
- `WhatsappBotService` agora considera `handoff_keywords` definidos no metadata da conversa/payload (além dos envs). Env novo: `WA_HANDOFF_RESET_COUNTERS` controla se os contadores são zerados ao retomar o bot.

Evidências
- Docs: `greenfield/docs/index.md`, `.sangoi/.tools/docs/README.md`, `greenfield/docs/standards/docs-naming.md`.
- Ledgers: `greenfield/docs/parity/backend-ledger.md`, `greenfield/docs/parity/whatsapp-ledger.md`, `greenfield/docs/ops/ops-ledger.md`.
- Teste: `greenfield/apps/backend/test/e2e/messages.manual.e2e.spec.ts`.

Comandos úteis
- `bash .sangoi/.tools/docs/link-check.sh greenfield/docs`
- `bash .sangoi/.tools/docs/find-nonconformant.sh`
- `cd greenfield/apps/backend && npm ci --cache=../../.npm-cache && npm test -- --runTestsByPath test/e2e/messages.manual.e2e.spec.ts`
- `cd greenfield/apps/backend && npm test -- --runTestsByPath test/e2e/wa.webhook.idempotency.e2e.spec.ts`
- `cd greenfield/apps/backend && npm test -- --runTestsByPath test/e2e/wa.metrics.e2e.spec.ts`
- `cd greenfield/apps/backend && npm test -- --runTestsByPath test/wa.queues.workers.spec.ts`
- `cd greenfield/apps/backend && IT_DOCKER=true npm test -- --runTestsByPath test/integration/wa.queues.workers.integration.spec.ts`
 - Configurar follow‑up (opcional): editar `greenfield/.env` — `WA_HANDOFF_FOLLOWUP_TEXT` ou `WA_HANDOFF_FOLLOWUP_TEMPLATE_NAME`.
 - Revisar ADR: `greenfield/docs/adr/ADR_20251109_bullmq_queue_separator.md`

Próximos passos (WhatsApp‑first)
- E2E webhook Meta (idempotência/assinatura/dedupe).
- E2E filas WA (enqueue/dequeue + métricas) e rate‑limit por destinatário.
- Specs formais de gatilhos de handoff (keywords/loop/score) e retomada “complete→follow‑up→bot_enabled=true”.

Arquivo da sessão
- Ver detalhes: `greenfield/docs/handoff/HANDOFF_2025-11-09-docs-org.md`.

## 2025-11-09 — Codex (Bot strategy selection + conversations API)
Resumo
- Adicionamos seleção dinâmica de strategies (`MenuBot`, `LLMBot` stub) via metadata/env (`WA_DEFAULT_BOT_STRATEGY`, `WA_BOT_STRATEGY_MAP`) e criamos o stub `LLMBotStrategy`, permitindo personalizar o engine por tenant.
- Refatoramos o worker Bull para expor o forwarder (`whatsapp-inbound.forwarder.ts`) e adicionamos testes unitários + cobertura do endpoint interno para fechar o circuito webhook→fila→bot.
- Expostos `/v2/conversations`, `/conversations/:id`, `/handoff/accept`, `/handoff/complete`, usando `ConversationStateService` para atualizar cache e registrar agentes/estados de handoff.
- Implementamos o roteador de handoff + gatilhos da Fase C: detecção de pedidos explícitos (“quero humano”), métricas `wa_handoff_requested_total` e `wa_bot_confidence`, e integração do LLMBot com o mesmo endpoint `/completion` do legado.
- `/v2/conversations/:id` passou a aceitar `include=events`, retornando o histórico derivado de `conversation_events` sem consultas extras para o dashboard.
- `/v2/messages/manual` libera respostas humanas (sessão) registrando `agentId`/fila/tags e enfileirando via BullMQ.
- Frontend (Attendance queue + ChatThread) agora consome `/v2/conversations`/`include=events`, assume/devolve/encerra handoffs e envia mensagens humanas via `/v2/messages/manual`.

Observações
- O `LLMBot` agora consulta o endpoint `/completion` compartilhado com o legado e decide entre resposta automática ou handoff usando o score retornado; precisamos calibrar prompts/thresholds por tenant + expor métricas.
- Falta um teste e2e cobrindo o fluxo completo Meta webhook → job → bot (o atual ainda foca em rate limit); anotado nos checklists.
- Conversas já expõem `conversation_events` e estão sendo consumidas pelo inbox Vue, mas precisamos de Playwright cobrindo o fluxo de atendimento.
- Enquanto Playwright não roda no sandbox, criamos specs Vitest para `ChatThreadView`/`AttendanceQueueView` garantindo que assume/devolve/encerra e envia mensagens corretamente.

Testes
- `cd greenfield/apps/backend && npm test -- whatsapp.service.spec.ts metaclient.contract.spec.ts integration/whatsapp.redis.integration.spec.ts whatsapp.menu-bot.strategy.spec.ts whatsapp.bot.service.spec.ts whatsapp.bot.resolver.spec.ts whatsapp.internal.controller.spec.ts conversations.controller.spec.ts conversation-state.service.spec.ts`
- `cd greenfield/apps/bull && npm test -- whatsapp-inbound.forwarder.spec.ts`
- `cd greenfield/apps/bull && npm run build`

Follow-up recomendado
1. [Postponed] Calibrar o `LLMBot` (prompts, observabilidade) quando houver dados reais para ajustar threshold e métricas.
2. [Postponed] Construir teste/e2e webhook→fila→bot→resposta (até lá validaremos em fluxo real).
3. Popular `/v2/conversations/:id` com o histórico completo (`conversation_events`) e instrumentar `/handoff/complete` com motivos/tags para dashboards.

## 2025-11-08 — Codex (WA bot integration + events)
Resumo
- Bull worker agora encaminha jobs inbound (`wa-inbound:<tenant>`) para o endpoint interno `POST /v2/internal/whatsapp/inbound`, mantendo o processamento de mídia/correlação existente e liberando o bot para operar dentro do Nest.
- Criamos `WhatsappBotService` com resolver/strategies (`MenuBot`) e publicador de eventos (`WhatsappConversationEvents`), consumindo `ConversationStateService` para atualizar `fail_count`, `handoff_state` e cache Redis.
- `WhatsappService` passou a emitir `message_sent` para cada envio (text/media/template/interactive) e o novo módulo de conversas ganhou helpers para incrementar falhas e solicitar handoff sem tocar em `last_user_at`.
- Adicionamos specs focadas (`whatsapp.bot.service.spec.ts`, `whatsapp.menu-bot.strategy.spec.ts`) e atualizamos os testes existentes (service, Meta contract, integração Redis) + build do worker Bull.

Observações
- Resolver ainda sempre devolve o `MenuBot`; falta carregar strategy por tenant e implementar o `LLMBot` prometido no plano.
- O evento `wa:events` já publica `message_received`/`message_sent`, mas não há consumidor ainda — dashboards/routers precisam assinar antes de liberar em produção.
- Checklist Fase B marca modelagem/engine básicos, porém scripts de backfill e APIs `/v2/conversations` continuam pendentes para o handoff completo.

Testes
- `cd greenfield/apps/backend && npm test -- whatsapp.service.spec.ts metaclient.contract.spec.ts integration/whatsapp.redis.integration.spec.ts whatsapp.menu-bot.strategy.spec.ts whatsapp.bot.service.spec.ts`
- `cd greenfield/apps/bull && npm run build`

Follow-up recomendado
1. Implementar seleção de strategy por tenant (`gateways.repository`/config) e iniciar o esqueleto do `LLMBot` (mesmo que devolva `noop`) para destravar o checklist.
2. Expandir o fluxo webhook→fila→bot→resposta em `test/whatsapp.redis.integration.spec.ts` ou e2e dedicado, garantindo que o bot é invocado (hoje o teste foca só no rate limit).
3. Começar a expor `/v2/conversations` + rotas de handoff (Fase C/D) consumindo o cache + eventos recém-publicados, evitando divergência com o dashboard Vue.

## 2025-11-08 — Codex (WA conversation state groundwork)
Resumo
- Escrevemos a migração `20251108_001_conversation_state.sql` criando `conversations`/`conversation_participants`, colunas extras em `messages`/`replies` e novas SRFs/views multi-tenant; `ensure_tenant_schema` passa a provisionar tudo automaticamente.
- Implementamos `ConversationsRepository` + `ConversationStateService` (Redis cache) e conectamos ao fluxo Meta/Gupshup/Smarters + `WhatsappService` (`prepare*`, `send*`, controllers, queue jobs, manual send). Mensagens/replies agora recebem `conversation_id`, `channel`, `direction`, `source`.
- Atualizamos controllers (public/internal/templates/messages simples) para carregar o estado, propagar `conversationId` nas filas e normalizar `to`; ajustes em `RepliesRepository`, `WhatsappQueueJob` e `templates`/docs (`docs/plan/whatsapp-atendimento-implementation.md`, novo `docs/plan/whatsapp-conversation-schema.md`).
- Casos de teste tocados: service, meta webhook specs, gupshup/smarters providers e integração Redis adaptados ao novo serviço de conversa.

Observações
- Ainda falta consumir o estado na camada de bot (BotStrategy), eventos dominó (`message_received` etc.) e APIs de conversa/handoff (Fase C/D do plano).
- Precisamos migrar/seedar conversas históricas para tenants existentes assim que o catálogo de mensagens estiver disponível (script TODO em `tools`).

Testes
- `cd greenfield/apps/backend && npm test -- whatsapp.service.spec.ts providers/gupshup.providers.controller.spec.ts providers/smarters.providers.controller.spec.ts providers/meta.notify.mapping.spec.ts providers/meta.signature.spec.ts providers/meta.verify.gateway.spec.ts`

Follow-up recomendado
1. Implementar o repositório de conversas/handoff no bot engine (BotStrategy + eventos) usando o cache recém-criado.
2. Disponibilizar endpoints `/v2/conversations` + rotas de handoff (Phase C/D) antes de liberar dashboards/agents.
3. Rodar script de bootstrap/backfill assim que o ETL de histórico estiver pronto e registrar no ledger.

## 2025-11-10 — Codex (WhatsApp adjustments)
Resumo
- Consolidamos o hash determinístico do webhook Meta usando o raw body e removemos qualquer fallback silencioso; testes cobrindo raw ausente agora falham explicitamente.
- Reestruturamos as filas WA inbound/outbound para nascerem apenas para tenants com gateways ativos e apenas quando a fila tem jobs pendentes, reduzindo workers ociosos; DLQ agora ignora payloads sem metadata e registra métricas de skip.
- Introduzimos gauges separados `wa_phone_rate_delay_ms` e `wa_recipient_delay_ms`, mais métricas por tenant, com testes e documentação; novos envs (`WA_RECIPIENT_NORMALIZE_MODE`, `WA_RECIPIENT_MIN_DIGITS`) permitem short codes e números não-E.164.
- Normalização dos webhooks (Meta/Gupshup/Smarters) agora respeita o opt-out acima; runbook/CONFIGURATION/.env.example atualizados, checklist/todo revisados.

Observações
- Falta atualizar dashboards Prometheus/Grafana com os novos gauges e expor alertas para `wa_dlq_skipped_total`.
- Registrar métricas reais de workers WA x gateways antes do próximo deploy para confirmar o ganho de footprint.

Testes
- `cd greenfield/apps/backend && npm test -- --runTestsByPath test/modules/whatsapp/meta-webhook-idempotency.middleware.spec.ts test/modules/whatsapp/whatsapp.queue.service.spec.ts test/whatsapp.service.spec.ts`
- `cd greenfield/apps/bull && npm run build`

Follow-up recomendado
1. Configurar os dashboards/alertas Prometheus para `wa_phone_rate_delay_ms`, `wa_recipient_delay_ms` e `wa_dlq_skipped_total`.
2. Adotar métricas de workers vs gateways no monitoramento (com o guardião do runbook) para detectar tenants zombie.
3. Prosseguir com o ajuste opcional remanescente: métricas explícitas para DLQ drop reasons adicionais (se surgirem) e scripts de limpeza automática.
## 2025-11-10 — Codex (WhatsApp templates & quality)
Resumo
- Entregamos o catálogo `wa_templates` por tenant (tabela nova + repositório `wa-templates.repository.ts`), expusemos `/v2/whatsapp/templates/catalog` e adicionamos comandos no `manual_api.py` para listar/registrar entradas.
- `WhatsappService.prepareTemplateJob` agora consulta o catálogo e bloqueia envios com contagem de placeholders divergente, eliminando o modo “melhor esforço”.
- Implementamos o monitoramento de qualidade/tier: `POST /v2/whatsapp/admin/phone-numbers/refresh` consulta o Graph, atualiza métricas `wa_phone_quality_rating`/`wa_phone_quality_alert_total` e loga `wa_phone_quality_degraded`.
- Runbooks/ops: criamos `docs/ops/observability.md`, `docs/ops/postgres-backup.md` e `docs/ops/log-retention.md`, atualizando o ops-ledger (Prometheus, backups, rotação).

Evidências
- Handoff: `docs/handoff/HANDOFF_2025-11-10-wa-template-quality.md`.
- Testes: `npm test -- --runTestsByPath test/whatsapp.service.spec.ts test/whatsapp.templates.catalog.controller.spec.ts test/whatsapp.quality.monitor.service.spec.ts`, `npm run db:reset`.
- Ledgers: `docs/parity/whatsapp-ledger.md` (linhas “Catálogo WA templates…” e “Monitoramento de qualidade/tier”), `docs/ops/ops-ledger.md`.

Pendências
- Biblioteca “sessão vs template” (sem fallback automático) e visibilidade do tier no dashboard NetSuite.
- Go-live checklist (e2e sandbox Meta, plano de contingência, alertas validados, treinamento agentes) permanece aberto.

## 2025-11-10 — Codex (WhatsApp go-live prep)
Resumo
- Formalizamos o plano de contingência (`docs/ops/whatsapp-alerting-followup.md`), incluindo WA_FORCE_HANDOFF e pausa/retomada de filas.
- Automatizamos backups (`.sangoi/.tools/ops/pg_dump_daily.sh`) e consolidamos políticas de logs/segredos (`docs/ops/postgres-backup.md`, `docs/ops/log-retention.md`, `docs/ops/ops-ledger.md`).
- Criamos o guia rápido para agentes (`docs/ops/whatsapp-agent-training.md`) e atualizamos o runbook com os comandos de catálogo/componentes.

Evidências
- Checklist: `docs/todo/whatsapp-handoff-checklist.md` (Fase F e Go-live atualizados).
- Ops docs citados acima + handoff atual (`docs/handoff/HANDOFF_2025-11-10-whatsapp-go-live.md`).

Pendências
- Executar testes e2e no sandbox Meta assim que a janela estiver disponível.
- Integrar status da biblioteca sessão vs template ao inbox NetSuite (`~/.netsuite/TODO-FRONTEND.md`).
## 2025-11-10 — Codex (Fase B – Flows & metadata)
Resumo
- Entregamos `wa_flows`/`wa_flow_templates` (migração `20251110_010`) e criamos `WhatsappFlowsService` + endpoints internos/CLI (`manual_api.py whatsapp flows publish/list/link-template`) para registrar e vincular Flows aprovados.
- `WhatsappService.prepareInteractiveJob` agora suporta `interactiveType=flow`, o MenuBot pode responder com `flowId`, e o worker publica `wa-send-interactive` com payload nativo.
- O webhook Meta persiste `nfm_reply`/`flow_token` em `payload_meta`, `conversation_events` e `conversation.metadata` (gated por `WA_FLOW_METADATA_ENABLED`/`WA_FLOW_METADATA_TENANTS`).
- Checklist Fase B atualizado para dar baixa em bot registry, ingestão de Flows e persistência de eventos; ledger ganhou as linhas “Flow registry & CLI” e “Flow metadata em conversation events”.

Evidências
- Handoff: `docs/handoff/HANDOFF_2025-11-10-phase-b-flows.md`.
- CLI: `manual_api.py whatsapp flows publish --flow-id ...`, `... flows list`, `... flows link-template`.
- Testes: `cd greenfield/apps/backend && npm test -- --runTestsByPath test/whatsapp.bot.service.spec.ts -i` (✅) e `npm test -- --runTestsByPath test/whatsapp.service.spec.ts -i` (falhou no caso “session window expired”, registrado no handoff).

Pendências
- Ajustar o mock do teste `whatsapp.service.spec.ts` para que o cenário “session window expired” valide o `BadRequestException` sem tentar acessar `client.sendText`.
- Cobrir envio Flow (bot/menu → queue → worker) com e2e dedicado e adicionar ao ledger.

## 2025-11-17 — Codex (CSS token unification, phase 2)
Resumo
- Continuei a refatoração de CSS focando em tokens: unifiquei `--background-color`/`--panel-bg-color` com `--ab-bg-default`/`--ab-bg-panel-alt`, consolidei sombras (`--ab-shadow-*`) em quatro níveis e alinhei a cor de marca (`--primary-color`) como alias explícito de `--ab-accent` no suitelet.
- Padronizei erros/status e algumas pills/metadados para usar tokens de texto/erro existentes (`--color-danger`, `--ab-status-err-color`, `--ab-time-*`, `--ab-text-*`) em vez de cores literais, e tokenizei pontos óbvios em `customer-search.css` (ícone e fundo).
- Fiz um sweep em `src/assets/app.css` removendo aliases shadcn não usados (`--accent*`, `--color-card*`, `--color-popover*`, `--color-secondary*`, `--color-destructive*`, `--color-input`, `--color-chart-*`, `--radius-xl`, `--input-height`, `--sidebar*`), mantendo só base + derivados que o bundle consome de fato.
- Após cada rodada rodei `css-usage-report`, `css-conflict-report` e `css-var-usage-report`; hoje `analysis/css-var-usage.json` aponta `variableCount=370`, `definedOnlyCount=28`, sempre com `Unused=0` e `Conflicts/Duplicates=0`.

Evidências
- Task-log: `.sangoi/task-logs/2025-11-16_css-bloat-trim-and-vars.md` (seções “Token unification”, “Sombras”, “Marca / warm”, “Texto/bordas”, “paleta base”).
- Handoffs: `.sangoi/handoffs/HANDOFF_2025-11-16-css-bloat-trim-and-vars.md` (fase anterior) e `.sangoi/handoffs/HANDOFF_2025-11-17-css-token-unification.md` (esta sessão).
- Análise: `.sangoi/analysis/2025-11-16_css-token-unification.md` (seção 4.8 revisada) e `analysis/css-var-usage.json` (estado mais recente).

Pendências
- Revisar os 28 tokens `definedOnly` restantes (glass/overlay/`chart-*`/`secondary*`/`popover*`/`destructive*`/`input`) e decidir, caso a caso, se permanecem como backlog documentado ou se podem ser removidos/aliasados.
- Rodar novo sweep guiado por `analysis/css-token-lint.json` principalmente em `message.css`, `regua-*.css` e `omniview*.css` para trocar literais residuais por tokens já existentes antes do próximo deploy.

## 2025-11-25 — Codex (Dashboard queue preview hover + messages)
Resumo
- Hover de preview da fila (`ab-queue-preview`) ainda apresenta dois problemas: posição inconsistente quando a tabela não está centralizada no viewport e ausência de mensagens no modal, mesmo quando a view `/attend` exibe histórico para a mesma conversa.
- Este handoff cria um follow-up explícito para revisar o contrato front/back do preview de fila e simplificar o cálculo de posição do modal, evitando novos remendos pontuais.

Próximos passos
- Criar/atualizar handoff detalhado em `.sangoi/handoffs/HANDOFF_2025-11-25-dashboard-queue-preview.md` documentando:
  - Como o hover é disparado (`QueuePanel.vue`), que parâmetros o front envia para `queue_preview` (`disabledUserId`, `phoneNumber`, `chatType`) e como o Suitelet traduz isso para `DisabledUserFilter` (`/disabled/user/history` + fallback em `/disabled/user/messages`).
  - Diferenças entre o filtro usado em `queue_preview` e o filtro de `conversation_detail` (que já exibe eventos corretos na view `/attend`), incluindo janela de datas, uso de `disabledUser` vs `phoneNumber` e tratamento de `chatType`.
  - Novo algoritmo de posicionamento do `ab-queue-preview`: baseado em linha/viewport, com decisão simples entre “acima” e “abaixo” do pointer e clamps de gutter, sem depender de containerRects frágeis.
- Validar manualmente no sandbox:
  - Que o hover mostra as mesmas 3 últimas mensagens que aparecem na lateral de `/attend` para o mesmo atendimento.
  - Que o modal permanece visível e bem posicionado em diferentes resoluções e contextos (dashboard standalone, portlet NetSuite, overlay), mesmo quando a tabela está deslocada no host.

## 2025-11-25 — Codex (Dashboard queue preview hover/UI)
Resumo
- Refatoramos o hover `ab-queue-preview` para ancorar diretamente no ponteiro (`clientX/clientY`), seguindo o mouse em tempo real dentro da linha e decidindo esquerda/direita e cima/baixo com regras geométricas simples + espaço disponível em viewport.
- O conteúdo do modal é controlado por hover nas células (cliente/número/atividade/status), com debounce/caching por conversa via `getRowPreview`; o estado alterna entre “Carregando…”, erro e lista de mensagens.
- Detalhes completos de algoritmo/abordagens anteriores foram registrados em `.sangoi/handoffs/HANDOFF_2025-11-25-dashboard-queue-preview-hover-ui.md`.
- Atualizar `.sangoi/reference/frontend/frontend-dump-for-llm.md` com o contrato final do `queue_preview` e o novo comportamento de posicionamento do hover.

Responsável sugerido
- Codex / Dashboard Feature Team.

Notas
- Ao assumir este follow-up, alinhar primeiro o filtro do preview com o de `conversation_detail` (backend como fonte da verdade) e só depois mexer em UX. Evitar novos toggles/debug flags; se precisar de inspeção longa, reutilizar o `KEEP_QUEUE_PREVIEW_OPEN` apenas em ambientes de desenvolvimento.

2025-11-26 — CSS overlap & radius harmonization (Codex)
- Handoff: .sangoi/handoffs/HANDOFF_2025-11-26-css-overlap-and-radius.md
- Task-logs: .sangoi/task-logs/2025-11-26_css-overlap-pass-1.md, .sangoi/task-logs/2025-11-26_css-radius-unification.md
- Focus: reduzir overlaps (root/container/panéis/pills/botões) e unificar border-radius via tokens --ab-radius-* em :where(:root,:host).

2025-11-26 — Templates (legado) & Régua UI (Codex)
- Handoff: .sangoi/handoffs/HANDOFF_2025-11-26-templates-and-regua-ui.md
- Task-logs: .sangoi/task-logs/2025-11-26_regua-scheduling-ui-grid-and-toolbar.md
- Focus: padronizar painéis de Templates/Régua com `AbSurfaceView` + `ab-panel` + `ab-table`, usando toast para erros (`Toaster`) e resolvendo sticky headers e grids sem inline styles.

2025-11-26 — CSS overlap, radius & dashboard modifiers (Codex)
- Handoff: .sangoi/handoffs/HANDOFF_2025-11-26-css-overlap-and-radius.md
- Task-logs: .sangoi/task-logs/2025-11-25_css-pill-overlap.md, .sangoi/task-logs/2025-11-26_css-overlap-pass-1.md, .sangoi/task-logs/2025-11-26_css-radius-unification.md, .sangoi/task-logs/2025-11-26_css-overlap-pass-2-buttons-quick-cards-status.md, .sangoi/task-logs/2025-11-26_css-overlap-pass-3-queue-and-viz.md
- Focus: reduzir overlaps no dashboard (root/container/painéis/pills/botões/status/time/queue/cards/charts), unificando geometria via tokens --ab-radius-* e deslocando modifiers/estados para tokens de cor/sombra/spacing.

2025-11-26 — Régua Dinâmica scheduling UI (Codex)
- Handoff: .sangoi/handoffs/HANDOFF_2025-11-26-regua-scheduling-ui.md
- Task-logs: .sangoi/task-logs/2025-11-26_css-regua-radius-and-shadow.md, .sangoi/task-logs/2025-11-26_regua-scheduling-ui-grid-and-toolbar.md
- Focus: alinhar a view de régua dinâmica (`SuiteletRegua.vue` + `ReguaEditor.vue`) ao mock EXEMPLO-REGUA, ajustando colunas da grade de etapas, aproximando timeline e editor e tornando o painel “Roteiros salvos” compacto e legível.
## 2025-11-27 — Codex (Aba WhatsApp via reports/messages/sentmessages)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-11-27-whatsapp-tab-reports.md` documentando a mudança da aba "WhatsApp" do `ab_buttons_ue` para listar templates enviados usando o endpoint de relatório `/reports/messages/sentmessages` (via `AB/legacyApi`), em vez do custom record `customrecord_{{AB_TAG}}_sent_msgs`.
- O UE agora resolve o telefone do registro (mobile/phone), gera um fragmento numérico com os últimos 9 dígitos e consulta o report por `phoneNumber`, populando uma sublista “Templates enviados” com data/hora (formatada via `N/format`), ID da mensagem, número de origem, template e descrição. O campo numérico `status` é guardado como `statusCode` mas ainda não é exibido.
- `legacy_api.js` foi atualizado para tratar `/reports/messages/sentmessages` como endpoint AES de sessão em `AES_TOKEN_PREFIXES`, alinhando o uso do Token com os demais endpoints do SecurityInterceptor.

## 2025-11-28 — Codex (Dashboard queue, gauges e layout dos painéis)
Resumo
- Novo handoff em `.sangoi/handoffs/HANDOFF_2025-11-28-dashboard-queue-and-gauges.md` detalhando a reestruturação da fila dentro de um `ab-panel` com card interno (`ab-card ab-card--viz`), o reposicionamento do burger-menu para o rodapé (ao lado da paginação) e a troca dos botões de paginação "Anterior"/"Próxima" por setas com textos `sr-only`.
- `DashboardSurface.vue` agora monta a fila como um painel `ab-panel--queue` com header (`ab-panel__title` + `ab-subtitle`) alinhado aos painéis "Consolidados gerais" / "Atendimentos ao ano", e o corpo contém `QueuePanel` dentro de um card, mantendo a fila visualmente consistente com os demais cards de métricas.
- `QueuePanel.vue` passou a cuidar apenas do corpo (badges, tabela, preview e footer), consolidando a lógica de menu/sort/paginação em um único lugar; o dedupe por `id` em `queueRows` foi recuado para não esconder múltiplos atendimentos de uma mesma pessoa, e o follow-up recomenda investigar `dashboard_stats.rows` para definir uma chave de unicidade mais segura.
# Handoff — Lean Template (first person)

Date (UTC‑3): 2025-12-19T10:45  •  Author: Codex  •  Anchor: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`

## 0) TL;DR (3–5 lines)
- Objective I was pursuing: substituir a abordagem de “rewrites AB→SB” por placeholders `{{AB_TAG}}` no repo (source-of-truth) e gerar builds renderizadas para deploy (ab/sb).
- Delivered: migração automática `_ab_` → `_{{AB_TAG}}_` (inclui nomes de arquivos) + OmniView `custrecord_abov_` → `custrecord_{{AB_TAG}}ov_`; `sdf-variant-generate` agora renderiza placeholders; `buildAndDeploy` agora sempre gera build SDF antes do deploy.
- In progress: rodar `npm run deploy` / `npm run deploy:dev` em Windows/WSL e capturar o primeiro erro do SuiteCloud (se houver) para normalizações pontuais no gerador.
- Next immediate step (single command): `npm run deploy:dev` (Windows/WSL).

## 1) What I did (precise and auditable)
- Migrei o repo inteiro para placeholders `{{AB_TAG}}` como source-of-truth, incluindo renome de arquivos `*_{{AB_TAG}}_*`. Evidence: `.sangoi/task-logs/2025-12-19_ab-tag-placeholders.md`.
- Atualizei o gerador `sdf-variant-generate` para renderizar placeholders (conteúdo + paths) e gerar builds deployáveis em `build/sdf-variants/<tag>`. Evidence: `.sangoi/task-logs/2025-12-19_ab-tag-placeholders.md`.
- Refatorei o `buildAndDeploy.js` para sempre gerar uma build SDF renderizada (default `ab`) e fazer deploy a partir do diretório gerado. Evidence: `.sangoi/task-logs/2025-12-19_ab-tag-placeholders.md`.
- Ajustei pontos do `ab_vue_ui` que não podem conter `{{AB_TAG}}` como identificador TS/JS (ex.: `window.__...`) para usar acesso por string (`window['__{{AB_TAG}}_...']`). Evidence: `.sangoi/task-logs/2025-12-19_ab-tag-placeholders.md`.

## 2) File‑by‑file changes (why before how)
- `.sangoi/.tools/ab-tag-placeholders-migrate.mjs`: migra/renomeia o repo para placeholders (conteúdo + filenames) para evitar drift manual. Expected impact: IDs e paths ficam “templated” e reaproveitáveis para `ab/sb`.
- `.sangoi/.tools/sdf-variant-generate.mjs`: deixou de depender de `_ab_` como token e passa a renderizar `{{AB_TAG}}`/`{{AB_TAG_UPPER}}` e paths/nomes de arquivos, gerando `build/sdf-variants/<tag>`. Expected impact: builds determinísticas e fáceis de inspecionar.
- `buildAndDeploy.js`: deploy passa a acontecer sempre em cima da build renderizada; usa `src/manifest.xml` para detectar `appId` base. Expected impact: `src/` não precisa mais ser editado para instalar variantes.
- `package.json`: `deploy`/`build:deploy` usam `--tag ab` (oficial) e `deploy:dev` usa `--tag sb` (dev).
- `fastDeploy.mjs`: wrapper para `buildAndDeploy.js --fast-deploy/--fast-bundle` (mantém compat de comandos).
- `ab_vue_ui/src/*`: acesso a campos `custrecord_{{AB_TAG}}_*` e globals `__{{AB_TAG}}_*` ajustado para bracket notation (evita TypeScript parse errors no source templated).

## 3) In progress (clear stop point)
- Task: validar deploy ab/sb com build renderizada  •  Area: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`
- What’s done: repo templated + build generator renderiza `{{AB_TAG}}` e gera `build/sdf-variants/ab` e `build/sdf-variants/sb` sem placeholders remanescentes.
- Still missing: executar deploy real no Windows/WSL e mapear o próximo erro (se houver).
- Acceptance criteria on resume: `npm run deploy` e `npm run deploy:dev` completam sem erros de validação; `build/sdf-variants/<tag>/src` não contém `{{AB_TAG}}`.

## 4) Follow‑up (execution order)
1. Rodar `npm run deploy:dev` (depende de Windows/WSL com `pwsh` + SuiteCloud CLI). Expected output: deploy do `build/sdf-variants/sb` sem erro.
2. Rodar `npm run deploy` (Windows/WSL). Expected output: deploy do `build/sdf-variants/ab` sem erro.
3. Se falhar: capturar o primeiro “Details:” + “File:” e ajustar normalização no gerador. Unblock by: adicionar regra pontual e regenerar.

## 5) Relevant notes
- `src/` agora é **template** (não deployável direto). O deploy deve ser sempre a partir do `build/sdf-variants/<tag>`.
- Regra para Vue/TS: placeholders não podem entrar como identificador (ex.: `window.__{{AB_TAG}}_x`); use `window['__{{AB_TAG}}_x']`.
- Sensitive: credenciais do SuiteCloud são locais; não registrar tokens em logs/docs.

## 6) Commands I ran (with short outcome)
`node .sangoi/.tools/ab-tag-placeholders-migrate.mjs --dry-run`
Result: ✅ (mostrou arquivos a editar/renomear)

`node .sangoi/.tools/ab-tag-placeholders-migrate.mjs --apply`
Result: ✅ (aplicou replace + rename)

`node .sangoi/.tools/sdf-variant-generate.mjs --tag sb --preset full`
Result: ✅ (gera `build/sdf-variants/sb`)

`node .sangoi/.tools/sdf-variant-generate.mjs --tag ab --preset full`
Result: ✅ (gera `build/sdf-variants/ab`)

## 7) Useful links (no duplication)
- Task‑log: `.sangoi/task-logs/2025-12-19_ab-tag-placeholders.md`
- Changelog: `.sangoi/CHANGELOG.md` item 2025-12-19
- Planning: `.sangoi/planning/2025-12-19_suiteapp-dev-variant.md`
