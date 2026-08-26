[CmdletBinding()]
param(
    [string]$OutputPath = '',
    [string]$PolicyPath = '',
    [string]$TargetTriple = 'x86_64-pc-windows-msvc',
    [switch]$EnforceDistribution
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot 'target\stage7\supply-chain.json'
}
if ([string]::IsNullOrWhiteSpace($PolicyPath)) {
    $PolicyPath = Join-Path $repositoryRoot 'quality\distribution-policy.json'
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)
$policy = Get-Content -LiteralPath $PolicyPath -Raw | ConvertFrom-Json

$metadataText = & cargo metadata --locked --offline --filter-platform $TargetTriple --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw 'cargo metadata falhou; a auditoria não pode usar um grafo parcial.'
}
$metadata = $metadataText | ConvertFrom-Json
$nodeById = @{}
foreach ($node in $metadata.resolve.nodes) {
    $nodeById[$node.id] = $node
}
$activeIds = [System.Collections.Generic.HashSet[string]]::new()
$pendingIds = [System.Collections.Generic.Queue[string]]::new()
foreach ($workspaceMember in $metadata.workspace_members) {
    $pendingIds.Enqueue($workspaceMember)
}
while ($pendingIds.Count -gt 0) {
    $packageId = $pendingIds.Dequeue()
    if (-not $activeIds.Add($packageId)) {
        continue
    }
    $node = $nodeById[$packageId]
    if ($null -ne $node) {
        foreach ($dependency in $node.deps) {
            $pendingIds.Enqueue($dependency.pkg)
        }
    }
}
$rustPackages = @($metadata.packages | Where-Object { $activeIds.Contains($_.id) })
$rustDenied = @($rustPackages | Where-Object { $_.license -match '(?<!L)GPL-|AGPL-|SSPL-' })
$rustMissing = @($rustPackages | Where-Object { [string]::IsNullOrWhiteSpace($_.license) })
$rustSourceReview = @($rustPackages | Where-Object {
    $null -ne $_.source -and $_.source -notin $policy.allowedRegistrySources
})
$rustNotice = @($rustPackages | Where-Object { $_.license -match 'LGPL-|MPL-|CDDL-|EPL-' })

$technicalPassed = (
    $rustDenied.Count -eq 0 -and
    $rustMissing.Count -eq 0 -and
    $rustSourceReview.Count -eq 0
)
$legalBlockers = @($policy.legalBlockers | Where-Object {
    $_.disposition -eq 'distribution_blocking' -and $_.status -notin @('resolved', 'approved')
})
$distributionAllowed = $technicalPassed -and $legalBlockers.Count -eq 0
$report = [ordered]@{
    schemaVersion = 1
    auditedAt = [DateTime]::UtcNow.ToString('o')
    target = $TargetTriple
    rust = [ordered]@{
        resolvedPackages = $rustPackages.Count
        deniedLicenses = @($rustDenied | Select-Object name, version, license, source)
        missingLicenseMetadata = @($rustMissing | Select-Object name, version, source)
        sourceReview = @($rustSourceReview | Select-Object name, version, license, source)
        noticeLicenses = @($rustNotice | Select-Object name, version, license, source)
    }
    technicalPassed = $technicalPassed
    legalBlockers = $legalBlockers
    distributionAllowed = $distributionAllowed
}
New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($resolvedOutput)) -Force | Out-Null
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $resolvedOutput -Encoding utf8
$report | ConvertTo-Json -Depth 12
if (-not $technicalPassed) {
    throw 'Auditoria técnica de licenças/fontes reprovada. Consulte o relatório JSON.'
}
if ($EnforceDistribution -and -not $distributionAllowed) {
    throw "Distribuição bloqueada por: $($legalBlockers.id -join ', ')."
}
