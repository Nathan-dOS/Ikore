# Stage composition benchmark

- generated_at: 2026-09-13T18:20:01.232974700+00:00
- mode: fixture
- code_base: f8235ab2fb8a7af963c8eb0f0d8aaab27c2bf553
- fixture_version: stage-composition-fixtures-v1
- fixtures_sha256: 786e5cc1da8409dc4985da0d27cea9c388d4c42fc82a82121ce1515c1f093c5e
- host: windows x86_64

Fixture mode: every worker reply is scripted from the prompt the host rendered; tokens are **simulated** (characters over four) and there is **no monetary conclusion**. What this mode proves is orchestration: calls, retries, checkpoints, publication and the oracle on the published files.

## Arms

| Arm | Set | Fixtures | Completed | Correct | Tasks correct | Calls | Rejected | Interventions | Input tokens | Output tokens | Tokens / correct task | Spend / correct task (µUSD) | Wall p50 ms | Wall p95 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A: pipeline v3 (legacy, one call per stage) | development | 16 | 16 | 16 | 20/20 | 100 | 0 | 0 | 273808 | 14728 | 14426.8 | unknown | 4052 | 6989 |
| A: pipeline v3 (legacy, one call per stage) | held-out | 4 | 4 | 4 | 6/6 | 25 | 0 | 0 | 68162 | 3874 | 12006.0 | unknown | 4236 | 6889 |
| B: pipeline v4, one unit per stage | development | 16 | 16 | 16 | 20/20 | 100 | 4 | 0 | 121316 | 13977 | 6764.6 | unknown | 3924 | 4861 |
| B: pipeline v4, one unit per stage | held-out | 4 | 4 | 4 | 6/6 | 25 | 1 | 0 | 30391 | 3731 | 5687.0 | unknown | 4329 | 4856 |
| C: pipeline v4, planned units, sequential | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 126162 | 18168 | 7216.5 | unknown | 4199 | 4892 |
| C: pipeline v4, planned units, sequential | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 32694 | 4899 | 6265.5 | unknown | 4386 | 4986 |
| D: pipeline v4, planned units, up to two in flight | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 126162 | 18168 | 7216.5 | unknown | 4326 | 5789 |
| D: pipeline v4, planned units, up to two in flight | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 32694 | 4899 | 6265.5 | unknown | 4571 | 4910 |

## Families (development set)

| Family | Arm | Correct | Calls | Input tokens | Output tokens | Interventions |
|---|---|---|---|---|---|---|
| CohesiveMultiFile | A | 4/4 | 24 | 66909 | 3286 | 0 |
| CohesiveMultiFile | B | 4/4 | 24 | 29471 | 3137 | 0 |
| CohesiveMultiFile | C | 4/4 | 28 | 31567 | 4358 | 0 |
| CohesiveMultiFile | D | 4/4 | 28 | 31567 | 4358 | 0 |
| FailureRecovery | A | 4/4 | 28 | 71490 | 3768 | 0 |
| FailureRecovery | B | 4/4 | 28 | 31609 | 3465 | 0 |
| FailureRecovery | C | 4/4 | 31 | 31735 | 4422 | 0 |
| FailureRecovery | D | 4/4 | 31 | 31735 | 4422 | 0 |
| SeparableMultiTask | A | 4/4 | 24 | 71237 | 4619 | 0 |
| SeparableMultiTask | B | 4/4 | 24 | 32525 | 4465 | 0 |
| SeparableMultiTask | C | 4/4 | 32 | 37079 | 5876 | 0 |
| SeparableMultiTask | D | 4/4 | 32 | 37079 | 5876 | 0 |
| SimpleMaintenance | A | 4/4 | 24 | 64172 | 3055 | 0 |
| SimpleMaintenance | B | 4/4 | 24 | 27711 | 2910 | 0 |
| SimpleMaintenance | C | 4/4 | 24 | 25781 | 3512 | 0 |
| SimpleMaintenance | D | 4/4 | 24 | 25781 | 3512 | 0 |

## Qualification per recipe

