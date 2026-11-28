# Sky Layer 2 baseline — 2026-11-27

The frozen parcel clearinghouse contains 10,694 lines of source and tests across six languages, 288 facts in 96 notes and 84 questions across 12 features. Each of the seven retrieval categories contributes 12 questions. The same fact text appears in both vault formats. Candidate relation labels stay in the reference data. Their destinations appear as ordinary links in both vaults until the Observatory format admits additional Line types.

## Retrieval

| Metric | Native query | Question query | Sky |
| --- | --- | --- | --- |
| Recall@5 | 38.89% | 38.89% | Pending 4d / 5 |
| Recall@10 | 50.83% | 47.22% | Pending 4d / 5 |
| Recall@all | 83.06% | 83.24% | Pending 4d / 5 |
| Context precision | 2.00% | 2.29% | Pending 4d / 5 |
| MRR | 0.404 | 0.402 | Pending 4d / 5 |
| nDCG@10 | 0.354 | 0.342 | Pending 4d / 5 |
| Tokens | 685.086 | 632.424 | Pending 4d / 5 |
| Tokens per retrieved relevant fact | 618.589 | 571.680 | Pending 4d / 5 |
| File hit | 87.78% | 88.33% | Pending 4d / 5 |
| Line hit | 0.00% | 0.00% | Pending 4d / 5 |
| Correct refusal | 0.00% | 0.00% | Pending 4d / 5 |
| Tokens on unanswered questions | 683.217 | 625.767 | Pending 4d / 5 |
| Unanswered similarity (mean of maxima / maximum) | 0.666 / 0.717 | 0.555 / 0.589 | Pending 4d / 5 |
| Retrieval p50 / p95 (ms) | 1,828.000 / 5,741.000 | 1,821.000 / 5,788.000 | Pending 4d / 5 |
| Peak working set (paired process) | 53.34 MiB | 53.34 MiB | Pending 4d / 5 |

Each feature is the graph seed in both modes. A real AppCore supervisor populates SQLite with the pinned Ollama embeddings. The ordinary stage worker delivers its prompt to a deterministic HTTP loopback. The benchmark-only query override changes the embedding text for the question mode. Both modes enable the legacy YAML graph RAG and retain the default shadow adaptive policy, file budgets and similarity settings. The provider returns a fixed stage response. The worker reported yaml_graph (591), yaml_graph_rag (249). Native retrieval delivered a vector rerank on 123/420 samples and question retrieval on 126/420. On the remaining samples it returned the graph-only strategy.

The reported rank is the first literal occurrence of each distinct fact in delivered order, including its offset within a multi-fact note. Recall, reciprocal rank and binary nDCG are averaged over answerable samples. Context precision counts relevant fact characters and the visible code excerpts spanning the expected fingerprint line, divided by delivered content characters. Tokens round up after dividing characters by four, following the S3 convention. Tokens per hit excludes zero-hit samples. A file hit counts a matching path in the visible source-files or test-files frontmatter or a delivered snippet. A line hit requires a visible snippet spanning the fingerprint. In shadow mode the worker omits the exact-code attachment. A replay verified the character counts and SHA-256 of all delivered items before scoring file references on the original 840 timed samples. Refusal uses the twelve unanswered questions and measures empty deliveries, their token count and the greatest delivered note similarity. A similarity was available on 11 of 60 native unanswered deliveries and 12 of 60 question-query deliveries. The table shows the mean of the observed per-delivery maxima and their overall maximum. Missing similarity is null. Each sample also records the ordered item paths, content digests, sizes and visible snippets.

The same question runs once per mode for warmup and five more times per mode. The leading mode alternates with question and repetition. Latency comes from the worker retrieval trace, before generation. Percentiles use nearest rank. Parts of the retrieval run overlapped local Cargo validation. Windows recorded two standby/resume pairs during the overnight observation window. Scheduling delays may affect the timing and the strategy selected at the retrieval deadline. PeakWorkingSet64 covers the child across setup, both modes and teardown. Memory cannot be attributed separately to either paired mode. The maximum target-token Jaccard is below 0.2 in 56 of 84 questions, excluding unanswered questions from the numerator. No target shares a contiguous five-token sequence with its question.

## Legacy scale

| N | Indexed | Index (s) | SQLite (MiB) | Native p50 / p95 (ms) | Question p50 / p95 (ms) | Peak (MiB) |
| --- | --- | --- | --- | --- | --- | --- |
| 100 | 100 | 366.62 | 3.61 | 3,212.0 / 5,086.0 | 4,064.0 / 6,239.0 | 46.34 |
| 1000 | 1000 | 1,317.25 | 13.89 | 15,865.0 / 19,012.0 | 16,016.0 / 18,457.0 | 54.36 |
| 10000 | 10000 | 7,057.13 | 117.11 | 276,502.0 / 346,661.0 | 276,329.0 / 292,011.0 | 143.94 |

