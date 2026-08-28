# Sincronização de pulsações — checkpoint M2

Data: 2026-08-28. Repositório: `C:\Dev\Hard-Vibe-Code`, branch `gpui-migration`.
Base: `4d220ba` (M1). Implementação: `541ec1d`.
Decisão: ADR-007 e REGRAS de sincronização do cérebro do projeto.

Status: **M2 concluído; G2 aprovado**. Não é autorização de rollout M5.

## Entrega

| Item | Implementação |
|---|---|
| M2.1 | FIFO imutável aplicado à admissão SQLite; proprietário persistido do início de Specification até encerramento seguro. HITL mantém o proprietário e libera o executor. Projetos distintos podem ter proprietários simultâneos. |
| M2.2 | Aprovação final vinculada ao checkpoint, revisão, versão e produtor. Publicações têm prioridade; a barreira confere hashes atuais e referências explícitas de fontes quando presentes. `Done`, evento, epoch, liberação e intenção Learning são uma transação. Aprovação antiga nunca finaliza uma revisão nova. |
| M2.3 | Supervisor do AppCore com duas lanes, reservas globais únicas `main`/`background`, notificação registrada antes da inspeção e reconciliação de segurança a cada 1 s. Cada passagem publica no máximo oito unidades. Claim e publicação usam trabalho bloqueante aguardado; não há pools autônomos. |
| M2.4 | A intenção de cancelamento impede a próxima unidade de arquivo; a unidade já iniciada termina com ACK sob serialização SQLite. A reserva de executor sobrevive ao cancelamento lógico até o retorno da execução. Shutdown aguarda a unidade bloqueante iniciada; acima de 3 s registra diagnóstico e continua aguardando, sem destacar a escrita. |
| M2.5 | Snapshot coerente de fila, proprietário, bloqueio, publicações e contadores separados. UI permite atualizar a revisão, escolher explicitamente um trabalho legado, informar justificativa e adotar a base atual. Rejeita versões/hashes antigos; mantém os demais trabalhos e todo o histórico. |
| M2.6 | Testes A/B/C, empate de timestamp, claims/aprovações concorrentes, projetos distintos, HITL, background simulado, crash, publicação/cancelamento com barreiras, wakeup, falha de setup, migrações V9/V10 e ações versionadas da UI. |

## Dados e reconciliação

- Migration aditiva **V11**; V1–V10 não foram alteradas. Backup `VACUUM INTO`, incluindo WAL, antes do upgrade, com `integrity_check`. O backup novo usa `hard-vibe-code.pre-v11.<uuid>.db`.
- Reservas de execução não são leases distribuídos. No startup, o lock exclusivo do AppCore permite remover reservas de processos encerrados, preservar o proprietário e retomar somente a execução atual não aceita.
- Múltiplos trabalhos avançados, inclusive produzidos durante M1, ficam em `coordination_hold`. O usuário escolhe qual retomar. Ao encerrá-lo, os restantes voltam a exigir escolha; não são descartados ou eleitos silenciosamente.
- Reconciliação é adoção humana explícita do estado atual, **não** retry de publicação, rollback nem sucesso da tarefa. Registra justificativa, observações, seleção, versão e novo epoch em `base_reconciliations`.
- `project_file_baselines` mantém os hashes publicados/adotados para comparação otimista do próximo trabalho. Arquivos protegidos/ilegíveis não viram destinos autorizados por essa operação.
- Evidências de trabalho ativo continuam sujeitas à verificação final, mesmo depois de uma reconciliação. Evidência legada ausente/ambígua ou divergente não é inventada: a UI permite cancelar explicitamente o trabalho, mantendo suas saídas, antes de adotar a base e criar uma nova tarefa.
- O arquivo substituído atomicamente é a unidade de efeitos. Não há transação global com editores externos nem rollback de um conjunto de arquivos; edições divergentes exigem revisão.

## Limites de ativação

