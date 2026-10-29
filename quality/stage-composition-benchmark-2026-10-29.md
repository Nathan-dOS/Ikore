# Stage composition benchmark

- generated_at: 2026-10-29T22:24:36.122573800+00:00
- mode: fixture
- code_base: d5509be5965817845b97e126326ea605b5999ed0
- fixture_version: stage-composition-fixtures-v1
- fixtures_sha256: 786e5cc1da8409dc4985da0d27cea9c388d4c42fc82a82121ce1515c1f093c5e
- host: windows x86_64

Fixture mode: every worker reply is scripted from the prompt the host rendered; tokens are **simulated** (characters over four) and there is **no monetary conclusion**. What this mode proves is orchestration: calls, retries, checkpoints, publication and the oracle on the published files.

## Arms

| Arm | Set | Fixtures | Completed | Correct | Tasks correct | Calls | Rejected | Interventions | Input tokens | Output tokens | Tokens / correct task | Spend / correct task (µUSD) | Wall p50 ms | Wall p95 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A: pipeline v3 (legacy, one call per stage) | development | 16 | 16 | 16 | 20/20 | 100 | 0 | 0 | 280365 | 14728 | 14754.6 | unknown | 5254 | 8704 |
| A: pipeline v3 (legacy, one call per stage) | held-out | 4 | 4 | 4 | 6/6 | 25 | 0 | 0 | 69784 | 3874 | 12276.3 | unknown | 4400 | 6847 |
| B: pipeline v4, one unit per stage | development | 16 | 16 | 16 | 20/20 | 100 | 4 | 0 | 151526 | 13977 | 8275.1 | unknown | 4461 | 5452 |
| B: pipeline v4, one unit per stage | held-out | 4 | 4 | 4 | 6/6 | 25 | 1 | 0 | 37600 | 3731 | 6888.5 | unknown | 5279 | 6667 |
| C: pipeline v4, planned units, sequential | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 176814 | 18168 | 9749.1 | unknown | 5537 | 6734 |
| C: pipeline v4, planned units, sequential | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 46416 | 4899 | 8552.5 | unknown | 5010 | 6190 |
| D: pipeline v4, planned units, up to two in flight | development | 16 | 16 | 16 | 20/20 | 115 | 4 | 0 | 176814 | 18168 | 9749.1 | unknown | 4993 | 6442 |
| D: pipeline v4, planned units, up to two in flight | held-out | 4 | 4 | 4 | 6/6 | 30 | 1 | 0 | 46416 | 4899 | 8552.5 | unknown | 5161 | 5994 |

## Families (development set)

| Family | Arm | Correct | Calls | Input tokens | Output tokens | Interventions |
|---|---|---|---|---|---|---|
| CohesiveMultiFile | A | 4/4 | 24 | 68535 | 3286 | 0 |
| CohesiveMultiFile | B | 4/4 | 24 | 36923 | 3137 | 0 |
| CohesiveMultiFile | C | 4/4 | 28 | 43667 | 4358 | 0 |
| CohesiveMultiFile | D | 4/4 | 28 | 43667 | 4358 | 0 |
| FailureRecovery | A | 4/4 | 28 | 73174 | 3768 | 0 |
| FailureRecovery | B | 4/4 | 28 | 39745 | 3465 | 0 |
| FailureRecovery | C | 4/4 | 31 | 44374 | 4422 | 0 |
| FailureRecovery | D | 4/4 | 31 | 44374 | 4422 | 0 |
| SeparableMultiTask | A | 4/4 | 24 | 72859 | 4619 | 0 |
| SeparableMultiTask | B | 4/4 | 24 | 39829 | 4465 | 0 |
| SeparableMultiTask | C | 4/4 | 32 | 54476 | 5876 | 0 |
| SeparableMultiTask | D | 4/4 | 32 | 54476 | 5876 | 0 |
| SimpleMaintenance | A | 4/4 | 24 | 65797 | 3055 | 0 |
| SimpleMaintenance | B | 4/4 | 24 | 35029 | 2910 | 0 |
| SimpleMaintenance | C | 4/4 | 24 | 34297 | 3512 | 0 |
| SimpleMaintenance | D | 4/4 | 24 | 34297 | 3512 | 0 |

## Qualification per recipe

| Arm | Development correct | Held-out correct | Unexpected interventions | Orchestration qualified | Cost/quality qualified |
|---|---|---|---|---|---|
| B | true | true | 0 | true | pending (pilot not run) |
| C | true | true | 0 | true | pending (pilot not run) |
| D | true | true | 0 | true | pending (pilot not run) |

