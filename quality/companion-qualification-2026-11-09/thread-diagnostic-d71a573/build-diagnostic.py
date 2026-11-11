"""Derive a diagnostic script without changing its authoritative original."""
from pathlib import Path
import hashlib
import json
import difflib

OUT = Path(__file__).resolve().parent
ROOT = OUT.parents[2]
SOURCE = ROOT / "scripts/benchmark-ui-latency.ps1"
raw = SOURCE.read_bytes()
base = raw.decode("utf-8-sig").replace("\r\n", "\n")
derived = base.replace("(Join-Path $PSScriptRoot '..')", "(Join-Path $PSScriptRoot '..\\..\\..')", 1)
derived = derived.replace("Join-Path $repositoryRoot 'target\\stage6\\ui-latency.json'", "Join-Path $PSScriptRoot 'diagnostic-latency.json'", 1)
derived = derived.replace("$stageRoot = Join-Path $repositoryRoot 'target\\stage6'", "$stageRoot = $PSScriptRoot", 1)
derived = derived.replace("$runtimeRoot = Join-Path $stageRoot 'ui-latency-runtime'", "$runtimeRoot = Join-Path $stageRoot 'isolated-runtime'", 1)
derived = derived.replace("$process = $null\n", """$process = $null
$threadSampler = $null
$threadSnapshotWritten = $false
$threadOutput = Join-Path $PSScriptRoot 'thread-cpu.json'
if ($null -eq ('HvcThreadDiagnostic.Sampler' -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot 'ThreadSampler.cs')
}
""", 1)
launch = "$process = Start-Process -FilePath $resolvedBinary -PassThru -WindowStyle Normal"
assert derived.count(launch) == 1
derived = derived.replace(launch, launch + "\n    $threadSampler = [HvcThreadDiagnostic.Sampler]::new($process.Id)", 1)
anchor = "    $displayRefreshRates = try {"
assert derived.count(anchor) == 1
derived = derived.replace(anchor, """    # Diagnostic only: stop before CIM and graceful shutdown; this does not qualify the gate.
    $threadSampler.StopAndSnapshot() | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $threadOutput -Encoding utf8
    $threadSnapshotWritten = $true

""" + anchor, 1)
derived = derived.replace("finally {\n    if ($null -ne $process", """finally {
    try {
        if ($null -ne $threadSampler -and -not $threadSnapshotWritten) {
            $threadSampler.StopAndSnapshot() | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $threadOutput -Encoding utf8
        }
    }
    catch {
        Write-Warning 'Diagnostic snapshot failed; the original process/environment cleanup continues.'
    }
    if ($null -ne $process""", 1)
assert "HVC_BENCHMARK_UI_SAMPLES = $Samples" in derived
(OUT / "benchmark-ui-latency.original.ps1").write_bytes(raw)
(OUT / "benchmark-ui-latency.diagnostic.ps1").write_text(derived, encoding="utf-8", newline="\n")
(OUT / "diagnostic-script.patch").write_text("".join(difflib.unified_diff(base.splitlines(True), derived.splitlines(True), fromfile="original", tofile="diagnostic")), encoding="utf-8", newline="\n")
metadata = {"purpose": "diagnostic only; does not replace the 2531a81 preflight FAIL", "original": str(SOURCE.relative_to(ROOT)), "originalSha256": hashlib.sha256(raw).hexdigest(), "sourceUnchanged": SOURCE.read_bytes() == raw,
 "sourceCommit": "2531a817e1d564ccf293e87777f27dfcac683393", "requiredInvocation": {"Scenario":"dock-three-streams", "Samples":140,"WarmupSamples":20,"WindowSize":"1440x900","TimeoutSeconds":30,"EnforceThresholds":False},
 "changes": ["root rebased for target-only derivative", "isolated runtime under diagnostic target directory", "bounded exact-PID CPU thread sampler", "no changes to fixture, readiness, cadence, limits or threshold evaluation"]}
(OUT / "derivation.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8", newline="\n")
print(json.dumps(metadata, indent=2))
