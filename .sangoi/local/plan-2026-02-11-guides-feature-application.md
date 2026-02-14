# Atualização dos guias de aplicação de features (2026-02-11)

Label: **medium**

## Scope + artefatos
- Criar/atualizar guias de reaplicação no working tree (copiando do backup `backup/reapply-state-20260209-125806` e incorporando lições reais do port `/accounts`).
- Sincronizar também `docs/multi-account-auth-plan.md` com as lições operacionais.
- Artefatos alvo:
  - `.sangoi/docs/guide-reapply-accounts.md` (novo no branch)
  - `.sangoi/docs/guide-reapply-mods-order.md` (novo no branch)
  - `.sangoi/docs/guide-reapply-subagents.md` (novo no branch)
  - `docs/multi-account-auth-plan.md` (existente)

## Checklist
- [x] 1) Recon dos guias atuais/backup
  - Done: diferenças de conteúdo e gaps identificados.
- [x] 2) Planejamento validado pelo Senior Plan Advisor
  - Done: plano revisado sem blockers e com DoD verificável.
- [x] 3) Portar guias do backup para `.sangoi/docs`
  - Done: 3 guias presentes no branch com conteúdo base + ajustes.
- [x] 4) Injetar aprendizados de `/accounts`
  - Done: contratos cross-file, persistência sem overwrite, fail-safe de logout, e comandos de validação atualizados.
- [x] 5) Sincronizar `docs/multi-account-auth-plan.md`
  - Done: seção objetiva com pitfalls reais e checks operacionais adicionada.
- [x] 6) Validar consistência textual
  - Done: sem contradições entre guias; comandos executáveis e coerentes com ambiente atual.
- [x] 7) Gate final com Senior Code Reviewer
  - Done: veredito READY/READY_WITH_NITS.

## Validação planejada
- `rg -n "upsert|update_auth_store|LogoutAllAccounts|--no-run|pending-snapshots|completed_status_message" .sangoi/docs/guide-reapply-*.md docs/multi-account-auth-plan.md`
- `rg -n "guide-reapply-accounts|guide-reapply-mods-order|guide-reapply-subagents" .sangoi/docs`

## Fan-out
- Senior Plan Advisor (read-only): revisar completude e clareza do plano/documentação.
- Root lane (mutating): edição dos guias/docs.
- Senior Code Reviewer (read-only): revisão final dos docs alterados.
