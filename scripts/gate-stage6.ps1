[CmdletBinding()]
param(
    [string]$BenchmarkDatasetPath = '',
    [switch]$SkipBenchmarks,
    [switch]$SkipAdvisories,
    [switch]$EnforceDistribution
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path

function Invoke-GateStep([string]$Name, [scriptblock]$Action) {
    Write-Host "`n[Etapa 6] $Name"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "Gate '$Name' falhou com exit code $LASTEXITCODE."
    }
}

Push-Location $repositoryRoot
try {
    Invoke-GateStep 'cargo fmt' { cargo fmt --all -- --check }
    Invoke-GateStep 'Clippy' { cargo clippy --workspace --all-targets -- -D warnings }
    Invoke-GateStep 'testes Rust' { cargo test --workspace }
    Invoke-GateStep 'testes frontend legado' { npm.cmd test }
    Invoke-GateStep 'build frontend legado' { npm.cmd run build }
    Invoke-GateStep 'build release GPUI' { cargo build --release -p hvc-app }
    Invoke-GateStep 'build release Tauri legado' { cargo build --release -p hvc-tauri-legacy }
    Invoke-GateStep 'smoke Windows GPUI' { & (Join-Path $PSScriptRoot 'smoke-windows.ps1') }
    Invoke-GateStep 'licenças e fontes' {
        & (Join-Path $PSScriptRoot 'audit-supply-chain.ps1') -EnforceDistribution:$EnforceDistribution
    }
    if (-not $SkipAdvisories) {
        Invoke-GateStep 'advisories Rust/npm' { & (Join-Path $PSScriptRoot 'audit-advisories.ps1') }
    }
    if (-not $SkipBenchmarks) {
        Invoke-GateStep 'benchmark Windows comparativo' {
            & (Join-Path $PSScriptRoot 'benchmark-windows.ps1') `
                -DatasetPath $BenchmarkDatasetPath `
                -EnforceThresholds
        }
        Invoke-GateStep 'latência input-render GPUI' {
            & (Join-Path $PSScriptRoot 'benchmark-ui-latency.ps1') -EnforceThresholds
        }
    }
    Write-Host "`nGate técnico da Etapa 6 aprovado."
}
finally {
    Pop-Location
}
