# Exact-PID thread CPU diagnostic — 2531a81

This is a diagnostic of the same release and three-stream workload. It cannot qualify a performance gate or replace the archived first preflight failure (p95 12.3584 ms).

`benchmark-ui-latency.original.ps1` is a byte-exact copy of the official script, with its hash in `derivation.json`. The derivative changes only its target-directory roots and adds `ThreadSampler.cs`. Invocation keeps Samples=140, WarmupSamples=20, Scenario=dock-three-streams, WindowSize=1440x900 and TimeoutSeconds=30, without EnforceThresholds. Threshold calculation remains in the application/report. All fresh profile data and output stay in this directory; the script retains the official graceful-close and runtime cleanup behavior.

The C# Task samples at a requested 25 ms interval with bounded duration (31 s), points (1,600), rows (100,000) and lifetime thread identities (512). It reads only the launched PID. OpenThread ownership is checked before GetThreadTimes or GetThreadDescription; live handles preserve identity across thread exit. Raw thread descriptions are never emitted: only source-known static labels are retained, with other descriptions redacted. No command line, argument, standard output, credential, stack or other process CPU is collected.

Points are ordered and record sampler elapsed time, collection duration, exact-process cumulative user/kernel CPU, visible-window state and HWND-derived UI thread ID. Thread rows contain cumulative user/kernel CPU and identity, presence and exit flags. `pre-window`, `window-visible` and `post-window` are sampled phases, with a boundary uncertainty of about one sample interval. Collection stops when the script observes the completed latency report, before CIM and shutdown. It does not provide exact per-frame timing; the official latency samples are sorted and cannot be aligned to individual points.

CPU timing has Windows accounting granularity. First observations can include CPU accrued before attachment; short-lived threads may disappear before enumeration. Compare captured thread CPU against exact-process counters and inspect errors/saturation. The sampler itself runs in the external PowerShell process and may perturb scheduling, which is another reason these measurements do not qualify release latency.

The official PowerShell script and Rust/Cargo source remain unchanged. C# compilation and PowerShell parsing validate the diagnostic files; they do not run the application.
