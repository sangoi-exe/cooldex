# Plano — Compilado de instruções frontend (`dammyjay93/interface-design`)

**Dificuldade:** medium

## Objetivo
Produzir um arquivo `.markdown` com compilado das instruções de frontend extraídas do repositório remoto `https://github.com/dammyjay93/interface-design`, com rastreabilidade de fonte (`arquivo:linha`) e inspeção feita com `gh`.

## Assunções operacionais (até ajuste do usuário)
- Idioma de saída: português.
- Formato: resumo consolidado (não cópia literal extensa).
- Caminho de saída: `./.sangoi/docs/interface-design-frontend-instructions-compilado.markdown`.

## Escopo e artefatos
- **Entrada (verbatim):**
  - `inspecione o repositório https://github.com/dammyjay93/interface-design, use o comando gh, identifique as instruções de frontend, e crie um arquivo .markdown com um compilado dessas instruções`
- **Inspeção remota:** `gh repo view`, `gh api`, `gh repo clone`.
- **Saída alvo:** `./.sangoi/docs/interface-design-frontend-instructions-compilado.markdown`

## Critério de inclusão/exclusão
- **Incluir:** regras explícitas de UI, layout, estilo, componentes, UX e processo de design frontend.
- **Excluir:** instruções de backend/devops sem impacto direto em frontend e inferências não explícitas.

## Verificação executável
- Descobrir arquivos candidatos de forma reprodutível:
  - `gh api repos/dammyjay93/interface-design/git/trees/main?recursive=1 --jq '.tree[].path'`
- Confirmar artefato final:
  - `test -s .sangoi/docs/interface-design-frontend-instructions-compilado.markdown`
- Confirmar rastreabilidade no arquivo final:
  - `rg -n 'Fonte:\s*`[^`]+:[0-9]+`' .sangoi/docs/interface-design-frontend-instructions-compilado.markdown`
- Revisão de conteúdo final:
  - `sed -n '1,260p' .sangoi/docs/interface-design-frontend-instructions-compilado.markdown`

## Checklist
- [x] Recon inicial com `gh` e mapeamento dos arquivos relevantes.
- [x] Consolidar instruções com taxonomia clara (fundamentos, processo, validação, memória, comandos de apoio).
- [x] Redigir compilado em `.markdown` com citação `arquivo:linha` em cada bloco.
- [x] Validar completude, legibilidade e aderência ao pedido.
- [x] Rodar gate de revisão final (`Senior Code Reviewer`) antes da entrega.

## DONE criteria
- [x] Arquivo `.markdown` criado no caminho definido.
- [x] Compilado contém apenas instruções frontend explícitas.
- [x] Todo bloco de instrução possui fonte `arquivo:linha`.
- [x] Evidência de uso de `gh` registrada na execução.

## Lanes (fan-out → fan-in)
- **Implementação:** lane única (escopo curto e linear).
- **Gate 1:** `Senior Plan Advisor` para revisão crítica do plano.
- **Gate 2:** `Senior Code Reviewer` com diff + validações executadas.
- **Fan-in:** incorporar ajustes dos gates, revalidar arquivo e entregar.