1. O slot de background está reservado na arquitetura, mas sua lane permanece sem executores habilitados nesta etapa. Intake antecipado e PreSpec reais só serão conectados em M3; a independência da barreira foi testada com uma reserva de preparação simulada.
2. A intenção Learning é única por `(pulsation_id, integration_epoch, report_version)`, somente para v2 habilitada. Em M2 é registrada como `unavailable`, não como job executável. M4 não deve transformar essas intenções históricas em Learning retroativo. Desabilitado ou pipeline v1 não cria intenção.
3. Não há TaskRefine, contexto de código sob demanda, perguntas no Companion, coletor AST, fronteira Brownfield, novo banco vetorial ou mudança de provider. A reindexação pré-estágio existente só será substituída em M4.
4. A barreira comprova as publicações conhecidas e as fontes explícitas disponíveis; não certifica veracidade semântica de notas, execução de testes/Git ou ausência de alterações desconhecidas no repositório. “No-op” significa nenhuma publicação de arquivo exigida, não uma análise completa do workspace.
5. UI M2 é a recuperação mínima: paths, hashes, diagnósticos, seleção e confirmação. Código deve ser conferido no editor externo; o editor atual do app continua limitado a `.md`/`.txt`. Visualização completa de artifacts e os demais fluxos humanos permanecem M3/M5.
6. Não foram adicionadas dependências, rotas LLM, estágios de pipeline, executores Git/testes ou ativação Brownfield. O perfil real do aplicativo não foi usado para validar a migration.

## Evidência automatizada

- `crates/hvc-core/src/orchestration/coordination_tests.rs`: 18 novos testes de estado/concorrência, com SQLite e filesystem reais, barreiras e falhas injetadas. Inclui escrita iniciada antes de cancelamento/shutdown e proteção após reconciliação manual.
- `crates/hvc-test-support/tests/coordination_upgrade.rs`: dois testes de upgrade V9/V10, preservação de IDs/checkpoints/outputs, backup e escolha explícita de proprietário.
- `crates/hvc-ui/src/pulsations.rs`: três testes de seleção, justificativa, clique duplicado, congelamento de projeto/epoch/versão/hashes e ausência de reabertura de tarefa terminal.
- Regressões M0/M1 continuam executadas. A fixture `serial_worker.rs` usa parser/roteador/provider HTTP simulado reais; o supervisor interrompe novas unidades no shutdown e o recovery publica resultados já aceitos sem nova chamada.

## Validação

| Verificação | Resultado |
|---|---|
| Baseline antes de editar | 247 testes aprovados; worktree limpo |
| `cargo fmt --all -- --check` | Aprovado |
| `cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` | Aprovado, sem warnings do workspace |
| `cargo test --workspace --locked --offline --quiet` | 270 aprovados: 162 core, 14 editor, 19 test-support, 16 integrações, 59 UI (254 unitários + 16 integrações) |
| `cargo build --release --locked --offline -p hvc-app` | Aprovado no código final `541ec1d` |
| `powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1` | Aprovado novamente no binário final: janela encontrada, banco isolado inicializado, shutdown gracioso, exit code 0 |
| `git diff --check` | Aprovado no fechamento do código e da documentação |
| Auditoria de licenças/fontes | Aprovada tecnicamente: 549 pacotes, sem licença negada/ausente ou fonte não aprovada |
| RustSec | Exit 0; zero vulnerabilidades reportadas; 10 avisos `unmaintained`, 2 `unsound`, 1 `yanked`; base `6420e39260b3d771b049954cf5d52b57e2118da4` |
| Política de distribuição | Continua bloqueada por `zed-55470`, conforme `quality/distribution-policy.json`; M2 não altera essa decisão |

Os relatórios locais das auditorias estão em `target/stage7/supply-chain.json` e `target/stage7/advisories/`. O aviso de incompatibilidade futura de `proc-macro-error2 2.0.1` é preexistente. Não houve atualização de dependências para suprimir os avisos.

As verificações foram chamadas individualmente, não pelo wrapper `gate-stage7.ps1`. O smoke cobre inicialização/encerramento da janela em dados temporários, não a matriz manual completa da UI. Benchmark, ganho de latência, rollout, desligamento de preparação em execução real e a matriz completa M5 continuam pendentes.

## Próximo marco

M3: contexto comum sob demanda, TaskRefine, preparação do sucessor imediato, revalidação e fluxo humano completo. Reusar as reservas e o lifecycle M2; não criar executores fora do supervisor nem tornar a preparação pré-requisito de Specification.
