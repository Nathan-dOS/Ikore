[CmdletBinding()]
param(
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'target\stage7\advisories'
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null

$localToolDirectory = Join-Path $repositoryRoot 'target\tools\cargo-audit\bin'
$previousPath = $env:PATH
if (Test-Path -LiteralPath (Join-Path $localToolDirectory 'cargo-audit.exe')) {
    $env:PATH = "$localToolDirectory$([System.IO.Path]::PathSeparator)$previousPath"
}
try {
    $cargoOutput = (& cargo audit --json 2>&1 | Out-String)
    $cargoExitCode = $LASTEXITCODE
    $cargoOutput | Set-Content -LiteralPath (Join-Path $resolvedOutput 'cargo-audit.json') -Encoding utf8

    $cargoReport = try { $cargoOutput | ConvertFrom-Json } catch { $null }
    $cargoWarningCounts = [ordered]@{}
    if ($null -ne $cargoReport -and $null -ne $cargoReport.warnings) {
        foreach ($property in $cargoReport.warnings.PSObject.Properties) {
            $cargoWarningCounts[$property.Name] = @($property.Value).Count
        }
    }
    $summary = [ordered]@{
        schemaVersion = 1
        auditedAt = [DateTime]::UtcNow.ToString('o')
        cargoAuditExitCode = $cargoExitCode
        cargoVulnerabilities = if ($null -ne $cargoReport) { $cargoReport.vulnerabilities.count } else { $null }
        cargoInformationalWarnings = $cargoWarningCounts
        rustSecLastCommit = if ($null -ne $cargoReport) { $cargoReport.database.'last-commit' } else { $null }
        passed = $cargoExitCode -eq 0
    }
    $summary | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $resolvedOutput 'summary.json') -Encoding utf8
    $summary | ConvertTo-Json
    if (-not $summary.passed) {
        throw 'Auditoria de advisories Rust reprovada. Consulte target/stage7/advisories.'
    }
}
finally {
    $env:PATH = $previousPath
}
