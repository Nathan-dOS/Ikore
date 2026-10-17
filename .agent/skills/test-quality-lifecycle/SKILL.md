---
name: test-quality-lifecycle
description: >-
  Guia o fluxo de trabalho em 3 fases para planejamento, implementacao, execucao
  e documentacao de testes automatizados em qualquer projeto. Exige analise previa
  e aprovacao do plano de testes antes da escrita de codigo, execucao com 100% de
  sucesso e atualizacao da documentacao publica ou base de conhecimento ao final.
---

# Test Quality Lifecycle

Esta skill define o ciclo padrao e estruturado para concepcao, implementacao, execucao e documentacao de testes automatizados em qualquer codebase, garantindo determinismo, isolamento de dependencias e manutencao da base de conhecimento do projeto.

---

## Estrutura do Ciclo de Testes

O fluxo de testes e estritamente dividido em 3 fases sequenciais:

```
[ Fase 1: Analise e Planejamento ]
               │
               ▼ (Aprovacao do Plano pelo Usuario)
[ Fase 2: Implementacao e Execucao ]
               │
               ▼ (100% dos Testes Passando)
[ Fase 3: Documentacao e Atualizacao da Base de Conhecimento ]
```

---

## Fase 1: Analise e Planejamento de Testes

Antes de escrever qualquer linha de codigo ou teste, execute a analise de contexto e elabore o plano de testes.

### 1.1 Inspecao de Escopo
- Analisar as modificacoes recentes no codigo, requisitos, arquitetura ou novas funcionalidades.
- Identificar os componentes afetados, contratos de interface (APIs, IPC, rotas, tipos) e camadas arquiteturais.
- Mapear a taxonomia de testes necessaria:
  - **Testes Unitarios**: Logica de negocio pura, funcoes utilitarias, validadores, algoritmos e conversores.
  - **Testes de Integracao / Modulo**: Interacao entre servicos internos, persistencia, adaptadores, fallbacks e maquinas de estado.
  - **Testes de Interface / Componentes**: Renderizacao, manipulacao de eventos, atualizacoes de estado e tratamento de feedback ao usuario.
  - **Testes de Contrato / Protocolo**: Serializacao, deserializacao, tratamento de codigos de erro e mapeamento de envelopes.

### 1.2 Identificacao de Dependencias e Dubles de Teste
- Mapear dependencias externas que precisam de isolamento (redes, servicos de terceiros, banco de dados persistente, relogio do sistema, timers, eventos de plataforma).
- Definir a estrategia de dubles de teste (mocks, stubs, fakes, in-memory databases, servidores TCP/HTTP de teste efemeros).
- Garantir que nenhum teste dependa de credenciais reais ou conectividade de internet nao controlada.

### 1.3 Estrutura do Plano de Testes
O plano de testes deve ser apresentado ao usuario contendo:
1. **Objetivo e Escopo**: O que esta sendo coberto e quais camadas serao testadas.
2. **Cenarios de Sucesso (Happy Path)**: Fluxos nominais e casos de uso principais.
3. **Cenarios de Erro e Borda (Edge Cases)**: Falhas de autenticacao, timeouts, exaustao de limites (rate limits), payloads malformados, cancelamentos.
4. **Cenarios de Resiliencia / Recuperacao** (quando aplicavel): Fallbacks, disjuntores (circuit breakers), retentativas, recuperacao de streaming parcial.
5. **Harness e Ferramentas**: Runners e utilitarios que serao utilizados.

> [!IMPORTANT]
> **Ponto de Parada Obrigatorio**: Apos apresentar o plano de testes, aguarde a confirmacao e aprovacao explicita do usuario antes de iniciar a Fase 2.

---

## Fase 2: Implementacao e Execucao

Apos a aprovacao do plano, inicia-se a construcao e validacao dos testes.

### 2.1 Regras de Implementacao
- **Determinismo**: Testes devem produzir o mesmo resultado independentemente da ordem de execucao, fuso horario ou ambiente.
- **Isolamento de Efeitos Colaterais**: Cada teste deve limpar seus recursos, bancos temporarios ou mocks apos a execucao.
- **Assercoes Claras**: Testar comportamento e invariantes de contrato, evitando acoplamento excessivo com detalhes internos de implementacao.
- **Seguranca de Dados**: Nunca persistir nem registrar segredos, tokens ou dados sensiveis em fixtures ou assercoes.

### 2.2 Execucao e Verificacao
- Executar todas as suites de teste por meio dos comandos oficiais do projeto.
- Executar validacoes complementares de sanidade (checagem de tipos, linters, formato de codigo).
- Caso ocorra qualquer falha, diagnosticar a causa raiz, corrigir o teste ou o codigo de producao e reexecutar a suite inteira.

> [!IMPORTANT]
> **Integridade de Relatorio**: Nunca assumir ou declarar que testes passaram sem a confirmacao direta e sem erros do log de saida do runner de testes.

---

## Fase 3: Documentacao e Atualizacao da Base de Conhecimento

Apos a suite ser executada com 100% de sucesso, a base de conhecimento do projeto deve ser atualizada.

### 3.1 Identificacao da Base de Conhecimento

Neste repositorio, use [docs/testing.md](../../../docs/testing.md) para o fluxo publico e [quality/README.md](../../../quality/README.md) para evidencias datadas.

Verificar se o projeto possui um diretorio `docs/`, vault de documentacao, pasta de arquitetura/qualidade ou documentacao viva de testes.

### 3.2 Atualizacoes Obrigatorias (Se Existir Base de Conhecimento / Docs)
1. **Inventario de Testes**:
   - Atualizar a contagem total de testes automatizados por runner e modulo.
   - Registrar os nomes dos novos testes e suites adicionados.
   - Atualizar a data da ultima verificacao confirmada.
2. **Mapa / Visao Geral de Qualidade**:
   - Atualizar a baseline consolidada de testes e o status geral das validacoes.
3. **Backlog de Cobertura e Estabilizacao**:
   - Mover os itens concluidos nesta iteracao para a secao de entregas.
   - Registrar novos gaps, cenarios de melhoria ou testes futuros identificados durante a implementacao.

### 3.3 Relatorio de Conclusao (Se Nao Existir Base de Conhecimento)
Caso o projeto nao possua uma estrutura formal de documentacao viva:
- Gerar um resumo tecnico direto contendo:
  - Total de testes criados e executados.
  - Cenarios cobertos por camada.
  - Resultado das validacoes do runner.
  - Recomendacoes ou passos futuros de qualidade.

---

## Boas Praticas e Diretrizes Gerais

- **Independencia de Linguagem / Stack**: Esta logica se aplica a qualquer ecossistema (Rust, TypeScript, Python, Go, Java, C#, etc.).
- **Feedback Conciso**: Comunicar o progresso de forma objetiva, mantendo foco nos resultados verificaveis.
- **Evolucao Continua**: Sempre que novas funcionalidades forem adicionadas ao projeto, este ciclo deve ser acionado para manter a suite de testes e a documentacao sincronizadas com o estado real do codigo.