Each fresh synthetic checkout contains N feature notes with a repeating 4/6/8 Star density and seeded links. At N = 100, 1,000 and 10,000 the corpora contain 598, 5,998 and 59,998 Stars and 327, 3,171 and 30,870 directed Lines. They include one 30-neighbor cluster, three 60-neighbor clusters and ten 90-neighbor clusters, respectively. Three fixed district questions probe the beginning, midpoint and end of each corpus. Index time includes workspace opening, project setup and the wait for the expected embedding count. Database size is SQLite page count times page size. All three runs reached their target embedding counts. Each scale runs in its own process. An interrupted 10,000-note attempt entered Windows Modern Standby twice. With only 92 notes indexed, its legacy embedding job timed out. We discarded the partial retrieval and repeated the workload on AC with a temporary idle-sleep request. The Balanced power plan remained active and Windows logged zero standby or resume events during the final scale run. The raw scale result reports any incomplete index or failed requests separately from successful retrieval latency.

## Pulsations and mutations

| Feature | Completed | Turns | Input tokens | Output tokens | Elapsed (ms) |
| --- | --- | --- | --- | --- | --- |
| intake | True | 6 | 19796 | 513 | 18,568.1 |
| weighing | True | 6 | 20402 | 515 | 18,637.8 |
| sorting | True | 6 | 20953 | 513 | 17,537.6 |
| packing | True | 6 | 21431 | 513 | 17,803.8 |
| dispatch | True | 6 | 21969 | 515 | 18,823.5 |
| tracking | True | 6 | 22510 | 515 | 17,638.9 |
| customs | True | 6 | 22871 | 513 | 18,229.0 |
| delivery | True | 6 | 23595 | 515 | 18,385.3 |
| returns | True | 6 | 24271 | 513 | 18,417.8 |
| claims | True | 6 | 24660 | 513 | 19,906.9 |
| billing | True | 6 | 25186 | 513 | 17,898.5 |
| remittance | True | 6 | 25737 | 518 | 17,639.1 |

| Stage | Turns | Input tokens | Output tokens | Median job duration (ms) |
| --- | --- | --- | --- | --- |
| Intake | 12 | 33985 | 384 | 2,138.0 |
| SPECIFICATION | 12 | 70211 | 1069 | 2,383.5 |
| Contract | 12 | 36345 | 996 | 2,201.5 |
| Code | 12 | 42098 | 1218 | 3,176.0 |
| Test | 12 | 46422 | 1530 | 3,243.0 |
| Integrator | 12 | 44320 | 972 | 2,269.0 |

The twelve tasks request boundary documentation for the twelve parcel operations. The harness routes host-managed stages through its deterministic API, runs the Code and Test roles through the cli_fixture bridge, and approves artifact-review checkpoints using the host facade. Stage duration is the stored job finish time minus its start time. End-to-end elapsed time also includes queues, retries and checkpoint decisions. The raw file preserves each job duration and the input and output counts for each synthetic turn. The latest headless Claude task reached Contract/stage_failure in 142.05 s. The corrected attempt recorded 5 terminal invocations. Intake and Specification each completed once. The Contract role ran three times. All five CLI processes exited successfully and reported claude-sonnet-5-5. The host opened a stage-failure checkpoint. Neither attempt completed the entire pulsation.

The legacy file still contained the target in 48 of 60 mutations. The frozen mutation oracle expects 36 verified and 24 stale results in slice 4b. None of the four requested edit families removes just the fingerprint while preserving its original symbol, the prerequisite for a positive CO-R14 repair suggestion. Legacy references supply a file path only.

## E7 on Ollama

The measured model is nomic-embed-text:137m-v1.5-fp16. Its local manifest SHA-256 is 0a109f422b47e3a30ba2b10eca18548e944e8a23073ee3f3e947efcf3c45e59f. Individual and batch calls used identical UTF-8 input with the Nomic search_query or search_document prefix. Every response had 768 finite values.

| Corpus | Inputs | Batch | Minimum | p1 | Median | ≥ 0.9999 |
| --- | --- | --- | --- | --- | --- | --- |
| questions | 84 | 15 | 0.9999999999999998 | 0.9999999999999998 | 1 | True |
| questions | 84 | 32 | 0.9999999999999998 | 0.9999999999999998 | 1 | True |
| stars | 2000 | 15 | 0.9999999999999998 | 0.9999999999999998 | 1 | True |
| stars | 2000 | 32 | 0.9999999999999998 | 0.9999999999999998 | 1 | True |

