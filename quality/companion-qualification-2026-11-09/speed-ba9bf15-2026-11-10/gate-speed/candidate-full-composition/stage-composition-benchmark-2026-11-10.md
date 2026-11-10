# Stage composition benchmark

- generated_at: 2026-11-10T11:06:27.379632300+00:00
- mode: fixture
- code_base: ba9bf15b5945e369ecb0ab8d9d97a5a364f80070
- fixture_version: stage-composition-fixtures-v1
- fixtures_sha256: 786e5cc1da8409dc4985da0d27cea9c388d4c42fc82a82121ce1515c1f093c5e
- host: windows x86_64

Fixture mode: every worker reply is scripted from the prompt the host rendered; tokens are **simulated** (characters over four) and there is **no monetary conclusion**. What this mode proves is orchestration: calls, retries, checkpoints, publication and the oracle on the published files.

## Arms

| Arm | Set | Fixtures | Completed | Correct | Tasks correct | Calls | Rejected | Interventions | Input tokens | Output tokens | Tokens / correct task | Spend / correct task (µUSD) | Wall p50 ms | Wall p95 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A: pipeline v3 (legacy, one call per stage) | development | 16 | 16 | 16 | 20/20 | 100 | 0 | 0 | 288435 | 14728 | 15158.1 | unknown | 5741 | 8329 |
| A: pipeline v3 (legacy, one call per stage) | held-out | 4 | 4 | 4 | 6/6 | 25 | 0 | 0 | 71766 | 3874 | 12606.7 | unknown | 5023 | 8429 |
| B: pipeline v4, one unit per stage | development | 16 | 16 | 16 | 20/20 | 100 | 4 | 0 | 179387 | 13977 | 9668.2 | unknown | 4525 | 5607 |
| B: pipeline v4, one unit per stage | held-out | 4 | 4 | 4 | 6/6 | 25 | 1 | 0 | 44629 | 3731 | 8060.0 | unknown | 5333 | 5592 |
| C: pipeline v4, planned units, sequential | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 210183 | 18168 | 11417.5 | unknown | 5146 | 5700 |
| C: pipeline v4, planned units, sequential | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 55240 | 4899 | 10023.2 | unknown | 5425 | 5841 |
| D: pipeline v4, planned units, up to two in flight | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 210183 | 18168 | 11417.5 | unknown | 4962 | 6759 |
| D: pipeline v4, planned units, up to two in flight | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 55240 | 4899 | 10023.2 | unknown | 5223 | 5547 |

## Families (development set)

| Family | Arm | Correct | Calls | Input tokens | Output tokens | Interventions |
|---|---|---|---|---|---|---|
| CohesiveMultiFile | A | 4/4 | 24 | 70518 | 3286 | 0 |
| CohesiveMultiFile | B | 4/4 | 24 | 43818 | 3137 | 0 |
| CohesiveMultiFile | C | 4/4 | 28 | 51972 | 4358 | 0 |
| CohesiveMultiFile | D | 4/4 | 28 | 51972 | 4358 | 0 |
| FailureRecovery | A | 4/4 | 28 | 75323 | 3768 | 0 |
| FailureRecovery | B | 4/4 | 28 | 47025 | 3465 | 0 |
| FailureRecovery | C | 4/4 | 31 | 52779 | 4422 | 0 |
| FailureRecovery | D | 4/4 | 31 | 52779 | 4422 | 0 |
| SeparableMultiTask | A | 4/4 | 24 | 74879 | 4619 | 0 |
| SeparableMultiTask | B | 4/4 | 24 | 47153 | 4465 | 0 |
| SeparableMultiTask | C | 4/4 | 32 | 64569 | 5876 | 0 |
| SeparableMultiTask | D | 4/4 | 32 | 64569 | 5876 | 0 |
| SimpleMaintenance | A | 4/4 | 24 | 67715 | 3055 | 0 |
| SimpleMaintenance | B | 4/4 | 24 | 41391 | 2910 | 0 |
| SimpleMaintenance | C | 4/4 | 24 | 40863 | 3512 | 0 |
| SimpleMaintenance | D | 4/4 | 24 | 40863 | 3512 | 0 |

## Qualification per recipe

