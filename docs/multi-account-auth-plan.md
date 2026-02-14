# Multi-account (ChatGPT) Auth + Auto Switch on Usage Limit (Plan)

Status: in progress (Phase 1 complete; Phase 2 complete; Phase 3 MVP complete; Phase 4 pending)
Owner: Lucas
Scope: `codex-rs` (core + cli + tui); global in `CODEX_HOME`

## Current implementation status

- ✅ Multi-account auth store (`AuthStore` v1) with legacy migration is implemented in `codex-rs/core`.
- ✅ Per-account usage snapshots are persisted (best-effort) from `RateLimitSnapshot` updates.
- ✅ Per-account exhaustion tracking (`usage_limit_reached` → `exhausted_until`) is implemented.
- ✅ Auto switch on `usage_limit_reached` (opt-in) is implemented (switch + retry in the same turn).
- ✅ “Lowest weekly remaining” selection is implemented (weekly used%, then credits balance, then primary used%).
- ✅ Core integration test covers 429 → switch → success.
- ✅ Manual account management UX is implemented in `codex-rs/tui` (`/accounts` and multi-account `/logout` selector).

## Lições operacionais da reaplicação (2026-02-11)

- O port de `/accounts` na TUI é **cross-file**; não funciona com patch isolado só em `chatwidget.rs`.
  - Contratos mínimos: `tui/src/app_event.rs`, `tui/src/app.rs`, `tui/src/chatwidget.rs`, `tui/src/bottom_pane/mod.rs`, `tui/src/status/{account,helpers,card}.rs`.
- Persistência de login deve ser por **upsert no store**:
  - em `login/src/server.rs`, `persist_tokens_async` precisa usar `update_auth_store` e preservar contas existentes.
  - evitar fluxo que reconstrói store inteiro a partir de payload legado.
- Fluxo `Logout all accounts` deve ser fail-safe:
  - só encerrar sessão quando `logout()` realmente retornar sucesso;
  - em erro, exibir mensagem e permanecer no app.
- `/accounts` não pode reintroduzir regressão de slash:
  - preservar disponibilidade durante task para comandos existentes (`DebugConfig`, `Clean`);
  - manter `/accounts` e `/logout` indisponíveis durante task.
- Validação prática que evitou falso-green nesta reaplicação:
  - `cargo test -p codex-tui --no-run` antes dos filtros;
  - filtros com contagem >0 para `accounts_popup`, `logout_popup`, `slash_command`, `completed_status_message`;
  - snapshots via `cargo insta pending-snapshots` (sem `-p` neste ambiente) e `cargo insta accept`.

## TL;DR

Adicionar suporte a **múltiplas contas ChatGPT armazenadas** (tokens + refresh) e um modo **opt-in** para, ao receber `usage_limit_reached`, **trocar automaticamente** para outra conta “disponível” e **tentar novamente no mesmo turno**, sem precisar reiniciar a sessão (sem `/resume`, sem perder histórico/reasoning).

O critério principal de seleção é **preferir a conta com menor saldo semanal restante**, para “queimar” primeiro o que está mais perto de acabar e evitar saldo fragmentado.

## Decisões (respostas do Lucas)

1. Credenciais: **somente ChatGPT tokens** (não API keys).
2. Troca ao atingir limite: **opção B (auto switch)**, mas **opt-in** e transparente.
3. Seleção: **preferir menor saldo weekly restante**.
4. Logout:
   - Se houver **1** conta cadastrada: `/logout` faz logout direto (equivalente a logout all).
   - Se houver **>1** conta: `/logout` abre um selector com:
     - **logout all** (apaga o store inteiro), ou
     - **remove selected account** (remove só uma conta; mantém as outras).
5. Escopo: **global** no `codex_home` (não por `profile`).

## Background (estado atual do código)

- Auth agora é multi-account (versionado):
  - `codex-rs/core/src/auth/storage.rs` define `AuthStore` (v1) e o storage (file/keyring/auto) em `$CODEX_HOME/auth.json` ou keyring.
  - `codex-rs/login/src/server.rs` persiste tokens no login (upsert; não sobrescreve outras contas).
  - `codex-rs/cli/src/login.rs` idem para CLI (login device-code/browser).
  - `codex-rs/tui*/src/onboarding/auth.rs` implementa onboarding de login assumindo 1 credencial.
