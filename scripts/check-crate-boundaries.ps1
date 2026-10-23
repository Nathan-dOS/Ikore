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
    $contracts = @($metadata.packages | Where-Object { $_.name -eq 'hvc-contracts' -and $null -eq $_.source })
    $terminal = @($metadata.packages | Where-Object { $_.name -eq 'hvc-terminal' -and $null -eq $_.source })
    $voice = @($metadata.packages | Where-Object { $_.name -eq 'hvc-voice' -and $null -eq $_.source })
    if ($contracts.Count -ne 1 -or $terminal.Count -ne 1 -or $voice.Count -ne 1) {
        throw 'The workspace must contain exactly one hvc-contracts, one hvc-terminal and one hvc-voice package.'
    }
    foreach ($dependency in $contracts[0].dependencies) {
        if ($dependency.name -notin @('serde', 'serde_json')) {
            throw "hvc-contracts may depend only on serde/serde_json; found $($dependency.name)."
        }
    }

    # The only allowed local edge is terminal -> contracts, whose complete
    # dependency set was checked above. This rules out indirect local cycles too.
    foreach ($dependency in $terminal[0].dependencies) {
        if ($dependency.name -in @('hvc-core', 'hvc-ui', 'hvc-app', 'hvc-test-support', 'gpui') -or
            ($dependency.path -and $dependency.name -ne 'hvc-contracts')) {
            throw "hvc-terminal has a forbidden dependency on $($dependency.name), including build/dev edges."
        }
    }
    # Voice is a leaf: no local crate at all, so the host can depend on it
    # without the speech runtime ever reaching back into the app or GPUI.
    foreach ($dependency in $voice[0].dependencies) {
        if ($dependency.name -eq 'gpui' -or $dependency.path) {
            throw "hvc-voice has a forbidden dependency on $($dependency.name), including build/dev edges."
        }
    }
    Write-Output 'Crate boundaries passed: contracts are independent; terminal and voice have no dependency on the host or GPUI.'
}
finally {
    Pop-Location
}
