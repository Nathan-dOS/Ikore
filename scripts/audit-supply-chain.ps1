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
    $OutputPath = Join-Path $repositoryRoot 'target\stage6\supply-chain.json'
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

$npmLock = Get-Content -LiteralPath (Join-Path $repositoryRoot 'package-lock.json') -Raw |
    ConvertFrom-Json -AsHashTable
$npmPackages = @($npmLock.packages.GetEnumerator() |
    Where-Object { $_.Key -ne '' } |
    ForEach-Object { $_.Value })
$npmDenied = @($npmPackages | Where-Object { $_.license -match '(?<!L)GPL-|AGPL-|SSPL-' })
$npmMissing = @($npmPackages | Where-Object { [string]::IsNullOrWhiteSpace($_.license) })
$npmSourceReview = @($npmPackages | Where-Object {
    $resolvedSource = $_.resolved
    -not [string]::IsNullOrWhiteSpace($resolvedSource) -and
    -not ($policy.allowedRegistrySources | Where-Object { $resolvedSource.StartsWith($_) })
})

$technicalPassed = (
    $rustDenied.Count -eq 0 -and
    $rustMissing.Count -eq 0 -and
    $rustSourceReview.Count -eq 0 -and
    $npmDenied.Count -eq 0 -and
    $npmMissing.Count -eq 0 -and
    $npmSourceReview.Count -eq 0
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
    npm = [ordered]@{
        lockedPackages = $npmPackages.Count
        deniedLicenses = @($npmDenied | Select-Object name, version, license, resolved)
        missingLicenseMetadata = @($npmMissing | Select-Object name, version, resolved)
        sourceReview = @($npmSourceReview | Select-Object name, version, license, resolved)
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