- Usage limit já é erro estruturado:
  - 429 `usage_limit_reached` → `CodexErr::UsageLimitReached(UsageLimitReachedError{plan_type,resets_at,rate_limits})` em `codex-rs/core/src/api_bridge.rs`.
  - Mensagem “You've hit your usage limit…” é só `Display` em `codex-rs/core/src/error.rs`.
- O ponto ideal para interceptar e tentar novamente:
  - `codex-rs/core/src/codex.rs::run_sampling_request` atualiza rate limits, persiste `exhausted_until` e retorna o erro.
  - `ModelClientSession` é criado **por turno** via `turn_context.client.new_session()` em `codex-rs/core/src/codex.rs`.
  - Websocket reusa conexão enquanto aberta (`codex-rs/core/src/client.rs`); ao trocar auth, é preciso **forçar reconexão** (criar um novo `ModelClientSession`).
- Sinais de “saldo weekly” existem:
  - `RateLimitSnapshot.secondary` é rotulado como “weekly” quando `window_minutes` não existe (ver `codex-rs/tui/src/status/rate_limits.rs`).
  - `RateLimitSnapshot.credits.balance` costuma ser string numérica e é exibido como `<balance> credits` (mesmo arquivo).

## Objetivos

- Guardar múltiplas contas (ChatGPT tokens) de forma segura (file/keyring/auto).
- Trocar conta ativa sem reiniciar sessão e sem quebrar histórico.
- Auto switch ao `UsageLimitReached` (opt-in), com regra “menor saldo weekly” e guardrails anti-loop.
- Expor UX coerente:
  - onboarding: usar conta existente ou adicionar nova
  - comando: listar/trocar/remover contas
  - logout: escolher qual remover quando há várias
  - visibilidade: sempre mostrar qual conta está ativa

## Não-objetivos (por enquanto)

- Suporte multi-account para API keys.
- Expor multi-account via `app-server`/protocol v2 (fase posterior).
- “Picker” idêntico ao Google UI (a inspiração é só conceitual).

## Restrições e guardrails

- Respeitar restrições existentes de login (workspace/método) quando aplicável:
  - `forced_chatgpt_workspace_id` deve filtrar quais contas podem ser ativadas.
- Auto switch deve ser:
  - **opt-in explícito** via config (default `false`)
  - **observável**: emitir warning/evento “trocando de A → B”
  - **limitado**: no máximo `N` trocas por turno (ex.: `accounts.len()`), para não entrar em loop se todas estiverem exaustas.

## Design de storage (V1 versionado)

### Novo schema

Criar um formato versionado para armazenar várias contas em um único blob:

- **Arquivo**: `$CODEX_HOME/auth.json` (quando `cli_auth_credentials_store=file`)
- **Keyring**: **um único entry** contendo o JSON completo (quando `keyring/auto`), porque o keyring não oferece enumeração de entries e o `KeyringStore` atual é key-based.

Estrutura proposta (alto nível):

- `version: 1`
- `active_account_id: String | null`
- `accounts: Vec<StoredAccount>`
  - `id: String` (preferir `TokenData.account_id` quando presente; fallback UUID)
  - `label: Option<String>` (ex.: “pessoal”, “work”)
  - `tokens: TokenData`
  - `last_refresh: Option<DateTime<Utc>>`
  - `usage: Option<AccountUsageCache>`

`AccountUsageCache` (best effort):
- `last_rate_limits: Option<RateLimitSnapshot>`
- `exhausted_until: Option<DateTime<Utc>>` (quando bater `usage_limit_reached`)
- `last_seen_at: Option<DateTime<Utc>>`

### Migração do formato antigo

Ao carregar:
- se detectar `AuthDotJson` (legado), converter para `AuthStoreV1` com 1 conta:
  - `active_account_id = account.id`
  - `accounts = [account]`
- persistência:
  - opção A: escrever de volta imediatamente no novo formato (migração eager)
  - opção B (recomendada): migrar em memória e só persistir no próximo `save` (migração lazy) para evitar churn

