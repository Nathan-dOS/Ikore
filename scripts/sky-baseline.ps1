param(
    [Parameter(Mandatory=$true)][ValidateSet('retrieval','scale-100','scale-1000','scale-10000','mutations','pulsations','live','e7','sqlite')][string]$Phase,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [string]$Profile = 'debug'
)
$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $repository "target\$Profile\examples\sky_baseline.exe"
$directory = [IO.Directory]::CreateDirectory($OutputDirectory)
$outputFile = Join-Path $directory.FullName "$Phase.json"
$child = New-Object Diagnostics.Process
$child.StartInfo.FileName = $binary
$child.StartInfo.Arguments = "$Phase ""$outputFile"""
$child.StartInfo.WorkingDirectory = $repository
$child.StartInfo.UseShellExecute = $false
$child.StartInfo.CreateNoWindow = $true
try {
    [void]$child.Start()
    $peak = 0L
    do {
        $child.Refresh()
        if (!$child.HasExited) {
            $peak = [Math]::Max($peak, $child.PeakWorkingSet64)
        }
    } until ($child.WaitForExit(25))
    if ($child.ExitCode -ne 0) { throw "$Phase exited with code $($child.ExitCode)" }
    $memoryFile = Join-Path $directory.FullName "$Phase-memory.json"
    [IO.File]::WriteAllText($memoryFile, (@{phase=$Phase;peak_working_set_bytes=$peak} | ConvertTo-Json))
} finally {
    $child.Dispose()
}
