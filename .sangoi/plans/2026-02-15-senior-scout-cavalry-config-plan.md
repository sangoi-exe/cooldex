# Plan (hard) — Senior Scout Cavalry em `~/.codex/config.toml`

## Objetivo
Criar instruções de `Senior Scout Cavalry` no bloco `developer_instructions`, com gatilho obrigatório para recon/busca, mantendo estilo/tom do stack atual e sem conflitar com semântica de sub-agent já existente.

## Decisions
- **Escopo de fontes:** mapear prompts/templates **runtime** usados no código + instruções ativas locais (`~/.codex/config.toml` e `subagent_instructions_file`).
- **Tipo do Scout:** `agent_type="default"` (pedido explícito do usuário).
- **Idioma do bloco novo:** inglês, para manter consistência do `developer_instructions` atual.
- **Obrigatoriedade:** uso padrão para todo recon/busca; exceção apenas em falha/limite operacional de spawn, com fallback fail-loud no lead agent.

## Lanes (fan-out -> fan-in)
- **Lane A — Runtime Prompt Map (Scout A):**
  mapear templates/prompts usados em runtime e vínculos com sub-agent.
  - Toca: leitura apenas.
- **Lane B — Active Instruction Stack (Scout B):**
  mapear precedência e contratos ativos em `config.toml`/`subagent_instructions_file`.
  - Toca: leitura apenas.
- **Lane C — Style/Tone Synthesis (Scout C):**
  extrair padrão textual com evidência (`file:line`).
  - Toca: leitura apenas.
- **Fan-in (lead):** consolidar tabela de cobertura + matriz de estilo + texto final do `Senior Scout Cavalry`.

## Checklist
- [x] 1) Inventariar prompts/templates runtime relevantes
  - Done criteria: tabela `arquivo -> como é carregado -> relevância` com foco em sub-agent.
  - Verification commands:
    - `rg -n "include_str!\\(|spawn_agent\\(|AgentRole|subagent_instructions_file|developer_instructions" /home/lucas/work/codex/codex-rs/core/src`
    - `nl -ba /home/lucas/work/codex/codex-rs/core/src/agent/role.rs | sed -n '1,180p'`
- [x] 2) Inventariar stack ativo local e precedência de instruções
  - Done criteria: mapa de precedência entre `developer_instructions`, `model_instructions_file` e `subagent_instructions_file`.
  - Verification commands:
    - `nl -ba /home/lucas/.codex/config.toml | sed -n '1,320p'`
    - `nl -ba /home/lucas/.codex/sangoi_subagent_instructions_jules.md | sed -n '1,260p'`
    - `nl -ba /home/lucas/work/codex/codex-rs/core/src/tools/handlers/collab.rs | sed -n '780,840p'`
- [x] 3) Extrair padrão de escrita/estilo/tom
  - Done criteria: matriz com pelo menos 8 anchors (`file:line`) cobrindo: Advisor, Reviewer, subagent base instructions e template core de orquestração.
  - Verification commands:
    - `sed -n '1,260p' /home/lucas/.codex/config.toml`
    - `sed -n '1,220p' /home/lucas/work/codex/codex-rs/core/templates/agents/orchestrator.md`
- [x] 4) Redigir e inserir `Senior Scout Cavalry` em `developer_instructions`
  - Done criteria: regra no playbook + seção dedicada com snippet `spawn_agent`/`send_input`/`wait` e contrato explícito de entrada/saída.
  - Verification commands:
    - `rg -n "Senior Scout Cavalry|Scout Cavalry|recon|search|spawn_agent" /home/lucas/.codex/config.toml`
- [x] 5) Validar consistência
  - Done criteria: TOML parseável, seção única, sem conflito textual com regras existentes.
  - Verification commands:
    - `python3 -c "import tomllib, pathlib; tomllib.loads(pathlib.Path('/home/lucas/.codex/config.toml').read_text()); print('OK')"`
    - `rg -n "^## Senior (Plan Advisor|Code Reviewer|Scout Cavalry)" /home/lucas/.codex/config.toml`
- [x] 6) Handoff
  - Done criteria: entregar diffs + tabela de cobertura + matriz de estilo + rationale das decisões.

## Riscos e mitigação
- Risco: colisão entre obrigatoriedade do Scout e limites de spawn.
  - Mitigação: exceção explícita para falha/limite operacional com fallback fail-loud.
- Risco: drift de tom/estrutura.
  - Mitigação: copiar o padrão de seção já usado por Advisor/Reviewer (role, job, rules, deliverable, commands).
- Risco: expectativa incorreta por causa de `subagent_instructions_file`.
  - Mitigação: registrar precedência ativa no handoff e manter foco no que o usuário pediu (editar `developer_instructions`).