Falhar alto:
- Se `version` desconhecida → erro claro instruindo o usuário (ex.: “faça backup e rode `codex logout` (ou apague `$CODEX_HOME/auth.json`) para voltar ao estado limpo”).

## Core auth: APIs necessárias

Em `codex-rs/core/src/auth.rs` e `.../auth/storage.rs`:

- `AuthManager::list_accounts() -> Vec<AccountSummary>`
- `AuthManager::active_account() -> Option<AccountSummary>`
- `AuthManager::set_active_account(id: &str) -> Result<()>` (persist + reload cache)
- `AuthManager::remove_account(id: &str) -> Result<()>` (se remover ativa: escolher outra ativa ou ficar “NotAuthenticated”)
- `AuthManager::add_account(tokens: TokenData, label: Option<String>, make_active: bool) -> Result<()>`
- `AuthManager::update_usage_for_active(snapshot: RateLimitSnapshot)`
- `AuthManager::mark_usage_limit_reached(resets_at: Option<i64>, snapshot: Option<RateLimitSnapshot>)`

Notas:
- `AuthManager` hoje cacheia um único `CodexAuth`; passará a cachear `AuthStoreV1` + “active auth”.
- Refresh token permanece “just-in-time” para a conta ativa.

## Seleção automática (heurística “menor saldo weekly”)

Intenção: evitar “retalhar” o limite de uso entre várias contas. Ou seja: ao precisar trocar, preferir a conta **mais perto de ficar indisponível**, para “queimar” uma conta por vez.

### Dados usados (por conta)

Preferência de sinal (na ordem):
1) `last_rate_limits.secondary.used_percent` (weekly). `used_percent` é **percentual consumido (0–100)**, então “menor saldo weekly restante” = **maior used_percent**.
2) `last_rate_limits.credits.balance` (string numérica): menor = preferido (se presente e parseável; `unlimited` deve ser o último recurso).
3) Fallback: `last_rate_limits.primary.used_percent` (maior = preferido).
4) Sem dados: ordem estável (ex.: ordem de cadastro ou round-robin) para evitar thrash.

### Disponibilidade

Uma conta é “selecionável” se:
- não está `exhausted_until > now`, e
- (se `forced_chatgpt_workspace_id` está setado) o `TokenData` pertence ao workspace permitido.

### Empates / edge cases

Empate: usar `last_seen_at` (preferir a menos recentemente usada) ou `id` lexical (determinístico).
Se todas indisponíveis: emitir erro com lista de contas e o menor `exhausted_until`.

## Retry no mesmo turno (core)

Ponto: `codex-rs/core/src/codex.rs::run_sampling_request`.

Comportamento proposto:

1) Executa request com conta ativa.
2) Se sucesso: segue fluxo normal.
3) Se `Err(CodexErr::UsageLimitReached(e))`:
   - atualizar state de rate limits do session (já existe hoje)
   - `auth_manager.mark_usage_limit_reached(e.resets_at, e.rate_limits)`
   - se `config.auth.auto_switch_on_usage_limit == true`:
     - escolher próxima conta via heurística
     - `auth_manager.set_active_account(next)`
     - recriar o `ModelClientSession` (evitar websocket reaproveitado)
     - emitir evento/warning “SwitchAccount: A → B (motivo: usage_limit_reached, reset: HH:MM)”
     - retry (com contador de tentativas)
   - caso contrário: retornar o erro como hoje (UI mostra mensagem)

Guardrails:
- `max_switches_per_turn = accounts.len()` (ou valor configurável com default seguro)
- se uma conta falhar repetidamente por `usage_limit_reached`, marcar `exhausted_until` e não tentar de novo no mesmo turno

## UX / Interação (TUI)

### Onde o usuário gerencia contas

Adicionar um comando: `/accounts`

Conteúdo do modal/painel:
- Lista de contas (radio/selector):
  - marcador de ativa
  - email (se disponível), plan (se disponível), label
  - status weekly: `secondary.used_percent` / reset time
  - credits balance (se houver)
  - “exhausted until” (se marcado por erro)
- Ações:
  - **Add account...** (abre fluxo de login; ao finalizar, a nova conta vira ativa)
  - **Enter**: set active
  - (future) remove account
  - (future) edit label

