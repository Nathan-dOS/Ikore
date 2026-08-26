[CmdletBinding()]
param(
    [switch]$SkipAdvisories,
    [switch]$EnforceDistribution
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path

function Invoke-GateStep([string]$Name, [scriptblock]$Action) {
    Write-Host "`n[Etapa 7] $Name"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "Gate '$Name' falhou com exit code $LASTEXITCODE."
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
    Invoke-GateStep 'testes Rust' { cargo test --workspace }
    Invoke-GateStep 'build release GPUI' { cargo build --release -p hvc-app }
    Invoke-GateStep 'smoke Windows GPUI' { & (Join-Path $PSScriptRoot 'smoke-windows.ps1') }
    Invoke-GateStep 'licenças e fontes Rust' {
        & (Join-Path $PSScriptRoot 'audit-supply-chain.ps1') -EnforceDistribution:$EnforceDistribution
    }
    if (-not $SkipAdvisories) {
        Invoke-GateStep 'advisories Rust' { & (Join-Path $PSScriptRoot 'audit-advisories.ps1') }
    }
    Write-Host "`nGate técnico da Etapa 7 aprovado: aplicação exclusivamente GPUI/Rust."
}
finally {
    Pop-Location
}
