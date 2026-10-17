[CmdletBinding()]
param(
    [switch]$SkipAdvisories,
    [switch]$EnforceDistribution,
    [string]$TimingOutputPath = ''
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($TimingOutputPath)) {
    $TimingOutputPath = Join-Path $repositoryRoot 'target\stage7\timings.json'
}
$resolvedTimingOutput = [System.IO.Path]::GetFullPath($TimingOutputPath)
$gateStartedAt = [DateTime]::UtcNow
$gateTimer = [System.Diagnostics.Stopwatch]::StartNew()
$gateTimings = [System.Collections.Generic.List[object]]::new()
$gatePassed = $false

function Invoke-GateStep([string]$Name, [scriptblock]$Action) {
    Write-Host "`n[Etapa 7] $Name"
    $stepTimer = [System.Diagnostics.Stopwatch]::StartNew()
    $stepPassed = $false
    $stepExitCode = $null
    # Some steps only call PowerShell functions. Do not inherit an earlier
    # native command's exit code when those steps succeed.
    $global:LASTEXITCODE = 0
    try {
        & $Action
        $stepExitCode = $global:LASTEXITCODE
        if ($stepExitCode -ne 0) {
            throw "Gate '$Name' falhou com exit code $stepExitCode."
        }
        $stepPassed = $true
    }
    finally {
        $stepTimer.Stop()
        if ($null -eq $stepExitCode -and $global:LASTEXITCODE -ne 0) {
            $stepExitCode = $global:LASTEXITCODE
        }
        $gateTimings.Add([pscustomobject]@{
            name = $Name
            durationSeconds = [Math]::Round($stepTimer.Elapsed.TotalSeconds, 3)
            passed = $stepPassed
            exitCode = $stepExitCode
        })
        Write-Host ("[Etapa 7] {0}: {1:N3} s (aprovado: {2})" -f $Name, $stepTimer.Elapsed.TotalSeconds, $stepPassed)
    }
}

function Assert-GpuiOnly {
    $legacyPaths = @(
        'src-tauri',
        'src',
        'package.json',
        'package-lock.json',
        'index.html',
        'vite.config.ts',
        'tsconfig.json',
        'tsconfig.node.json',
        'eslint.config.js'
    )
    $remaining = @($legacyPaths | Where-Object {
        Test-Path -LiteralPath (Join-Path $repositoryRoot $_)
    })
    if ($remaining.Count -gt 0) {
        throw "Artefatos legados ainda presentes: $($remaining -join ', ')"
    }

    $tree = (& cargo tree -p hvc-app --locked 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo tree falhou durante a validação GPUI-only.'
    }
    if ($tree -match '(?im)^.*\btauri(?:-|\s)') {
        throw 'A árvore do binário ainda contém dependências Tauri.'
    }
}

Push-Location $repositoryRoot
try {
    Invoke-GateStep 'estrutura exclusivamente GPUI/Rust' { Assert-GpuiOnly }
    Invoke-GateStep 'cargo fmt' { cargo fmt --all -- --check }
    Invoke-GateStep 'Clippy' { cargo clippy --workspace --all-targets -- -D warnings }
    Invoke-GateStep 'testes Rust' { cargo test-all }
    Invoke-GateStep 'build release GPUI' { cargo build --release -p hvc-app }
    Invoke-GateStep 'smoke Windows GPUI' { & (Join-Path $PSScriptRoot 'smoke-windows.ps1') }
    Invoke-GateStep 'licenças e fontes Rust' {
        & (Join-Path $PSScriptRoot 'audit-supply-chain.ps1') -EnforceDistribution:$EnforceDistribution
    }
    if (-not $SkipAdvisories) {
        Invoke-GateStep 'advisories Rust' { & (Join-Path $PSScriptRoot 'audit-advisories.ps1') }
    }
    $gatePassed = $true
    Write-Host "`nGate técnico da Etapa 7 aprovado: aplicação exclusivamente GPUI/Rust."
}
finally {
    $gateTimer.Stop()
    $timingReport = [ordered]@{
        schemaVersion = 1
        startedAt = $gateStartedAt.ToString('o')
        finishedAt = [DateTime]::UtcNow.ToString('o')
        durationSeconds = [Math]::Round($gateTimer.Elapsed.TotalSeconds, 3)
        passed = $gatePassed
        steps = @($gateTimings.ToArray())
    }
    # Reporting must not hide the original error if a gate step failed.
    try {
        New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($resolvedTimingOutput)) -Force | Out-Null
        $timingReport | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resolvedTimingOutput -Encoding utf8
        Write-Host "`nTempos do gate: $resolvedTimingOutput"
        $gateTimings | Format-Table name, durationSeconds, passed, exitCode -AutoSize | Out-Host
    }
    catch {
        if ($gatePassed) {
            Pop-Location
            throw
        }
        Write-Warning "Não foi possível gravar os tempos do gate: $_"
    }
    Pop-Location
}
