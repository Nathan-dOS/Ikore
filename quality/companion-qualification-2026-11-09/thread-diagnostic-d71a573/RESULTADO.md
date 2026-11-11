# Diagnóstico de CPU por thread — release 2531a81

Uma única execução diagnóstica usou o mesmo binário e fixture: 141 frames, 423 atualizações, três conversas, 120 amostras após 20 de aquecimento, viewport 1440×900. O diagnóstico não qualifica desempenho nem substitui o primeiro FAIL oficial (p95 12,3584 ms). Nesta execução instrumentada, p50=6,2829 ms, p95=9,5315 ms e p99=12,3067 ms; `passed=false`. O campo CIM de frequência do monitor ficou `null`; nenhum ajuste do monitor foi feito e não se presume uma frequência medida neste run.

O PID lançado foi 19556. A thread da janela visível, identificada por HWND→GetWindowThreadProcessId, foi 28244. O sampler coletou 58 pontos ordenados, 3.177 registros e 73 identidades de threads; nenhum erro, descarte ou saturação. Os intervalos reais ficaram em p50=31,35 ms e p95=34,95 ms. A janela apareceu na primeira observação aos 882,71 ms; a coleta acabou aos 1.829,94 ms, quando o script observou o relatório completo.

| Período amostrado | Parede | CPU da UI | CPU das demais threads capturadas |
|---|---:|---:|---:|
| Antes da janela | 850,18 ms | 312,50 ms | 765,63 ms |
| Transição de visibilidade | 30,54 ms | 15,63 ms | 171,88 ms |
| Janela visível | 920,09 ms | 781,25 ms | 796,88 ms |

Durante a janela visível, a UI acumulou 656,25 ms em user e 125,00 ms em kernel, equivalentes a 84,91% de um núcleo. As outras threads acumularam 531,25 ms user e 265,63 ms kernel; 734,38 ms foram registrados em threads com o rótulo estático `hvc-runtime`. Esses nomes não identificam a função executada nem provam que esse trabalho é descoberta ou hidratação.

A concorrência se concentrou no início visível. Nos primeiros dois grupos aproximados de 200 ms, 376,15 ms de parede tiveram 265,63 ms de UI e 781,25 ms das demais threads. Nos últimos três grupos, 543,94 ms de parede tiveram 515,63 ms de UI (94,79% de um núcleo) e apenas 15,63 ms das demais. Os grupos são definidos pelo fim de cada intervalo e têm incerteza de cerca de uma amostra; não são divisões dos frames medidos.

Portanto, a UI permanece perto de ocupar um núcleo depois que a atividade concorrente observada cai. A hipótese de que todo o custo vem apenas de workers de startup não explica este trecho tardio. Também há concorrência relevante no início. A coleta não mostra funções, stacks, esperas, compositor, CPU dos processos filhos nem quais intervalos produziram os picos p95: os dados oficiais de latência são ordenados. Não há base para atribuir a cauda a uma causa única ou aceitar uma nova otimização só com estes números.

Os contadores têm granularidade Windows. A soma visível capturada foi 1.578,125 ms contra 1.562,5 ms no contador do processo, diferença de 15,625 ms. O período contém 15,625 ms de contadores iniciais de threads recém-observadas, que não devem ser atribuídos estritamente a um intervalo. Leituras sequenciais e threads breves também limitam a comparação. O sampler externo consumiu 53,17 ms em suas consultas ao longo da coleta, com p95 de 1,20 ms por ponto e máximo de 6,35 ms; esse overhead pode perturbar o escalonamento.

A revisão independente recalculou os totais user/kernel dos registros, confirmou zero deltas negativos e nenhum TID repetido, sem bloqueio de agregação. Nenhuma segunda execução foi feita.

Após a coleta, os 965 arquivos do manifesto 2531a81 continuam com os mesmos hashes. O binário continua SHA-256 `56c4ef95c818bf77caae37ddc90ecd0816f64681b6d336e201197c50e7827b45`; o script oficial continua `83d0555c3cca098d8577132dae13bdb83ea034a886cbbe46361c1f4de9de9f47`. Rust, manifests, lock e script oficial não foram alterados. O aplicativo encerrou pelo caminho gracioso original e o diretório de perfil isolado ficou vazio. Nenhuma configuração persistente foi modificada.
