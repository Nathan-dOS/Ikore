function Get-RequiredSpeedProperty {
    param($Object, [string]$Name)
    if ($null -eq $Object -or -not $Object.PSObject.Properties[$Name]) { throw "Missing speed-report property: $Name" }
    return $Object.PSObject.Properties[$Name].Value
}

function ConvertTo-SpeedNumber {
    param($Value, [switch]$AllowNegative)
    if ($null -eq $Value -or $Value -is [bool] -or $Value -is [string] -or $Value -isnot [ValueType]) {
        throw 'Speed measurements must be JSON numbers, not null or text.'
    }
    try { $number = [double]$Value } catch { throw 'Invalid numeric speed measurement.' }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number) -or (-not $AllowNegative -and $number -lt 0)) {
        throw 'Speed measurements must be finite nonnegative numbers.'
    }
    return $number
}

function Get-SpeedPercentile {
    param([object[]]$Values, [double]$Quantile)
    if ($Values.Count -eq 0 -or [double]::IsNaN($Quantile) -or [double]::IsInfinity($Quantile) -or $Quantile -lt 0 -or $Quantile -gt 1) { throw 'Invalid speed samples or quantile.' }
    $sorted = @($Values | ForEach-Object { ConvertTo-SpeedNumber $_ -AllowNegative } | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($Quantile * $sorted.Count) - 1)
    return [double]$sorted[$index]
}

function Compare-SpeedMetric {
    param([object[]]$CalibrationA, [object[]]$CalibrationB, [object[]]$Baseline, [object[]]$Candidate)
    if ($CalibrationA.Count -eq 0 -or $CalibrationA.Count -ne $CalibrationB.Count -or $Baseline.Count -ne $Candidate.Count -or $Baseline.Count -ne $CalibrationA.Count) {
        throw 'Speed comparisons require nonempty paired samples.'
    }
    $CalibrationA = @($CalibrationA | ForEach-Object { ConvertTo-SpeedNumber $_ })
    $CalibrationB = @($CalibrationB | ForEach-Object { ConvertTo-SpeedNumber $_ })
    $Baseline = @($Baseline | ForEach-Object { ConvertTo-SpeedNumber $_ })
    $Candidate = @($Candidate | ForEach-Object { ConvertTo-SpeedNumber $_ })
    $noise = for ($index = 0; $index -lt $CalibrationA.Count; $index++) { [Math]::Abs($CalibrationB[$index] - $CalibrationA[$index]) }
    $deltas = for ($index = 0; $index -lt $Baseline.Count; $index++) { $Candidate[$index] - $Baseline[$index] }
    $tolerance = Get-SpeedPercentile @($noise) 0.95
    $delta = Get-SpeedPercentile @($deltas) 0.5
    return [pscustomobject]@{
        samples = $Baseline.Count
        calibrationPairs = $CalibrationA.Count
        noiseP95 = $tolerance
        pairedMedianDelta = $delta
        passed = $delta -le $tolerance
    }
}

function Get-SpeedTransitionSample {
    param($Report)
    if ((Get-RequiredSpeedProperty $Report 'measurement') -ne 'pulsation_transitions') { throw 'Unexpected transition measurement.' }
    $result = Get-RequiredSpeedProperty $Report 'result'
    $nonTerminal = Get-RequiredSpeedProperty $result 'non_terminal'
    $terminal = Get-RequiredSpeedProperty $result 'terminal_done'
    foreach ($section in @($nonTerminal,$terminal)) {
        if ((ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $section 'n')) -le 0) { throw 'A transition report has no observations.' }
    }
    return [pscustomobject]@{
        nonTerminalP50Us = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $nonTerminal 'p50_us')
        nonTerminalP95Us = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $nonTerminal 'p95_us')
        terminalP50Us = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $terminal 'p50_us')
        terminalP95Us = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $terminal 'p95_us')
    }
}

