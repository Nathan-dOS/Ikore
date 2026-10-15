<#
.SYNOPSIS
Post-run analysis of a pulsation recorded in the execution database.

.DESCRIPTION
Reports where a pulsation spent its wall clock, its CLI time, its tokens and
the developer's attention, so a pipeline change can be measured against an
earlier run instead of argued about. The database is opened read-only, so this
is safe to run while the desktop app is open.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/analyze-pulsation.ps1

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/analyze-pulsation.ps1 -Id 0de4fb85-c773-42f7-a35e-b5d7fc3381b8 -Json quality/pulsation-analysis.json

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/analyze-pulsation.ps1 -Latest -Html quality/pulsation-report.html
#>
[CmdletBinding()]
param(
    # Pulsation to analyze. Defaults to the most recent one that reached `done`.
    [string]$Id,
    # Analyze the most recent pulsation whatever its status.
    [switch]$Latest,
    # Database path. Defaults to the same location the desktop app uses.
    [string]$Database,
    # Also write the full machine-readable report here.
    [string]$Json,
    # Also write the self-contained HTML pulsation report (ADR-024) here.
    [string]$Html
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path

$arguments = @('run', '--quiet', '--package', 'hvc-test-support', '--example', 'analyze_pulsation', '--')
if ($Id) {
    $arguments += @('--id', $Id)
} elseif ($Latest) {
    $arguments += '--latest'
} else {
    $arguments += '--latest-done'
}
if ($Database) { $arguments += @('--db', $Database) }
if ($Json) {
    $resolved = if ([System.IO.Path]::IsPathRooted($Json)) { $Json } else { Join-Path $repositoryRoot $Json }
    $arguments += @('--json', $resolved)
}
if ($Html) {
    $resolved = if ([System.IO.Path]::IsPathRooted($Html)) { $Html } else { Join-Path $repositoryRoot $Html }
    $arguments += @('--html', $resolved)
}

Push-Location $repositoryRoot
try {
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "analyze_pulsation falhou com exit code $LASTEXITCODE."
    }
} finally {
    Pop-Location
}
