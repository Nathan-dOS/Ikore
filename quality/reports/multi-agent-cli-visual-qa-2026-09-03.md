# QA visual — painel Agents CLI/API

**Data:** 2026-09-03  
**Plataforma:** Windows x64, captura nativa via Windows Graphics Capture  
**Tema disponível:** escuro  
**Fixture:** `agents-ui-qa-20260903b`

## Reprodução

```powershell
cargo run -p hvc-test-support --example agents_ui_fixture -- target\agents-ui-qa-<sufixo-novo>
$env:HVC_DATA_DIR = '<raiz>\data'
$env:PATH = '<raiz>\bin;' + $env:PATH
$env:HVC_QA_WINDOW_SIZE = '1024x768'
cargo run -p hvc-app --bin hard-vibe-code
```

O fixture se recusa a reutilizar uma raiz existente. Ele cria workspace e banco isolados, configura os sete agentes entre Codex, Claude, Gemini, OpenCode, Custom e API, simula estados completed/failed/cancelled/interrupted/running/fallback, inclui saída Unicode longa e redigida, duas sessões FreeForm ativas e uma arquivada. Estados adicionais de discovery podem ser selecionados por `HVC_CLI_FIXTURE_<BACKEND>_STATE`.

## Matriz observada

| Janela solicitada | Área capturada | Cenário revisado | Resultado |
|---|---:|---|---|
| 1024×768 | 1026×799 com decoração | CLI Mode, API Mode e editor inline | aprovado; uma coluna, metadados legíveis, ações principais visíveis, headers e editor quebram linha sem overflow horizontal |
| 1280×720 | 1282×751 com decoração | CLI Mode | aprovado; uma coluna, ações e diagnóstico visíveis |
| 1440×900 | 1442×931 com decoração | CLI Mode, estados simultâneos | aprovado; duas colunas, redaction e status textuais visíveis |
| 1920×1080 | host limitou a captura a 1512 px e cortou a lateral | tentativa CLI Mode | inconclusivo neste host; não contado como aprovação física |
| 2560×1440 | maior que a superfície física disponível | não capturável integralmente | bloqueado por hardware do host |

## Cenários inspecionados

- execução CLI concluída, falha, cancelamento, recovery como interrupted e execução ativa;
- ausência de OpenCode e estados configuráveis de autenticação/versão;
- fallback pendente com ações explícitas;
- API Mode com tabela de bindings, viewer e edição inline;
- Custom aguardando confirmação de risco;
- histórico longo, Unicode, segredo redigido e exportação;
- FreeForm retomável, Custom e lista arquivada com restaurar/excluir.

## Defeitos encontrados e corrigidos

1. O painel Agents usava largura integral dentro de uma linha que também continha a sidebar, causando overflow equivalente à largura da navegação. A raiz passou a ocupar o espaço flexível restante com `min_w_0/min_h_0`.
2. Metadados e ações competiam pela mesma linha em 1024 px, comprimindo texto caractere a caractere. Metadados foram movidos para uma linha própria e toolbars/headers passaram a quebrar de forma previsível.
3. API Mode e editor inline mantinham grupos rígidos. Headers, ações e campos agora reorganizam sem ocultar Configurar, Salvar ou Cancelar.

## Cobertura complementar automatizada

- `qa_window_size_accepts_only_supported_finite_dimensions` torna as dimensões de debug reproduzíveis sem alterar o release.
- `live_terminal_event_buffer_keeps_only_the_latest_five_hundred_events` prova limite visual sob 1.000 eventos.
- `api_activity_is_project_scoped_redacted_and_bounded` prova escopo, redaction e limite de 200 itens.
- Os testes de text input cobrem Unicode e composição IME; status incluem texto e não dependem apenas de cor.

## Pendências manuais não mascaradas

- Repetir 1920×1080 e 2560×1440 em monitor/superfície física capaz de capturar toda a janela.
- Repetir em escala Windows 100%, 125% e 150% e validar ordem/foco de teclado. A árvore UIA do GPUI neste host não expôs controles internos de forma observável.
- Tema claro não pode ser validado porque o produto atual não oferece alternância/implementação de tema claro.

Essas pendências são gates físicos de QA, não lacunas conhecidas de implementação da feature. O relatório não declara aprovado aquilo que o host não conseguiu observar.
