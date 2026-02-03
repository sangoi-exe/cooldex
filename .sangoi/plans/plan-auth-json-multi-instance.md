# Plan: Evitar perda de contas com múltiplos Codex (auth.json)

## Problema (observado)

Quando existem **múltiplas instâncias do Codex** rodando ao mesmo tempo (mesmo `CODEX_HOME`), apenas abrir `/accounts` em uma instância pode **reescrever** o `auth.json` e **perder** uma conta recém-adicionada em outra instância.

Sintomas:
- Uma conta adicionada em um Codex só aparece no outro após reabrir/reiniciar.
- Pior: se você abrir `/accounts` no Codex “desatualizado”, ele pode sobrescrever o `auth.json` e remover a conta nova.

## Recon (o que o código faz hoje)

Raiz provável:
- `AuthManager` mantém um **snapshot em memória** do `AuthStore`.
- Várias mutações (incluindo o cache de rate limits usado pelo `/accounts`) persistem via `AuthManager::update_store`, que hoje:
  - clona o store em memória, aplica mutação e faz `storage.save(...)` (overwrite completo).
- No TUI, `/accounts`:
  - pega a lista de contas via `auth_manager.list_accounts()` (pode estar stale),
  - busca rate limits, e
  - chama `auth_manager.update_rate_limits_for_accounts(...)` (que persiste e pode clobber).

Além disso:
- O backend file (`FileAuthStorage::save`) grava `auth.json` via truncate+write (não-atomic).
- Não há lock cross-process ao redor de “load → mutate → save”.

## Objetivos (DONE)

- **Zero data loss**: uma instância stale não pode remover contas criadas em outra.
- **/accounts** (e idealmente **/logout**) devem refletir o estado atual do store **ao abrir** (sem precisar reiniciar).
- Escritas no store devem ser:
  - **serializadas** entre processos (lock), e
  - **crash-safe** no backend file (atomic replace).
- **Fail loud**: se o store não puder ser carregado/parseado, não sobrescrever “por acidente”.

## Opções (com recomendação)

### Opção A — Lock + atomic replace (recomendado)
- Adicionar um lockfile estável (ex.: `$CODEX_HOME/auth.json.lock`) e adquirir um lock exclusivo com retry+timeout.
- Sob o lock:
  - carregar o store atual do backend,
  - aplicar a mutação,
  - validar,
  - persistir (file: write temp + rename).
- Resultado: sem clobber e sem arquivos truncados/corrompidos.

### Opção B — Parar de persistir rate limits em `auth.json` (parcial)
- Reduz escrita ao abrir `/accounts`, mas não resolve outras mutações (tokens, active account, etc.).

### Opção C — Merge “best-effort” sem lock (insuficiente)
- Ainda tem race: 2 writers podem perder updates.

## Implementação proposta (fan-out → fan-in)

### Lane A — Core (auth store safety)
Arquivos: `codex-rs/core/src/auth.rs`, `codex-rs/core/src/auth/storage.rs`
- [ ] Criar helper de lock do auth store (lockfile + retry bounded) reaproveitando o padrão já usado no repo.
- [ ] Tornar `AuthManager::update_store` seguro cross-process:
  - [ ] adquirir lock,
  - [ ] carregar store atual do backend (erro ≠ “voltar para default e sobrescrever”),
  - [ ] aplicar mutator,
  - [ ] validar,
  - [ ] salvar,
  - [ ] atualizar cache.
- [ ] Tornar `FileAuthStorage::save` crash-safe (atomic write):
  - [ ] escrever JSON em arquivo temporário no mesmo diretório,
  - [ ] `flush`/`fsync` conforme aplicável,
  - [ ] `rename`/replace para `auth.json`,
  - [ ] garantir permissões (0600) como hoje.
- [ ] Revisar outras escritas diretas (ex.: logout/save_auth) para seguir o mesmo locking/erro “fail loud” quando apropriado.

### Lane B — TUI (atualização de estado ao abrir popups)
Arquivos: `codex-rs/tui/src/app.rs`, `codex-rs/tui/src/chatwidget.rs`
- [ ] Ao abrir `/accounts`, garantir que a lista de contas vem do store atual:
  - [ ] chamar `auth_manager.reload()` (ou uma variante “strict” com `Result`) antes de `list_accounts`.
- [ ] Ajustar `/logout` para não deixar cache stale:
  - [ ] preferir `auth_manager.logout()` em vez de chamar `codex_core::auth::logout` direto, ou recarregar após logout.

### Lane C — Login server (adicionar conta com segurança)
Arquivo: `codex-rs/login/src/server.rs`
- [ ] Garantir que o fluxo de persistência do login faça “load → mutate → save” sob o mesmo lock.
- [ ] Remover o comportamento de “Failed to load existing auth store, overwriting” (fail loud; não sobrescrever store quebrado).

### Lane D — Testes (repro fix)
Arquivos: `codex-rs/core/src/auth.rs` (tests), possivelmente `codex-rs/tui/src/chatwidget/tests.rs`
- [ ] Adicionar teste que reproduz o bug:
  - [ ] criar `AuthManager` (snapshot em memória),
  - [ ] simular modificação externa do `auth.json` adicionando uma conta,
  - [ ] chamar `update_rate_limits_for_accounts` no manager stale,
  - [ ] assert: a conta nova **permanece** no store após a escrita.
- [ ] Rodar testes do core e do TUI; atualizar snapshots se necessário.

## Verificação (comandos)

- [ ] `cd codex-rs && just fmt`
- [ ] `cd codex-rs && cargo test -p codex-core`
- [ ] `cd codex-rs && cargo test -p codex-tui`
- [ ] (Opcional — confirmar antes) `cd codex-rs && cargo test --all-features`

## Perguntas (para cravar o comportamento esperado)

1) Você está usando `cli_auth_credentials_store = "file"` (gera `~/.codex/auth.json`) ou `keyring/auto`?  
2) As duas instâncias estão apontando para o **mesmo** `CODEX_HOME`?  
3) “Atualizar sem reiniciar” significa:
   - (a) aparecer ao abrir `/accounts`/`/logout` (recomendado; simples), ou
   - (b) atualização live automática (watcher de arquivo; maior escopo)?