## Observations

| Fixture | Family | Arm | Completed | Correct | Calls | Rejected | Interventions | Input | Output | Prompt chars | Wall ms | Diagnostics |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| cohesive-01-session-expiry | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17157 | 821 | 68621 | 5363 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 9423 | 781 | 37681 | 4591 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 11109 | 1086 | 44422 | 5782 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 11109 | 1086 | 44422 | 4657 | — |
| cohesive-02-money-format | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17137 | 830 | 68539 | 5145 | — |
| cohesive-02-money-format | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 9183 | 791 | 36727 | 5406 | — |
| cohesive-02-money-format | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 10874 | 1097 | 43485 | 6734 | — |
| cohesive-02-money-format | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 10874 | 1097 | 43485 | 5664 | — |
| cohesive-03-slug-links | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17111 | 817 | 68436 | 4281 | — |
| cohesive-03-slug-links | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 9097 | 786 | 36373 | 4415 | — |
| cohesive-03-slug-links | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 10772 | 1090 | 43077 | 4668 | — |
| cohesive-03-slug-links | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 10772 | 1090 | 43077 | 6442 | — |
| cohesive-04-rate-window | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17130 | 818 | 68508 | 5262 | — |
| cohesive-04-rate-window | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 9220 | 779 | 36872 | 4334 | — |
| cohesive-04-rate-window | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 10912 | 1085 | 43635 | 5537 | — |
| cohesive-04-rate-window | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 10912 | 1085 | 43635 | 4631 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17134 | 814 | 68531 | 4278 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 9161 | 780 | 36634 | 4303 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 10842 | 1086 | 43353 | 4823 | — |
| cohesive-05-checksum-verify (held-out) | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 10842 | 1086 | 43353 | 4678 | — |
| recovery-01-transport-on-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 16407 | 759 | 76111 | 8704 | — |
| recovery-01-transport-on-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 9010 | 725 | 42685 | 4461 | — |
| recovery-01-transport-on-code | FailureRecovery | C | true | true | 7 | 1 | 0 | 8804 | 875 | 40180 | 4644 | — |
| recovery-01-transport-on-code | FailureRecovery | D | true | true | 7 | 1 | 0 | 8804 | 875 | 40180 | 4340 | — |
| recovery-02-malformed-intake | FailureRecovery | A | true | true | 7 | 0 | 0 | 18704 | 873 | 74805 | 6814 | — |
| recovery-02-malformed-intake | FailureRecovery | B | true | true | 7 | 1 | 0 | 9794 | 784 | 39170 | 5441 | — |
| recovery-02-malformed-intake | FailureRecovery | C | true | true | 7 | 1 | 0 | 9607 | 935 | 38420 | 4643 | — |
| recovery-02-malformed-intake | FailureRecovery | D | true | true | 7 | 1 | 0 | 9607 | 935 | 38420 | 4663 | — |
| recovery-03-transport-on-test | FailureRecovery | A | true | true | 7 | 0 | 0 | 18114 | 1152 | 85637 | 6701 | — |
| recovery-03-transport-on-test | FailureRecovery | B | true | true | 7 | 1 | 0 | 9993 | 1114 | 46967 | 4749 | — |
| recovery-03-transport-on-test | FailureRecovery | C | true | true | 9 | 1 | 0 | 13637 | 1466 | 62754 | 5511 | — |
| recovery-03-transport-on-test | FailureRecovery | D | true | true | 9 | 1 | 0 | 13637 | 1466 | 62754 | 5623 | — |
| recovery-04-malformed-code | FailureRecovery | A | true | true | 7 | 0 | 0 | 19949 | 984 | 79789 | 6783 | — |
| recovery-04-malformed-code | FailureRecovery | B | true | true | 7 | 1 | 0 | 10948 | 842 | 43774 | 4723 | — |
| recovery-04-malformed-code | FailureRecovery | C | true | true | 8 | 1 | 0 | 12326 | 1146 | 49292 | 4995 | — |
| recovery-04-malformed-code | FailureRecovery | D | true | true | 8 | 1 | 0 | 12326 | 1146 | 49292 | 6001 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | A | true | true | 7 | 0 | 0 | 18100 | 1158 | 83700 | 6847 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | B | true | true | 7 | 1 | 0 | 10025 | 1121 | 47805 | 6667 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | C | true | true | 9 | 1 | 0 | 13668 | 1475 | 60636 | 6190 | — |
| recovery-05-transport-on-code-pair (held-out) | FailureRecovery | D | true | true | 9 | 1 | 0 | 13668 | 1475 | 60636 | 5161 | — |
| separable-01-expiry-and-session | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18276 | 1156 | 73094 | 3916 | — |
| separable-01-expiry-and-session | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 10193 | 1109 | 40759 | 5452 | — |
| separable-01-expiry-and-session | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 13868 | 1462 | 55454 | 4889 | — |
| separable-01-expiry-and-session | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 13868 | 1462 | 55454 | 4926 | — |
| separable-02-retry-and-user | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18228 | 1153 | 72907 | 4236 | — |
| separable-02-retry-and-user | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 9902 | 1118 | 39602 | 4343 | — |
| separable-02-retry-and-user | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 13552 | 1470 | 54198 | 6100 | — |
| separable-02-retry-and-user | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 13552 | 1470 | 54198 | 4993 | — |
| separable-03-port-and-slug | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18129 | 1152 | 72506 | 5254 | — |
| separable-03-port-and-slug | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 9843 | 1117 | 39363 | 4388 | — |
| separable-03-port-and-slug | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 13498 | 1469 | 53975 | 5092 | — |
| separable-03-port-and-slug | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 13498 | 1469 | 53975 | 5101 | — |
| separable-04-window-and-money | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18226 | 1158 | 72895 | 4128 | — |
| separable-04-window-and-money | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 9891 | 1121 | 39556 | 4401 | — |
| separable-04-window-and-money | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 13558 | 1475 | 54218 | 6004 | — |
| separable-04-window-and-money | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 13558 | 1475 | 54218 | 5801 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | A | true | true | 6 | 0 | 0 | 18147 | 1147 | 72577 | 4400 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | B | true | true | 6 | 0 | 0 | 9781 | 1107 | 39116 | 5279 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | C | true | true | 8 | 0 | 0 | 13452 | 1463 | 53796 | 5010 | — |
| separable-05-percent-and-checksum (held-out) | SeparableMultiTask | D | true | true | 8 | 0 | 0 | 13452 | 1463 | 53796 | 5994 | — |
| simple-01-token-expiry | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16424 | 763 | 65685 | 5427 | — |
| simple-01-token-expiry | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 8901 | 725 | 35598 | 4195 | — |
| simple-01-token-expiry | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 8717 | 875 | 34864 | 6260 | — |
| simple-01-token-expiry | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 8717 | 875 | 34864 | 4156 | — |
| simple-02-retry-limit | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16525 | 768 | 66085 | 3877 | — |
| simple-02-retry-limit | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 8781 | 731 | 35118 | 4261 | — |
| simple-02-retry-limit | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 8595 | 882 | 34370 | 5079 | — |
| simple-02-retry-limit | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 8595 | 882 | 34370 | 4252 | — |
| simple-03-trim-username | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16358 | 759 | 65424 | 5085 | — |
| simple-03-trim-username | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 8636 | 728 | 34540 | 5067 | — |
| simple-03-trim-username | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 8452 | 878 | 33803 | 6227 | — |
| simple-03-trim-username | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 8452 | 878 | 33803 | 4188 | — |
| simple-04-port-parse | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16490 | 765 | 65950 | 4939 | — |
| simple-04-port-parse | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 8711 | 726 | 34829 | 4219 | — |
| simple-04-port-parse | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 8533 | 877 | 34115 | 6248 | — |
| simple-04-port-parse | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 8533 | 877 | 34115 | 6047 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | A | true | true | 6 | 0 | 0 | 16403 | 755 | 65596 | 4292 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | B | true | true | 6 | 0 | 0 | 8633 | 723 | 34518 | 4311 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | C | true | true | 6 | 0 | 0 | 8454 | 875 | 33802 | 4183 | — |
| simple-05-clamp-percent (held-out) | SimpleMaintenance | D | true | true | 6 | 0 | 0 | 8454 | 875 | 33802 | 5027 | — |

## Limitations

- Fixture mode never calls a model: semantic quality is not measured and tokens are simulated from prompt/reply length; only relative prompt size between arms is meaningful.
- Pilot mode reports the provider's own usage; a run whose usage is partial or unavailable yields no monetary conclusion.
- Wall time on the loopback provider is serialized by the single-slot connection: arm D is not expected to be faster here.
- Oracles are content predicates on published files, independent of the tests the workers authored.