### Onboarding/login (primeiro run e quando não autenticado)

Se existem contas salvas:
- Opção default: “Usar conta existente” (lista + Enter)
- Outra opção: “Adicionar nova conta” (ChatGPT browser login / device code)

Se não existem contas:
- comportamento atual permanece

### Auto switch feedback

Quando houver troca automática:
- não interromper o fluxo com prompt
- emitir um warning/evento visível no chat/log:
  - “Limite atingido na conta X (reset HH:MM). Trocando automaticamente para Y e tentando novamente…”

### Logout

Hook no comando `/logout`:
- se há 1 conta: logout all (apaga o store) e sai
- se há >1: abrir selector:
  - **Logout all accounts** (apaga o store) e sai
  - **Remove selected account** (remove só uma conta; mantém as outras) e sai
- se remover a ativa: o `AuthManager` escolhe outra conta ativa automaticamente

## CLI (não-TUI)

Adicionar subcommands (fase recomendada, mas pode vir depois do MVP):
- `codex accounts list`
- `codex accounts add` (browser/device code)
- `codex accounts use <id>`
- `codex accounts remove <id>`
- `codex accounts rename <id> <label>`
- `codex accounts doctor` (validar store/migração)

## Config

Adicionar em `ConfigToml` (em `codex-rs/core/src/config/mod.rs`):
- `auth.auto_switch_on_usage_limit = false` (default)

(Opcional) knobs avançados:
- `auth.auto_switch_max_per_turn = <int>`
- `auth.auto_switch_strategy = "lowest_weekly_remaining"` (default; prefer `credits.balance`, fallback: secondary window remaining) | "round_robin"

## Testes

- Unit tests (storage):
  - migração legado → V1 (file + keyring)
  - add/remove/set_active
  - remove ativa com fallback para outra conta
- Unit tests (seleção):
  - escolhe menor saldo weekly (secondary used_percent)
  - respeita `exhausted_until`
  - respeita `forced_chatgpt_workspace_id`
- Integration tests (core):
  - simular `usage_limit_reached` na conta A e sucesso na conta B no mesmo turno (sem `EventMsg::Error` final)

## Checklist de execução (por fases)

### Fase 1 — Storage + APIs core (sem UI nova)

- [x] Definir `AuthStore` (v1) + loader que aceita legado
- [x] Implementar migração lazy
- [x] Implementar `AuthManager` multi-account (list/add/remove/set_active)
- [x] Persistir `active_account_id` e validar invariantes (ativa ∈ accounts)
- [x] Adicionar testes de storage/migração (file + keyring)

### Fase 2 — Auto switch + retry no mesmo turno (core)

- [x] Adicionar config `auth.auto_switch_on_usage_limit`
- [x] Implementar cache de uso por conta (`AccountUsageCache`) + persistência best-effort do `RateLimitSnapshot`
- [x] Implementar `mark_usage_limit_reached` (persistir `exhausted_until` + rate limits quando disponíveis)
- [x] Implementar heurística “menor saldo weekly” (weekly `secondary.used_percent`, depois `credits.balance`, fallback: `primary.used_percent`)
- [x] Implementar retry dentro de `run_sampling_request` com guardrails
- [x] Adicionar eventos/warnings de troca automática
- [x] Teste de integração: 429 → switch → sucesso

### Fase 3 — UX (TUI)

- [x] `/accounts` (lista, set active, add account)
- [x] Logout selector quando há várias contas
- [x] Snapshots / testes de UI (tui)
- [ ] `/accounts` (remove, label)
- [ ] Onboarding: “usar conta existente” vs “adicionar nova”
- [x] Exibir conta ativa em `/status`
- [ ] Exibir conta ativa no header do chat (opcional)

### Fase 4 — CLI accounts (opcional)

- [ ] `codex accounts list/add/use/remove/rename/doctor`
- [ ] Docs (`docs/slash_commands.md`) atualizados
- [x] Docs (`docs/authentication.md`) atualizar (mencionar multi-account store e migração)

## Notas de rollout

- Implementar “feature flag” via config e manter default conservador (`false`).
- Documentar que o auto switch assume que o usuário tem direito de usar as contas configuradas.