| Workload | Median (ms) | Timed repeats |
| --- | --- | --- |
| batch_15 | 589.061 | 5 |
| cold | 600.382 | 5 |
| parallel_15 | 614.607 | 5 |
| reindex_16 | 4,980.511 | 5 |
| reindex_32 | 4,650.400 | 5 |
| warm | 43.462 | 5 |

The 15-query alternatives process identical inputs. Reindex timing covers the same 96 documents at widths 16 and 32. Cold measurements follow an explicit unload. Warm calls repeat the same query on the resident model.

## S15 database pre-measurement

| Stars | Journal | Reads | Read p50 / p95 / p99 (ms) | Writer p95 (ms) | Lock wait (ms) | Busy events | Timeouts |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 5000 | DELETE | 19898 | 0.205 / 0.666 / 14.811 | 133.356 | 52.704 | 37 | 0 |
| 5000 | WAL | 24052 | 0.082 / 0.274 / 0.510 | 167.220 | 0.000 | 0 | 0 |
| 50000 | WAL | 19444 | 0.098 / 2.709 / 3.461 | 177.354 | 0.000 | 0 | 0 |
| 50000 | DELETE | 14915 | 0.309 / 2.725 / 24.429 | 311.792 | 62.024 | 42 | 0 |

The preliminary decision is **DELETE** under the 20% improvement rule. Each size compares both journals on local Windows storage with 15 readers, FTS5 MATCH, primary-key lookups and 768-component f32 BLOB reads. The benchmark-only index schema lives in its own temporary sky/parcel.sqlite. Readers compete with a single writer for 256-row batches and 2,000-row generation publications. Read timing includes SQLite retries. The writer schedules twelve 256-row batches at 200 ms intervals and publishes three additional 2,000-row generations. WAL removed observed lock waits at both sizes. At 5,000 Stars its writer p95 rose from 133.36 to 167.22 ms, which disqualifies WAL under the no-regression condition. Busy events count first entries into the retry handler. Timeouts count exhausted retry budgets. The final journal decision belongs to the full 15-bridge S15 in slice 4e.

## Frozen inputs and replay

Legacy source e92603d. Fixture freeze 7b599ef. Manifest SHA-256 **db3869219dc61264a55f2e39ccc1c88615dd4b9d2668fda2e27d80ad723a7230**. A manifest change requires a fresh baseline before comparison in Layer 5. Public raw observations and machine details accompany this report. The last measurement finished at 2026-11-28T03:16:39-03:00.

Two empty destinations independently produced 660 byte-identical files and the same Git HEAD after 37 commits. The materialized native scanner passed its twelve C tests. All five required PR gates and the complete workspace test suite passed. The generator test command took 14.7 seconds including Cargo startup and build work.

Run the following from the repository root in PowerShell.

```powershell
cargo build --locked -p ikore-test-support --features sky-baseline --example sky_baseline --bin cli_fixture
.\scripts\sky-baseline.ps1 -Phase retrieval -OutputDirectory target/sky-layer2
```

Repeat the wrapper for each phase. Supported measurements are retrieval, scale-100, scale-1000, scale-10000, mutations, pulsations, e7 and sqlite. The optional live phase uses an already authenticated local Claude CLI for one task. The example's generate command exports the materialized repository and both vaults into an empty directory. The generate-scale command accepts a single object from fixtures/sky-bench/scale.yaml, encoded as YAML or JSON, followed by an empty destination directory.

### Headless CLI runbook

The first trial parked at an Intake fallback before a canonical terminal slot existed in the headless project. The follow-up uses the normal project terminal-slot initializer. The raw file distinguishes both attempts. Reproduce it on the benchmark host from the repository root with the pinned model available in Ollama. Check the existing session with `claude auth status --json`. With an already authenticated session, build the executable as above and run `.\scripts\sky-baseline.ps1 -Phase live -OutputDirectory target/sky-live-replay`. The runner creates an isolated mock and submits the first frozen task. Preserve both the endpoint and the child memory file. Stop at the recorded fallback if the host cannot execute the selected profile. A total of three live tasks is the Layer 2 cap.

Coordinate relocation, wrong-location and stale rates await slice 4b. Sky retrieval, ablations and scale await 4d and the comparative rerun in Layer 5. Full bridge concurrency and transport timing await 4e. The real-repository comparison and candidate Line and V4/V5 decisions await Layer 5.
