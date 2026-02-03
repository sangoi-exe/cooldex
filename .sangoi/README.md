# `.sangoi/` — contexto local (versionado) + runbooks

Tudo que é **teu** (planos, guias/runbooks e task logs) vive aqui pra não se perder nem virar bagunça na raiz do repo.

## Estrutura

- `.sangoi/docs/` — guias/runbooks (ex.: sync upstream → main → master)
- `.sangoi/plans/` — planos e rascunhos de execução
- `.sangoi/task-logs/` — logs do que foi feito (com data)
- `.sangoi/local/` — artefatos locais (ignorado via `.sangoi/.gitignore`)

## Regras

- Se mexeu em processo: atualiza o runbook correspondente em `.sangoi/docs/`.
- Se executou um procedimento relevante: registra em `.sangoi/task-logs/`.