| Arm | Development correct | Held-out correct | Unexpected interventions | Orchestration qualified | Cost/quality qualified |
|---|---|---|---|---|---|
| B | true | true | 0 | true | pending (pilot not run) |
| C | true | true | 0 | true | pending (pilot not run) |
| D | true | true | 0 | true | pending (pilot not run) |

## Observations

| Fixture | Family | Arm | Completed | Correct | Calls | Rejected | Interventions | Input | Output | Prompt chars | Wall ms | Diagnostics |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| cohesive-01-session-expiry | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17653 | 821 | 70601 | 5808 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 11147 | 781 | 44580 | 4435 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 13185 | 1086 | 52733 | 5645 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 13185 | 1086 | 52733 | 4818 | — |
| cohesive-02-money-format | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17633 | 830 | 70519 | 4611 | — |
| cohesive-02-money-format | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 10909 | 791 | 43632 | 4374 | — |
| cohesive-02-money-format | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 12953 | 1097 | 51803 | 4858 | — |
| cohesive-02-money-format | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 12953 | 1097 | 51803 | 4693 | — |
| cohesive-03-slug-links | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17607 | 817 | 70416 | 4813 | — |
| cohesive-03-slug-links | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 10814 | 786 | 43248 | 5488 | — |
| cohesive-03-slug-links | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 12842 | 1090 | 51360 | 5664 | — |
| cohesive-03-slug-links | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 12842 | 1090 | 51360 | 6759 | — |
| cohesive-04-rate-window | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17625 | 818 | 70488 | 4536 | — |
| cohesive-04-rate-window | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 10948 | 779 | 43783 | 4371 | — |
| cohesive-04-rate-window | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 12992 | 1085 | 51960 | 4597 | — |
| cohesive-04-rate-window | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 12992 | 1085 | 51960 | 5019 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17629 | 814 | 70511 | 4735 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 10888 | 780 | 43545 | 5333 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 12922 | 1086 | 51678 | 5841 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 12922 | 1086 | 51678 | 4506 | — |
| recovery-01-transport-on-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 16887 | 759 | 78370 | 7366 | — |
| recovery-01-transport-on-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 10627 | 725 | 50318 | 4436 | — |
| recovery-01-transport-on-code | FailureRecovery | C | true | true | 7 | 1 | 0 | 10473 | 875 | 48001 | 4635 | — |
| recovery-01-transport-on-code | FailureRecovery | D | true | true | 7 | 1 | 0 | 10473 | 875 | 48001 | 4669 | — |
| recovery-02-malformed-intake | FailureRecovery | A | true | true | 7 | 0 | 0 | 19263 | 873 | 77044 | 8329 | — |
| recovery-02-malformed-intake | FailureRecovery | B | true | true | 7 | 1 | 0 | 11566 | 784 | 46252 | 4615 | — |
| recovery-02-malformed-intake | FailureRecovery | C | true | true | 7 | 1 | 0 | 11430 | 935 | 45707 | 4669 | — |
| recovery-02-malformed-intake | FailureRecovery | D | true | true | 7 | 1 | 0 | 11430 | 935 | 45707 | 4608 | — |
| recovery-03-transport-on-test | FailureRecovery | A | true | true | 7 | 0 | 0 | 18619 | 1152 | 88043 | 7357 | — |
| recovery-03-transport-on-test | FailureRecovery | B | true | true | 7 | 1 | 0 | 11853 | 1114 | 55726 | 5096 | — |
| recovery-03-transport-on-test | FailureRecovery | C | true | true | 9 | 1 | 0 | 16190 | 1466 | 74395 | 5700 | — |
| recovery-03-transport-on-test | FailureRecovery | D | true | true | 9 | 1 | 0 | 16190 | 1466 | 74395 | 5784 | — |
| recovery-04-malformed-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 20554 | 984 | 82205 | 7602 | — |
| recovery-04-malformed-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 12979 | 842 | 51906 | 5013 | — |
| recovery-04-malformed-code | FailureRecovery | C | true | true | 8 | 1 | 0 | 14686 | 1146 | 58735 | 5147 | — |
| recovery-04-malformed-code | FailureRecovery | D | true | true | 8 | 1 | 0 | 14686 | 1146 | 58735 | 4962 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | A | true | true | 7 | 0 | 0 | 18605 | 1158 | 86060 | 8429 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | B | true | true | 7 | 1 | 0 | 11887 | 1121 | 56572 | 4791 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | C | true | true | 9 | 1 | 0 | 16223 | 1475 | 72081 | 5425 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | D | true | true | 9 | 1 | 0 | 16223 | 1475 | 72081 | 5547 | — |
| separable-01-expiry-and-session | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18781 | 1156 | 75113 | 5717 | — |
| separable-01-expiry-and-session | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 12025 | 1109 | 48092 | 4525 | — |
| separable-01-expiry-and-session | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 16393 | 1462 | 65562 | 5454 | — |
| separable-01-expiry-and-session | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 16393 | 1462 | 65562 | 6359 | — |
| separable-02-retry-and-user | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18734 | 1153 | 74926 | 5741 | — |
| separable-02-retry-and-user | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 11730 | 1118 | 46911 | 4705 | — |
| separable-02-retry-and-user | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 16071 | 1470 | 64274 | 5349 | — |
| separable-02-retry-and-user | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 16071 | 1470 | 64274 | 5265 | — |
| separable-03-port-and-slug | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18634 | 1152 | 74525 | 4747 | — |
| separable-03-port-and-slug | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 11673 | 1117 | 46684 | 5607 | — |
| separable-03-port-and-slug | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 16021 | 1469 | 64067 | 5052 | — |
| separable-03-port-and-slug | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 16021 | 1469 | 64067 | 5129 | — |
| separable-04-window-and-money | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18730 | 1158 | 74914 | 5776 | — |
| separable-04-window-and-money | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 11725 | 1121 | 46889 | 4536 | — |
| separable-04-window-and-money | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 16084 | 1475 | 64326 | 5146 | — |
| separable-04-window-and-money | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 16084 | 1475 | 64326 | 5245 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18651 | 1147 | 74596 | 4854 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 11626 | 1107 | 46497 | 5592 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 15995 | 1463 | 63968 | 5096 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 15995 | 1463 | 63968 | 5223 | — |
| simple-01-token-expiry | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16904 | 763 | 67603 | 4511 | — |
| simple-01-token-expiry | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 10489 | 725 | 41950 | 4429 | — |
| simple-01-token-expiry | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 10358 | 875 | 41421 | 5287 | — |
| simple-01-token-expiry | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 10358 | 875 | 41421 | 4129 | — |
| simple-02-retry-limit | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 17004 | 768 | 68003 | 5839 | — |
| simple-02-retry-limit | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 10372 | 731 | 41482 | 4145 | — |
| simple-02-retry-limit | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 10236 | 882 | 40939 | 4173 | — |
| simple-02-retry-limit | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 10236 | 882 | 40939 | 4246 | — |
| simple-03-trim-username | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16837 | 759 | 67342 | 4761 | — |
| simple-03-trim-username | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 10225 | 728 | 40892 | 4386 | — |
| simple-03-trim-username | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 10092 | 878 | 40360 | 4379 | — |
| simple-03-trim-username | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 10092 | 878 | 40360 | 4395 | — |
| simple-04-port-parse | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16970 | 765 | 67868 | 4741 | — |
| simple-04-port-parse | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 10305 | 726 | 41205 | 4355 | — |
| simple-04-port-parse | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 10177 | 877 | 40696 | 4214 | — |
| simple-04-port-parse | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 10177 | 877 | 40696 | 4411 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16881 | 755 | 67514 | 5023 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 10228 | 723 | 40906 | 4596 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 10100 | 875 | 40395 | 4580 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 10100 | 875 | 40395 | 4157 | — |

## Limitations

- Fixture mode never calls a model: semantic quality is not measured and tokens are simulated from prompt/reply length; only relative prompt size between arms is meaningful.
- Pilot mode reports the provider's own usage; a run whose usage is partial or unavailable yields no monetary conclusion.
- Wall time on the loopback provider is serialized by the single-slot connection: arm D is not expected to be faster here.
- Oracles are content predicates on published files, independent of the tests the workers authored.
