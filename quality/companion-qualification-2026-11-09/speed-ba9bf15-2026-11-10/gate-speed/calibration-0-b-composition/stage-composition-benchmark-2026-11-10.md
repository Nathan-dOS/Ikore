# Stage composition benchmark

- generated_at: 2026-11-10T10:12:28.823166500+00:00
- mode: fixture
- code_base: 3a2c973a5bbe58e3d6e28cc88739c5f86e1ae9de
- fixture_version: stage-composition-fixtures-v1
- fixtures_sha256: 7c931da80b5cb21dc310b01953f288ee68361f192cbc917718866735a34526e2
- host: windows x86_64

Fixture mode: every worker reply is scripted from the prompt the host rendered; tokens are **simulated** (characters over four) and there is **no monetary conclusion**. What this mode proves is orchestration: calls, retries, checkpoints, publication and the oracle on the published files.

## Arms

| Arm | Set | Fixtures | Completed | Correct | Tasks correct | Calls | Rejected | Interventions | Input tokens | Output tokens | Tokens / correct task | Spend / correct task (µUSD) | Wall p50 ms | Wall p95 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A: pipeline v3 (legacy, one call per stage) | development | 1 | 1 | 1 | 1/1 | 6 | 0 | 0 | 17653 | 821 | 18474.0 | unknown | 4906 | 4906 |
| B: pipeline v4, one unit per stage | development | 1 | 1 | 1 | 1/1 | 6 | 0 | 0 | 11147 | 781 | 11928.0 | unknown | 4751 | 4751 |
| C: pipeline v4, planned units, sequential | development | 1 | 1 | 1 | 1/1 | 7 | 0 | 0 | 13185 | 1086 | 14271.0 | unknown | 6044 | 6044 |
| D: pipeline v4, planned units, up to two in flight | development | 1 | 1 | 1 | 1/1 | 7 | 0 | 0 | 13185 | 1086 | 14271.0 | unknown | 7372 | 7372 |

## Families (development set)

| Family | Arm | Correct | Calls | Input tokens | Output tokens | Interventions |
|---|---|---|---|---|---|---|
| CohesiveMultiFile | A | 1/1 | 6 | 17653 | 821 | 0 |
| CohesiveMultiFile | B | 1/1 | 6 | 11147 | 781 | 0 |
| CohesiveMultiFile | C | 1/1 | 7 | 13185 | 1086 | 0 |
| CohesiveMultiFile | D | 1/1 | 7 | 13185 | 1086 | 0 |

## Qualification per recipe

| Arm | Development correct | Held-out correct | Unexpected interventions | Orchestration qualified | Cost/quality qualified |
|---|---|---|---|---|---|
| B | true | true | 0 | true | pending (pilot not run) |
| C | true | true | 0 | true | pending (pilot not run) |
| D | true | true | 0 | true | pending (pilot not run) |

## Observations

| Fixture | Family | Arm | Completed | Correct | Calls | Rejected | Interventions | Input | Output | Prompt chars | Wall ms | Diagnostics |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| cohesive-01-session-expiry | CohesiveMultiFile | A | true | true | 6 | 0 | 0 | 17653 | 821 | 70601 | 4906 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | B | true | true | 6 | 0 | 0 | 11147 | 781 | 44580 | 4751 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | C | true | true | 7 | 0 | 0 | 13185 | 1086 | 52733 | 6044 | — |
| cohesive-01-session-expiry | CohesiveMultiFile | D | true | true | 7 | 0 | 0 | 13185 | 1086 | 52733 | 7372 | — |

## Limitations

- Fixture mode never calls a model: semantic quality is not measured and tokens are simulated from prompt/reply length; only relative prompt size between arms is meaningful.
- Pilot mode reports the provider's own usage; a run whose usage is partial or unavailable yields no monetary conclusion.
- Wall time on the loopback provider is serialized by the single-slot connection: arm D is not expected to be faster here.
- Oracles are content predicates on published files, independent of the tests the workers authored.
