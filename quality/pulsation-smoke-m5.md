# M5 — smoke visual GPUI

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Data: 2026-08-28/29. Branch `gpui-migration`. Implementação visual e correções: `3a2e2f0`, `d5025c5`, `16a1706`. Complemento ao [checkpoint M5](../quality/pulsation-sync-m5.md).

Status: **smoke visual concluído; G5 permanece aberto**. A evidência aprova os fluxos exercitados, não autoriza rollout, distribuição ou onboarding Brownfield.

## Ambiente e isolamento

- Windows 10.0.26200.0, x86_64, janela GPUI release real controlada por Computer Use.
- Executável final `target/release/hard-vibe-code.exe`: SHA-256 `ff623200fadf47d2ba7a56160470bea3a6df09b37d3789109725073d4043806b`.
- Fixture `pulsation_ui_fixture`, raiz `target/m5-ui-20260828e` e `HVC_DATA_DIR` explícito em `target/m5-ui-20260828e/data`. O perfil real não foi aberto.
- Provedor HTTP exclusivamente loopback; conexões remotas desativadas; referências de credenciais `m5-ui-fixture:*`. A GUI foi o único supervisor. Nenhuma pulsação simulada executou código, shell, Git ou LLM remoto.
- Preparação habilitada apenas na fixture. Default e perfil real permaneceram inalterados.
- SQL foi usado somente pela configuração sintética e por diagnósticos read-only. Criação, revisão, respostas, aprovações, cancelamento, reconciliação, troca de projeto e restart ocorreram pela interface.
- O auxiliar terminou depois do fechamento do aplicativo e da criação de `STOP`. Aplicativo e fixture não permaneceram ativos.

## Correções produzidas pelo smoke

1. A fixture passou a criar workspaces pelo bootstrap greenfield real antes dos arquivos sintéticos. A proteção contra abrir Brownfields sem onboarding não foi relaxada.
2. Descrições longas agora respeitam a largura do painel, mantêm cancelamento visível e não sobrepõem o cabeçalho do Companion. Campos de texto têm foco visual e pintura contida.
3. Pulsações terminais não oferecem ações que reabram o fluxo principal; Learning consultivo continua independente.
4. A fixture desativa conexões remotas, usa credenciais isoladas e só permite `--resume` em uma raiz marcada que contenha exatamente os dois projetos e as conexões esperadas (`d5025c5`).
5. O smoke encontrou starvation real: a consulta de admissão do Learning considerava qualquer job principal pendente, inclusive uma descrição bloqueada em revisão humana de outro projeto. A admissão agora bloqueia Learning apenas quando há slot/publicação principal ou trabalho principal efetivamente reivindicável. O teste `human_blocked_foreground_does_not_starve_learning_but_claimable_work_does` cobre os dois lados (`16a1706`).

## Matriz executada

| Fluxo | Evidência / resultado |
|---|---|
| Bootstrap e restauração isolada | **Aprovado.** Dois projetos sintéticos; seleção restaurada após restart. |
| Revisão da descrição | **Aprovado.** Pedido vago recebeu score 65; revisão específica virou revisão 2 e score 0. |
| Perguntas Specification e Companion | **Aprovado.** Pergunta persistiu sem chamada adicional; resposta `JWT assinado` retomou Specification. |
| Restart no esclarecimento | **Aprovado.** Perguntas, tentativas, chat e checkpoint reapareceram exatamente. |
| Criação, FIFO e cancelamento | **Aprovado.** B aguardou A; C cancelada permaneceu terminal após restarts. |
| Artefato sem path e HITL | **Aprovado.** Conteúdo de Specification foi visualizado e aprovado sem caminho no workspace. |
| Contract → Code → Test → Integrator → Done | **Aprovado.** A percorreu todos os checkpoints; somente o host confirmou `Done`. |
| Learning após Done | **Aprovado após correção.** Relatório `status: proposed`, uma chamada, sem alterar Done/fila/regras; artefato e limitações visualizados. |
| Conflito de publicação | **Aprovado.** B terminou em `publication_conflict`; `notes/conflict.md` preservou `Edição humana preservada.` e SHA-256 `215381d8a18907e164739442263ef45404b7efb3ea77cbb80d3ad850fbaed798`. |
| Reconciliação tipada | **Aprovado.** Explicação obrigatória e adoção explícita da base atual liberaram a base sem ressuscitar B/C nem alterar o arquivo humano. |
| Troca de projeto | **Aprovado.** O projeto Principal mostrou apenas sua própria pulsação; A/B/C do Segundo não vazaram. |
| Shutdown e restauração final | **Aprovado visualmente.** Fechamento gracioso, restart no mesmo perfil isolado e estados preservados. |
| Script nativo `scripts/smoke-windows.ps1` | **Não executado nesta rodada.** A política da ferramenta de controle visual impede combiná-la com automação Win32 direta no mesmo turno. Não é contado como evidência do script padronizado. |

IDs sintéticos para auditoria local: A `5124a74d-eefa-4c53-a871-92db032372dd`, B `50e6b8b1-e49f-423d-bee2-ffbae1f88648`, C `9263daa2-1709-455f-941e-380102152935`. Capturas e interações permanecem no registro da execução; o fixture e seu banco ficam sob `target/` e não são versionados.

## Gates posteriores à correção

| Verificação | Resultado |
|---|---|
| `cargo fmt --all -- --check` | Aprovado |
| `cargo clippy --workspace --all-targets -- -D warnings` | Aprovado |
| `cargo test --workspace` | **319 aprovados**, zero falhas/ignorados: 197 core, 14 editor, 41 test-support, 3 fixture e 64 UI |
| `cargo build --release` | Aprovado antes da rodada visual final; SHA-256 acima. Aviso preexistente de futura incompatibilidade em `proc-macro-error2 2.0.1`. |
| Preservação do arquivo em conflito após shutdown | Conteúdo e SHA-256 idênticos |

## Reprodução segura

```powershell
cargo build --release --locked --offline -p hvc-app
cargo run --release --locked --offline -p hvc-test-support --example pulsation_ui_fixture -- target/m5-ui-NOVO
# Em outro terminal, iniciar target/release/hard-vibe-code.exe com
# HVC_DATA_DIR=<raiz absoluta>/target/m5-ui-NOVO/data.
# Fechar o aplicativo antes de criar STOP na raiz da fixture.
```

`--resume` existe somente para retomar uma fixture expirada e recusa perfil sem o marcador, as duas workspaces sintéticas exatas ou o isolamento de conexões esperado. Não selecionar o repositório de desenvolvimento, o cérebro do projeto ou um perfil real.

## Limites do gate

- O provider sintético comprova protocolo, persistência, prioridade e UI; não comprova qualidade semântica, execução do código, custo ou latência de LLM real.
- O smoke não substitui o script Win32 padronizado não executado nesta rodada.
- G5 permanece aberto pela evidência de desempenho já registrada: no cenário B com edição, P95 observado passou de 1.219,4 para 1.701,0 ms (+39,5%); no intervalo conservador aprovação→Specification, a cauda aumentou 15,8%. A amostra é exploratória, mas não sustenta ativação.
- `preparation_enabled` permanece `false` por default. Nenhuma alteração de AST, grafo, bootstrap ou descoberta Brownfield foi introduzida.
