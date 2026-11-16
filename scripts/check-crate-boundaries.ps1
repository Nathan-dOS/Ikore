[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Push-Location $repositoryRoot
try {
    # --no-deps inspects every declared local normal/build/dev/optional edge without
    # fetching registry packages for platforms not installed on this machine.
    $metadataText = (& cargo metadata --locked --offline --no-deps --format-version 1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed while checking crate boundaries.' }
    $metadata = $metadataText | ConvertFrom-Json
    $contracts = @($metadata.packages | Where-Object { $_.name -eq 'ikore-contracts' -and $null -eq $_.source })
    $terminal = @($metadata.packages | Where-Object { $_.name -eq 'ikore-terminal' -and $null -eq $_.source })
    $voice = @($metadata.packages | Where-Object { $_.name -eq 'ikore-voice' -and $null -eq $_.source })
    $core = @($metadata.packages | Where-Object { $_.name -eq 'ikore-core' -and $null -eq $_.source })
    if ($core.Count -ne 1) { throw 'The workspace must contain exactly one ikore-core package.' }
    foreach ($dependency in $core[0].dependencies) {
        if ($dependency.name -in @('gpui', 'ikore-ui', 'ikore-app')) {
            throw "ikore-core has a forbidden UI dependency on $($dependency.name), including build/dev edges."
        }
    }
    if ($contracts.Count -ne 1 -or $terminal.Count -ne 1 -or $voice.Count -ne 1) {
        throw 'The workspace must contain exactly one ikore-contracts, one ikore-terminal and one ikore-voice package.'
    }
    foreach ($dependency in $contracts[0].dependencies) {
        if ($dependency.name -notin @('serde', 'serde_json')) {
            throw "ikore-contracts may depend only on serde/serde_json; found $($dependency.name)."
        }
    }

    # The only allowed local edge is terminal -> contracts, whose complete
    # dependency set was checked above. This rules out indirect local cycles too.
    foreach ($dependency in $terminal[0].dependencies) {
        if ($dependency.name -in @('ikore-core', 'ikore-ui', 'ikore-app', 'ikore-test-support', 'gpui') -or
            ($dependency.path -and $dependency.name -ne 'ikore-contracts')) {
            throw "ikore-terminal has a forbidden dependency on $($dependency.name), including build/dev edges."
        }
    }
    # Voice is a leaf: no local crate at all, so the host can depend on it
    # without the speech runtime ever reaching back into the app or GPUI.
    foreach ($dependency in $voice[0].dependencies) {
        if ($dependency.name -eq 'gpui' -or $dependency.path) {
            throw "ikore-voice has a forbidden dependency on $($dependency.name), including build/dev edges."
        }
    }
    Write-Output 'Crate boundaries passed: core is UI-independent; contracts are independent; terminal and voice have no dependency on the host or GPUI.'
}
finally {
    Pop-Location
}
