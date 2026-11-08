[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BaselineRoot,
    [string]$CandidateRoot = '',
    [string]$OutputDirectory = '',
    [ValidateRange(20,200)][int]$Pairs = 20
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib\speed-metrics.ps1')
if (-not $CandidateRoot) { $CandidateRoot = Join-Path $PSScriptRoot '..' }
$BaselineRoot = (Resolve-Path -LiteralPath $BaselineRoot).Path
$CandidateRoot = (Resolve-Path -LiteralPath $CandidateRoot).Path
if ($BaselineRoot.Equals($CandidateRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Baseline and candidate must be separate checkouts.' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $CandidateRoot ('target\speed\' + [guid]::NewGuid().ToString('N')) }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if ((Test-Path -LiteralPath $OutputDirectory) -and @(Get-ChildItem -LiteralPath $OutputDirectory -Force).Count -ne 0) { throw 'Use a new, empty speed-report directory to prevent stale evidence.' }
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$uiScript = Join-Path $PSScriptRoot 'benchmark-ui-latency.ps1'
$probeName = 'persistence_service::exchange_baseline_tests::exchange_baseline_pulsation_transitions'
$raw = [Collections.Generic.List[object]]::new()
$script:timingFixtureId = $null
$script:canaryReference = $null
$expectedArms = @('a','b','c','d')

function Invoke-SpeedNative([string]$Binary, [string[]]$Arguments, [string]$LogPath) {
    if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { throw "Required speed binary is missing: $Binary" }
    # Windows PowerShell 5 wraps native stderr as ErrorRecord even on exit 0.
    # Capture legitimate fixture progress, then check exit status and JSON.
    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = @(& $Binary @Arguments 2>&1)
        $exitCode = $LASTEXITCODE
    } finally { $ErrorActionPreference = $previousPreference }
    $output | ForEach-Object { $_.ToString() } | Set-Content -LiteralPath $LogPath -Encoding utf8
    if ($exitCode -ne 0) { throw "Speed command failed with exit code ${exitCode}: $Binary (see $LogPath)" }
    return $output
}

function Get-CoreBinary([string]$Root, [string]$Label) {
    $binaries = @(Get-ChildItem -LiteralPath (Join-Path $Root 'target\debug\deps') -Filter 'hvc_core-*.exe' | Sort-Object LastWriteTime -Descending)
    foreach ($binary in $binaries) {
        $inventory = @(Invoke-SpeedNative $binary.FullName @('--list') (Join-Path $OutputDirectory "$Label-$($binary.BaseName)-inventory.log"))
        if (@($inventory | Where-Object { $_.ToString() -eq "${probeName}: test" }).Count -eq 1) { return $binary.FullName }
    }
    throw "Build core library tests first; no binary exposes the exact transition probe: $Root"
}

function Get-SpeedArtifacts([string]$Root, [string]$CoreBinary) {
    $paths = [ordered]@{ core=$CoreBinary; ui=(Join-Path $Root 'target\release\hard-vibe-code.exe'); composition=(Join-Path $Root 'target\debug\examples\composition_benchmark.exe') }
    $artifacts = [ordered]@{}
    foreach ($name in $paths.Keys) {
        if (-not (Test-Path -LiteralPath $paths[$name] -PathType Leaf)) { throw "Build the $name speed artifact first: $($paths[$name])" }
        $artifacts[$name] = [pscustomobject]@{ path=$paths[$name]; sha256=(Get-FileHash -LiteralPath $paths[$name] -Algorithm SHA256).Hash; modifiedAt=[IO.File]::GetLastWriteTimeUtc($paths[$name]).ToString('o') }
    }
    return $artifacts
}

function Invoke-CompositionReport([string]$Root, [string]$Label, [string]$FixtureId = '') {
    $directory = Join-Path $OutputDirectory "$Label-composition"
    $binary = Join-Path $Root 'target\debug\examples\composition_benchmark.exe'
    $arguments = @('--mode','fixture','--held-out','--output-dir',$directory)
    if ($FixtureId) { $arguments += @('--fixture',$FixtureId) }
    Push-Location $Root
    try { Invoke-SpeedNative $binary $arguments (Join-Path $OutputDirectory "$Label-composition.log") | Out-Null }
    finally { Pop-Location }
    $files = @(Get-ChildItem -LiteralPath $directory -Filter '*.json')
    if ($files.Count -ne 1) { throw "Composition report is missing or ambiguous: $Label" }
    return Get-Content -LiteralPath $files[0].FullName -Raw | ConvertFrom-Json
}

function Invoke-SpeedSample([string]$Root, [string]$CoreBinary, [string]$Label, [switch]$AllowLegacyUi) {
    Push-Location $Root
    try { $lines = @(Invoke-SpeedNative $CoreBinary @($probeName,'--exact','--ignored','--nocapture','--test-threads=1') (Join-Path $OutputDirectory "$Label-transition.log")) }
    finally { Pop-Location }
    $transition = Get-SpeedTransitionFromOutput $lines
    $canary = Invoke-CompositionReport $Root $Label $script:timingFixtureId
    Get-SpeedCompositionIndex $canary $expectedArms | Out-Null
    if (@($canary.meta.fixtures).Count -ne 1 -or $canary.meta.fixtures[0].id -ne $script:timingFixtureId) { throw 'The timing filter did not select exactly the frozen fixture.' }
    if ($null -eq $script:canaryReference) { $script:canaryReference = $canary }
    else { Assert-SpeedCompositionParity $script:canaryReference $canary $expectedArms | Out-Null }
    $wall = @($canary.observations | ForEach-Object { ConvertTo-SpeedNumber $_.wall_ms })
    $uiPath = Join-Path $OutputDirectory "$Label-ui.json"
    & $uiScript -BinaryPath (Join-Path $Root 'target\release\hard-vibe-code.exe') -OutputPath $uiPath -Scenario 'dock-closed' -WindowSize '1280x820' |
        Out-File -LiteralPath (Join-Path $OutputDirectory "$Label-ui.log") -Encoding utf8
    $ui = Get-SpeedUiSample (Get-Content -LiteralPath $uiPath -Raw | ConvertFrom-Json) 'dock-closed' -AllowLegacyIdle:$AllowLegacyUi -ExpectedWidth 1280 -ExpectedHeight 820
    $sample = [pscustomobject]@{
        label=$Label; nonTerminalP50Us=$transition.nonTerminalP50Us; nonTerminalP95Us=$transition.nonTerminalP95Us
        terminalP50Us=$transition.terminalP50Us; terminalP95Us=$transition.terminalP95Us
        compositionWallP50Ms=(Get-SpeedPercentile $wall 0.5); compositionWallP95Ms=(Get-SpeedPercentile $wall 0.95)
        uiP50Ms=$ui.uiP50Ms; uiP95Ms=$ui.uiP95Ms; uiP99Ms=$ui.uiP99Ms
    }
    $raw.Add($sample)
    return $sample
}

$report = [ordered]@{
    schemaVersion=1; startedAt=[DateTime]::UtcNow.ToString('o'); passed=$false
    pairs=$Pairs; baselineRoot=$BaselineRoot; candidateRoot=$CandidateRoot
    scope='Offline orchestration canary across four arms, host transitions and GPUI callback intervals; not real-model quality or compositor presentation'
    policy='20 or more A/A calibration pairs freeze p95 absolute noise before alternating A/B and B/A pairs; median paired delta for each p50/p95 statistic must not exceed that tolerance'
    sampleOrder='transition probe, fixed composition canary across a/b/c/d, closed-dock UI'
    timingAggregation='composition p50/p95 are nearest-rank quantiles of the four per-arm wall_ms observations in one sample'
}
try {
    $baselineCore = Get-CoreBinary $BaselineRoot 'baseline'
    $candidateCore = Get-CoreBinary $CandidateRoot 'candidate'
    $report.baselineArtifacts = Get-SpeedArtifacts $BaselineRoot $baselineCore
    $report.candidateArtifacts = Get-SpeedArtifacts $CandidateRoot $candidateCore
    $before = Invoke-CompositionReport $BaselineRoot 'baseline-full'
    Get-SpeedCompositionIndex $before $expectedArms -RequireHeldOut | Out-Null
    $script:timingFixtureId = @($before.meta.fixtures | Sort-Object id)[0].id
    $report.timingFixture = [ordered]@{id=$script:timingFixtureId; fixtureVersion=$before.meta.fixture_version; fullFixtureSha256=$before.meta.fixtures_sha256; arms=$expectedArms}
    Invoke-SpeedSample $BaselineRoot $baselineCore 'warmup-baseline' -AllowLegacyUi | Out-Null
    Invoke-SpeedSample $CandidateRoot $candidateCore 'warmup-candidate' | Out-Null
    $report.timingFixture.canaryFixtureSha256 = $script:canaryReference.meta.fixtures_sha256
    $calibrationA = @(); $calibrationB = @()
    for ($index=0; $index -lt $Pairs; $index++) {
        $calibrationA += Invoke-SpeedSample $BaselineRoot $baselineCore "calibration-$index-a" -AllowLegacyUi
        $calibrationB += Invoke-SpeedSample $BaselineRoot $baselineCore "calibration-$index-b" -AllowLegacyUi
    }
    @{a=$calibrationA; b=$calibrationB; frozenAt=[DateTime]::UtcNow.ToString('o'); timingFixture=$report.timingFixture} | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath (Join-Path $OutputDirectory 'calibration.json') -Encoding utf8
    $baseline = @(); $candidate = @()
    for ($index=0; $index -lt $Pairs; $index++) {
        if ($index % 2 -eq 0) {
            $baseline += Invoke-SpeedSample $BaselineRoot $baselineCore "comparison-$index-baseline" -AllowLegacyUi
            $candidate += Invoke-SpeedSample $CandidateRoot $candidateCore "comparison-$index-candidate"
        } else {
            $candidate += Invoke-SpeedSample $CandidateRoot $candidateCore "comparison-$index-candidate"
            $baseline += Invoke-SpeedSample $BaselineRoot $baselineCore "comparison-$index-baseline" -AllowLegacyUi
        }
    }
    $metrics = [ordered]@{}
    foreach ($metric in @('nonTerminalP50Us','nonTerminalP95Us','terminalP50Us','terminalP95Us','compositionWallP50Ms','compositionWallP95Ms','uiP50Ms','uiP95Ms')) {
        $metrics[$metric] = Compare-SpeedMetric @($calibrationA | ForEach-Object { $_.$metric }) @($calibrationB | ForEach-Object { $_.$metric }) @($baseline | ForEach-Object { $_.$metric }) @($candidate | ForEach-Object { $_.$metric })
    }
    $report.metrics = $metrics
    $after = Invoke-CompositionReport $CandidateRoot 'candidate-full'
    $report.compositionParity = Assert-SpeedCompositionParity $before $after $expectedArms -RequireHeldOut
    $streamPath = Join-Path $OutputDirectory 'three-streams.json'
    & $uiScript -BinaryPath (Join-Path $CandidateRoot 'target\release\hard-vibe-code.exe') -OutputPath $streamPath -Scenario 'dock-three-streams' -WindowSize '1440x900' -EnforceThresholds |
        Out-File -LiteralPath (Join-Path $OutputDirectory 'three-streams.log') -Encoding utf8
    $streamReport = Get-Content -LiteralPath $streamPath -Raw | ConvertFrom-Json
    $stream = Get-SpeedUiSample $streamReport 'dock-three-streams' -ExpectedWidth 1440 -ExpectedHeight 900
    if ($stream.uiP95Ms -gt 8.33 -or $stream.uiP99Ms -gt 16.67) { throw 'Three-stream UI exceeds the absolute latency limits.' }
    $report.streamingFixture = $streamReport
    foreach ($artifacts in @($report.baselineArtifacts,$report.candidateArtifacts)) {
        foreach ($artifact in $artifacts.Values) {
            if ((Get-FileHash -LiteralPath $artifact.path -Algorithm SHA256).Hash -ne $artifact.sha256) { throw 'A measured binary changed during the gate.' }
        }
    }
    $report.passed = @($metrics.Values | Where-Object { -not $_.passed }).Count -eq 0
    if (-not $report.passed) { throw 'Speed regression exceeds the frozen calibration noise.' }
} catch {
    $report.error = $_.Exception.Message
    throw
} finally {
    $report.finishedAt = [DateTime]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.json') -Encoding utf8
    $raw.ToArray() | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'raw-samples.json') -Encoding utf8
}