function Get-SpeedTransitionFromOutput {
    param([object[]]$Lines)
    $prefix = 'AGENT_EXCHANGE_BASELINE '
    $payloads = @($Lines | ForEach-Object {
        $line = $_.ToString()
        $offset = $line.IndexOf($prefix, [StringComparison]::Ordinal)
        if ($offset -ge 0) { $line.Substring($offset + $prefix.Length) }
    })
    if ($payloads.Count -ne 1) { throw 'Transition probe returned no unique measurement.' }
    return Get-SpeedTransitionSample ($payloads[0] | ConvertFrom-Json)
}

function Get-SpeedUiSample {
    param($Report, [string]$ExpectedScenario, [switch]$AllowLegacyIdle, [double]$ExpectedWidth = 0, [double]$ExpectedHeight = 0)
    if ((Get-RequiredSpeedProperty $Report 'status') -ne 'completed' -or (Get-RequiredSpeedProperty $Report 'metric') -ne 'gpui_animation_frame_callback_interval_ms') {
        throw 'UI report is incomplete or describes a different metric.'
    }
    $scenario = if ($Report.PSObject.Properties['scenario']) { $Report.scenario } else { 'idle' }
    if ($scenario -ne $ExpectedScenario -and -not ($AllowLegacyIdle -and $ExpectedScenario -eq 'dock-closed' -and $scenario -eq 'idle')) {
        throw "UI scenario mismatch: expected $ExpectedScenario, observed $scenario"
    }
    $passed = Get-RequiredSpeedProperty $Report 'passed'
    if ($passed -isnot [bool]) { throw 'UI pass state must be a JSON boolean.' }
    if ($ExpectedWidth -gt 0 -and $ExpectedHeight -gt 0 -and -not ($AllowLegacyIdle -and -not $Report.PSObject.Properties['viewportWidth'])) {
        $width = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'viewportWidth')
        $height = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'viewportHeight')
        if ([Math]::Abs($width - $ExpectedWidth) -gt 1 -or [Math]::Abs($height - $ExpectedHeight) -gt 1) { throw 'UI viewport dimensions differ from the requested scenario.' }
    }
    $samples = @(Get-RequiredSpeedProperty $Report 'rawMs')
    $count = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'samples')
    if ($samples.Count -eq 0 -or $count -ne $samples.Count) { throw 'UI raw sample count does not match the report.' }
    $numbers = @($samples | ForEach-Object { ConvertTo-SpeedNumber $_ })
    $p95 = Get-SpeedPercentile $numbers 0.95
    $p99 = Get-SpeedPercentile $numbers 0.99
    if ([Math]::Abs($p95 - (ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'p95Ms'))) -gt 0.000001 -or [Math]::Abs($p99 - (ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'p99Ms'))) -gt 0.000001) {
        throw 'UI summary percentiles do not agree with the raw samples.'
    }
    if ($ExpectedScenario -eq 'dock-three-streams') {
        $fixture = Get-RequiredSpeedProperty $Report 'streamingFixture'
        $ready = Get-RequiredSpeedProperty $Report 'fixtureReady'
        $frames = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'fixtureFrames')
        $updates = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'updatedConversations')
        $warmup = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $Report 'warmupSamples')
        if ($fixture -isnot [bool] -or -not $fixture -or $ready -isnot [bool] -or -not $ready -or $frames -le $warmup -or $updates -ne ($frames * 3)) {
            throw 'The three-conversation streaming fixture did not run.'
        }
    }
    return [pscustomobject]@{ uiP50Ms = Get-SpeedPercentile $numbers 0.5; uiP95Ms = $p95; uiP99Ms = $p99 }
}

