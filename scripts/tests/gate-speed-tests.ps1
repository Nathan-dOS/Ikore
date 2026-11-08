$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\lib\speed-metrics.ps1')
function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
Assert-True ((Get-SpeedPercentile @(1,2,3,4,5) 0.95) -eq 5) 'Nearest rank p95 failed'
$equal = Compare-SpeedMetric @(10,10,10) @(11,10,9) @(10,10,10) @(11,10,10)
Assert-True $equal.passed 'A change within calibrated noise should pass'
$slow = Compare-SpeedMetric @(10,10,10) @(10,10,10) @(10,10,10) @(12,12,12)
Assert-True (-not $slow.passed) 'A slowdown beyond noise should fail'
$faster = Compare-SpeedMetric @(10,10,10) @(10,10,10) @(10,10,10) @(9,8,7)
Assert-True $faster.passed 'An improvement should pass'
$failed = $false
try { Compare-SpeedMetric @(1,2) @(1) @(1) @(1) | Out-Null } catch { $failed = $true }
Assert-True $failed 'Mismatched sample counts must be rejected'
$failed = $false
try { Get-SpeedPercentile @() 0.5 | Out-Null } catch { $failed = $true }
Assert-True $failed 'Empty samples must not certify a comparison'
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    Assert-True $failed $Message
}
Assert-Throws { Get-SpeedPercentile @(1,$null,3) 0.5 } 'Null samples must not become zero'
Assert-Throws { Compare-SpeedMetric @(1,2) @(1,2) @(1) @(1) } 'Calibration and comparison counts must match'
Assert-Throws { Compare-SpeedMetric @([double]::NaN) @(1) @(1) @(1) } 'NaN calibration must fail'
Assert-Throws { Compare-SpeedMetric @(1) @(1) @(-1) @(1) } 'Negative measured durations must fail'

$transition = '{"measurement":"pulsation_transitions","result":{"non_terminal":{"n":1000,"p50_us":12,"p95_us":18},"terminal_done":{"n":200,"p50_us":30,"p95_us":40}}}' | ConvertFrom-Json
Assert-True ((Get-SpeedTransitionSample $transition).nonTerminalP95Us -eq 18) 'Transition schema must use result.non_terminal.p95_us'
$probeLine = 'test persistence_service::exchange_baseline_tests::exchange_baseline_pulsation_transitions ... AGENT_EXCHANGE_BASELINE ' + ($transition | ConvertTo-Json -Depth 5 -Compress)
Assert-True ((Get-SpeedTransitionFromOutput @('running 1 test',$probeLine,'ok')).terminalP95Us -eq 40) 'Rust harness prefixes must not hide the probe JSON'
Assert-Throws { Get-SpeedTransitionFromOutput @($probeLine,$probeLine) } 'Repeated probe output must be rejected'
$transition.result.non_terminal.PSObject.Properties.Remove('p95_us')
Assert-Throws { Get-SpeedTransitionSample $transition } 'Missing transition fields must not become zero'

$ui = '{"status":"completed","metric":"gpui_animation_frame_callback_interval_ms","scenario":"dock-three-streams","streamingFixture":true,"fixtureFrames":140,"updatedConversations":420,"fixtureReady":true,"warmupSamples":20,"viewportWidth":1440,"viewportHeight":900,"samples":4,"rawMs":[1,2,3,4],"p95Ms":4,"p99Ms":4,"passed":true}' | ConvertFrom-Json
Assert-True ((Get-SpeedUiSample $ui 'dock-three-streams').uiP50Ms -eq 2) 'UI samples must be recomputed from raw data'
$ui.fixtureFrames = 0
Assert-Throws { Get-SpeedUiSample $ui 'dock-three-streams' } 'A requested fixture flag does not prove it ran'
$ui.fixtureFrames = 140
$ui.fixtureReady = $false
Assert-Throws { Get-SpeedUiSample $ui 'dock-three-streams' } 'An incomplete fixture must not pass'
$ui.fixtureReady = $true
$ui.viewportWidth = 1280
Assert-Throws { Get-SpeedUiSample $ui 'dock-three-streams' -ExpectedWidth 1440 -ExpectedHeight 900 } 'Wrong viewport dimensions must fail'
$ui.viewportWidth = 1440
$ui.p95Ms = 0
Assert-Throws { Get-SpeedUiSample $ui 'dock-three-streams' } 'Inconsistent UI percentile summaries must fail'

function New-CompositionFixture {
    return [pscustomobject]@{
        meta = [pscustomobject]@{ mode='fixture'; fixture_version='fixture-v1'; fixtures_sha256=('a'*64); arms=@('a'); fixtures=@([pscustomobject]@{id='fixture-one';held_out=$true}) }
        observations = @([pscustomobject]@{ fixture='fixture-one'; arm='a'; mode='fixture'; completed=$true; correct=$true; provider_calls=6; wall_ms=12; interventions=0; human_checkpoints=4; failure_checkpoints=0; rejected_attempts=0 })
    }
}
$before = New-CompositionFixture
$after = New-CompositionFixture
Assert-True ((Assert-SpeedCompositionParity $before $after).observations -eq 1) 'Composition keys must use fixture, not fixture_id'
$after.observations = @()
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'Empty reports must not pass'
$after = New-CompositionFixture
$after.observations[0].PSObject.Properties.Remove('fixture')
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'A missing fixture key must fail'
$after = New-CompositionFixture
$after.observations = @($after.observations[0],$after.observations[0])
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'Duplicate observations must not overwrite one another'
$after = New-CompositionFixture
$after.observations[0].correct = 'true'
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'JSON strings must not count as true booleans'
$after = New-CompositionFixture
$after.observations[0].provider_calls = 7
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'Additional model calls must fail'
$after = New-CompositionFixture
$after.meta.fixtures_sha256 = 'b'*64
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'Fixture identities must match'
$after = New-CompositionFixture
$after.observations[0].interventions = 1
Assert-Throws { Assert-SpeedCompositionParity $before $after } 'More interventions must fail'
Write-Output 'Speed metric, report-schema and parity tests passed (27 scenarios).'