| Arm | Development correct | Held-out correct | Unexpected interventions | Orchestration qualified | Cost/quality qualified |
|---|---|---|---|---|---|
| B | true | true | 0 | true | pending (pilot not run) |
| C | true | true | 0 | true | pending (pilot not run) |
| D | true | true | 0 | true | pending (pilot not run) |

## Observations

| Fixture | Family | Arm | Completed | Correct | Calls | Rejected | Interventions | Input | Output | Prompt chars | Wall ms | Diagnostics |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| cohesive-01-session-expiry | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 16751 | 821 | 66997 | 4984 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 7424 | 781 | 29689 | 4040 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 7948 | 1086 | 31779 | 4081 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 7948 | 1086 | 31779 | 4080 | — |
| cohesive-02-money-format | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 16730 | 830 | 66915 | 3513 | — |
| cohesive-02-money-format | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 7367 | 791 | 29455 | 3450 | — |
| cohesive-02-money-format | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 7893 | 1097 | 31562 | 3791 | — |
| cohesive-02-money-format | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 7893 | 1097 | 31562 | 4152 | — |
| cohesive-03-slug-links | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 16705 | 817 | 66812 | 3975 | — |
| cohesive-03-slug-links | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 7278 | 786 | 29099 | 3740 | — |
| cohesive-03-slug-links | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 7798 | 1090 | 31179 | 4170 | — |
| cohesive-03-slug-links | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 7798 | 1090 | 31179 | 3873 | — |
| cohesive-04-rate-window | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 16723 | 818 | 66884 | 3511 | — |
| cohesive-04-rate-window | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 7402 | 779 | 29601 | 4131 | — |
| cohesive-04-rate-window | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 7928 | 1085 | 31702 | 4029 | — |
| cohesive-04-rate-window | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 7928 | 1085 | 31702 | 4556 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 16730 | 814 | 66907 | 3698 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 7348 | 780 | 29377 | 4329 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 7860 | 1086 | 31425 | 4986 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 7860 | 1086 | 31425 | 3414 | — |
| recovery-01-transport-on-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 16000 | 759 | 74271 | 6989 | — |
| recovery-01-transport-on-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 6977 | 725 | 33530 | 3924 | — |
| recovery-01-transport-on-code | FailureRecovery | C | true | true | 7 | 1 | 0 | 6487 | 875 | 29881 | 3256 | — |
| recovery-01-transport-on-code | FailureRecovery | D | true | true | 7 | 1 | 0 | 6487 | 875 | 29881 | 4345 | — |
| recovery-02-malformed-intake | FailureRecovery | A | true | true | 7 | 0 | 0 | 18291 | 873 | 73149 | 6600 | — |
| recovery-02-malformed-intake | FailureRecovery | B | true | true | 7 | 1 | 0 | 7694 | 784 | 30766 | 4098 | — |
| recovery-02-malformed-intake | FailureRecovery | C | true | true | 7 | 1 | 0 | 7203 | 935 | 28803 | 3379 | — |
| recovery-02-malformed-intake | FailureRecovery | D | true | true | 7 | 1 | 0 | 7203 | 935 | 28803 | 3454 | — |
| recovery-03-transport-on-test | FailureRecovery | A | true | true | 7 | 0 | 0 | 17709 | 1152 | 83554 | 6430 | — |
| recovery-03-transport-on-test | FailureRecovery | B | true | true | 7 | 1 | 0 | 8156 | 1114 | 38333 | 4422 | — |
| recovery-03-transport-on-test | FailureRecovery | C | true | true | 9 | 1 | 0 | 9290 | 1466 | 40751 | 4199 | — |
| recovery-03-transport-on-test | FailureRecovery | D | true | true | 9 | 1 | 0 | 9290 | 1466 | 40751 | 5283 | — |
| recovery-04-malformed-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 19490 | 984 | 77949 | 6093 | — |
| recovery-04-malformed-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 8782 | 842 | 35112 | 3501 | — |
| recovery-04-malformed-code | FailureRecovery | C | true | true | 8 | 1 | 0 | 8755 | 1146 | 35005 | 4757 | — |
| recovery-04-malformed-code | FailureRecovery | D | true | true | 8 | 1 | 0 | 8755 | 1146 | 35005 | 3608 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | A | true | true | 7 | 0 | 0 | 17695 | 1158 | 81860 | 6889 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | B | true | true | 7 | 1 | 0 | 8173 | 1121 | 39364 | 4856 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | C | true | true | 9 | 1 | 0 | 9303 | 1475 | 41239 | 4386 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | D | true | true | 9 | 1 | 0 | 9303 | 1475 | 41239 | 3760 | — |
| separable-01-expiry-and-session | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 17871 | 1156 | 71470 | 3607 | — |
| separable-01-expiry-and-session | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 8231 | 1109 | 32919 | 3368 | — |
| separable-01-expiry-and-session | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 9379 | 1462 | 37499 | 4666 | — |
| separable-01-expiry-and-session | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 9379 | 1462 | 37499 | 4416 | — |
| separable-02-retry-and-user | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 17823 | 1153 | 71283 | 4232 | — |
| separable-02-retry-and-user | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 8131 | 1118 | 32513 | 4861 | — |
| separable-02-retry-and-user | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 9260 | 1470 | 37025 | 4892 | — |
| separable-02-retry-and-user | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 9260 | 1470 | 37025 | 4786 | — |
| separable-03-port-and-slug | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 17723 | 1152 | 70882 | 3512 | — |
| separable-03-port-and-slug | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 8055 | 1117 | 32210 | 4580 | — |
| separable-03-port-and-slug | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 9194 | 1469 | 36762 | 4458 | — |
| separable-03-port-and-slug | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 9194 | 1469 | 36762 | 5789 | — |
| separable-04-window-and-money | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 17820 | 1158 | 71271 | 4855 | — |
| separable-04-window-and-money | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 8108 | 1121 | 32422 | 3093 | — |
| separable-04-window-and-money | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 9246 | 1475 | 36972 | 4499 | — |
| separable-04-window-and-money | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 9246 | 1475 | 36972 | 4527 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 17741 | 1147 | 70953 | 4046 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 8008 | 1107 | 32022 | 3704 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 9151 | 1463 | 36594 | 4279 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 9151 | 1463 | 36594 | 4910 | — |
| simple-01-token-expiry | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16018 | 763 | 64061 | 4052 | — |
| simple-01-token-expiry | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 6928 | 725 | 27703 | 3693 | — |
| simple-01-token-expiry | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 6448 | 875 | 25782 | 4210 | — |
| simple-01-token-expiry | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 6448 | 875 | 25782 | 3708 | — |
| simple-02-retry-limit | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16118 | 768 | 64461 | 3632 | — |
| simple-02-retry-limit | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 7004 | 731 | 28005 | 4827 | — |
| simple-02-retry-limit | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 6513 | 882 | 26046 | 3702 | — |
| simple-02-retry-limit | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 6513 | 882 | 26046 | 3640 | — |
| simple-03-trim-username | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 15952 | 759 | 63800 | 4004 | — |
| simple-03-trim-username | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 6862 | 728 | 27438 | 3858 | — |
| simple-03-trim-username | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 6380 | 878 | 25511 | 4809 | — |
| simple-03-trim-username | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 6380 | 878 | 25511 | 3072 | — |
| simple-04-port-parse | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16084 | 765 | 64326 | 3475 | — |
| simple-04-port-parse | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 6917 | 726 | 27659 | 3537 | — |
| simple-04-port-parse | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 6440 | 877 | 25754 | 4148 | — |
| simple-04-port-parse | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 6440 | 877 | 25754 | 4326 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 15996 | 755 | 63972 | 4236 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 6862 | 723 | 27436 | 3618 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 6380 | 875 | 25515 | 3933 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 6380 | 875 | 25515 | 4571 | — |

## Limitations

- Fixture mode never calls a model: semantic quality is not measured and tokens are simulated from prompt/reply length; only relative prompt size between arms is meaningful.
- Pilot mode reports the provider's own usage; a run whose usage is partial or unavailable yields no monetary conclusion.
- Wall time on the loopback provider is serialized by the single-slot connection: arm D is not expected to be faster here.
- Oracles are content predicates on published files, independent of the tests the workers authored.