function Get-SpeedCompositionIndex {
    param($Report, [string[]]$ExpectedArms = @(), [switch]$RequireHeldOut)
    $meta = Get-RequiredSpeedProperty $Report 'meta'
    if ((Get-RequiredSpeedProperty $meta 'mode') -ne 'fixture') { throw 'A speed gate must use offline fixture mode.' }
    $hash = Get-RequiredSpeedProperty $meta 'fixtures_sha256'
    if ($hash -isnot [string] -or $hash -notmatch '^[a-fA-F0-9]{64}$' -or -not (Get-RequiredSpeedProperty $meta 'fixture_version')) { throw 'Composition fixture identity is missing.' }
    $arms = @(Get-RequiredSpeedProperty $meta 'arms')
    $fixtures = @(Get-RequiredSpeedProperty $meta 'fixtures')
    if ($arms.Count -eq 0 -or $fixtures.Count -eq 0 -or @($arms | Sort-Object -Unique).Count -ne $arms.Count) { throw 'Composition fixture/arm inventory is empty or duplicated.' }
    if ($ExpectedArms.Count -gt 0 -and (@($ExpectedArms | Sort-Object) -join ',') -ne (@($arms | Sort-Object) -join ',')) { throw 'Composition arm inventory differs from the requested set.' }
    $expected = @{}
    $heldOut = $false
    foreach ($fixture in $fixtures) {
        $fixtureId = Get-RequiredSpeedProperty $fixture 'id'
        $held = Get-RequiredSpeedProperty $fixture 'held_out'
        if ($fixtureId -isnot [string] -or -not $fixtureId -or $held -isnot [bool]) { throw 'Malformed composition fixture metadata.' }
        $heldOut = $heldOut -or $held
        foreach ($arm in $arms) {
            if ($arm -isnot [string] -or $arm -notin @('a','b','c','d')) { throw 'Unknown composition arm.' }
            $key = "$fixtureId`:$arm"
            if ($expected.ContainsKey($key)) { throw 'Duplicate composition fixture metadata.' }
            $expected[$key] = $true
        }
    }
    if ($RequireHeldOut -and -not $heldOut) { throw 'Held-out fixtures are missing.' }
    $observations = @(Get-RequiredSpeedProperty $Report 'observations')
    if ($observations.Count -ne $expected.Count) { throw 'Composition observation coverage is incomplete or duplicated.' }
    $index = @{}
    foreach ($observation in $observations) {
        $fixtureId = Get-RequiredSpeedProperty $observation 'fixture'
        $arm = Get-RequiredSpeedProperty $observation 'arm'
        $key = "$fixtureId`:$arm"
        if (-not $expected.ContainsKey($key) -or $index.ContainsKey($key)) { throw "Unexpected or duplicate composition observation: $key" }
        foreach ($name in @('correct','completed')) {
            $value = Get-RequiredSpeedProperty $observation $name
            if ($value -isnot [bool] -or -not $value) { throw "Composition $name failed: $key" }
        }
        if ((Get-RequiredSpeedProperty $observation 'mode') -ne 'fixture') { throw 'A composition observation is not a fixture run.' }
        foreach ($name in @('provider_calls','wall_ms','interventions','human_checkpoints','failure_checkpoints','rejected_attempts')) {
            $number = ConvertTo-SpeedNumber (Get-RequiredSpeedProperty $observation $name)
            if ($number -ne [Math]::Floor($number)) { throw "Composition counter must be an integer: $name" }
        }
        $index[$key] = $observation
    }
    return $index
}

function Assert-SpeedCompositionParity {
    param($Baseline, $Candidate, [string[]]$ExpectedArms = @(), [switch]$RequireHeldOut)
    $before = Get-SpeedCompositionIndex $Baseline $ExpectedArms -RequireHeldOut:$RequireHeldOut
    $after = Get-SpeedCompositionIndex $Candidate $ExpectedArms -RequireHeldOut:$RequireHeldOut
    if ($Baseline.meta.fixtures_sha256 -ne $Candidate.meta.fixtures_sha256 -or $Baseline.meta.fixture_version -ne $Candidate.meta.fixture_version -or $before.Count -ne $after.Count) { throw 'Composition fixture identities differ.' }
    foreach ($key in $before.Keys) {
        if (-not $after.ContainsKey($key) -or $before[$key].provider_calls -ne $after[$key].provider_calls) { throw "Happy-path invocation count changed: $key" }
        foreach ($name in @('interventions','human_checkpoints','failure_checkpoints','rejected_attempts')) {
            if ($after[$key].$name -gt $before[$key].$name) { throw "Composition $name increased: $key" }
        }
    }
    return [pscustomobject]@{ observations=$after.Count; fixtureSha256=$Candidate.meta.fixtures_sha256 }
}
