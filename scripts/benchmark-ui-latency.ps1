[CmdletBinding()]
param(
    [string]$BinaryPath = '',
    [string]$OutputPath = '',
    [ValidateRange(20, 2000)]
    [int]$Samples = 140,
    [ValidateRange(1, 1000)]
    [int]$WarmupSamples = 20,
    [ValidateRange(5, 120)]
    [int]$TimeoutSeconds = 30,
    [switch]$EnforceThresholds
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $BinaryPath = Join-Path $repositoryRoot 'target\release\hard-vibe-code.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot 'target\stage6\ui-latency.json'
}
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)

if ($null -eq ('HvcUiBenchmark.NativeWindow' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace HvcUiBenchmark
{
    public static class NativeWindow
    {
        private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);
        [DllImport("user32.dll")]
        private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

        public static IntPtr FindVisibleForProcess(uint processId)
        {
            IntPtr result = IntPtr.Zero;
            EnumWindows(delegate(IntPtr window, IntPtr parameter)
            {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner == processId && IsWindowVisible(window))
                {
                    result = window;
                    return false;
                }
                return true;
            }, IntPtr.Zero);
            return result;
        }

        public static bool RequestClose(IntPtr window)
        {
            const uint WmClose = 0x0010;
            return PostMessage(window, WmClose, IntPtr.Zero, IntPtr.Zero);
        }
    }
}
'@
}

$stageRoot = Join-Path $repositoryRoot 'target\stage6'
$runtimeRoot = Join-Path $stageRoot 'ui-latency-runtime'
New-Item -ItemType Directory -Path $runtimeRoot -Force | Out-Null
$runtimeDirectory = Join-Path $runtimeRoot ([guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runtimeDirectory | Out-Null
New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($resolvedOutput)) -Force | Out-Null
Remove-Item -LiteralPath $resolvedOutput -Force -ErrorAction SilentlyContinue

$previousDataDirectory = $env:HVC_DATA_DIR
$previousReport = $env:HVC_BENCHMARK_UI_REPORT
$previousSamples = $env:HVC_BENCHMARK_UI_SAMPLES
$previousWarmup = $env:HVC_BENCHMARK_UI_WARMUP
$process = $null
$processMetrics = [System.Collections.Generic.List[object]]::new()

function Get-NearestRankPercentile {
    param(
        [double[]]$Values,
        [ValidateRange(0, 100)]
        [int]$Percentile
    )
    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = @($Values | Sort-Object)
    $index = [Math]::Ceiling(($Percentile / 100.0) * $sorted.Count) - 1
    return $sorted[[Math]::Max(0, [Math]::Min($index, $sorted.Count - 1))]
}

try {
    $env:HVC_DATA_DIR = $runtimeDirectory
    $env:HVC_BENCHMARK_UI_REPORT = $resolvedOutput
    $env:HVC_BENCHMARK_UI_SAMPLES = $Samples.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    $env:HVC_BENCHMARK_UI_WARMUP = $WarmupSamples.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    # Uma janela oculta/minimizada deixa de receber animation frames no backend Win32.
    $process = Start-Process -FilePath $resolvedBinary -PassThru -WindowStyle Normal
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastSampleAt = [DateTime]::UtcNow
    $process.Refresh()
    $lastCpu = $process.TotalProcessorTime
    $report = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
        $sampledAt = [DateTime]::UtcNow
        $process.Refresh()
        if ($process.HasExited) {
            throw "O binário encerrou antes de produzir o relatório de latência (exit code $($process.ExitCode))."
        }
        $elapsedMilliseconds = ($sampledAt - $lastSampleAt).TotalMilliseconds
        $cpuMilliseconds = ($process.TotalProcessorTime - $lastCpu).TotalMilliseconds
        if ($elapsedMilliseconds -gt 0) {
            $processMetrics.Add([pscustomobject]@{
                cpuPercentAllCores = [Math]::Round(
                    100.0 * $cpuMilliseconds / ($elapsedMilliseconds * [Environment]::ProcessorCount),
                    3
                )
                workingSetBytes = [int64]$process.WorkingSet64
                privateMemoryBytes = [int64]$process.PrivateMemorySize64
            })
        }
        $lastSampleAt = $sampledAt
        $lastCpu = $process.TotalProcessorTime
        if (Test-Path -LiteralPath $resolvedOutput -PathType Leaf) {
            try {
                $report = Get-Content -LiteralPath $resolvedOutput -Raw | ConvertFrom-Json
                if ($report.status -in @('completed', 'failed')) {
                    break
                }
            }
            catch {
                # A troca atômica do conteúdo pode ser observada entre truncate e write.
            }
        }
    }
    if ($null -eq $report -or $report.status -ne 'completed') {
        $lastStatus = if ($null -eq $report) { 'ausente' } else { $report.status }
        throw "O relatório de latência não foi concluído em $TimeoutSeconds segundos (status: $lastStatus)."
    }

    $displayRefreshRates = try {
        @(Get-CimInstance Win32_VideoController -ErrorAction Stop |
            Where-Object { $null -ne $_.CurrentRefreshRate -and $_.CurrentRefreshRate -gt 0 } |
            ForEach-Object { [int]$_.CurrentRefreshRate } |
            Sort-Object -Unique)
    }
    catch {
        @()
    }
    $report | Add-Member -NotePropertyName displayRefreshRatesHz -NotePropertyValue $displayRefreshRates -Force
    $cpu = @($processMetrics | ForEach-Object { [double]$_.cpuPercentAllCores })
    $workingSet = @($processMetrics | ForEach-Object { [double]$_.workingSetBytes })
    $privateMemory = @($processMetrics | ForEach-Object { [double]$_.privateMemoryBytes })
    $report | Add-Member -NotePropertyName scenario -NotePropertyValue 'idle' -Force
    $report | Add-Member -NotePropertyName processMetrics -NotePropertyValue ([pscustomobject]@{
        sampleIntervalMs = 100
        samples = $processMetrics.Count
        cpuPercentAllCores = [pscustomobject]@{
            p50 = Get-NearestRankPercentile -Values $cpu -Percentile 50
            p95 = Get-NearestRankPercentile -Values $cpu -Percentile 95
        }
        workingSetBytes = [pscustomobject]@{
            p50 = Get-NearestRankPercentile -Values $workingSet -Percentile 50
            p95 = Get-NearestRankPercentile -Values $workingSet -Percentile 95
        }
        privateMemoryBytes = [pscustomobject]@{
            p50 = Get-NearestRankPercentile -Values $privateMemory -Percentile 50
            p95 = Get-NearestRankPercentile -Values $privateMemory -Percentile 95
        }
    }) -Force
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $resolvedOutput -Encoding utf8

    $window = [HvcUiBenchmark.NativeWindow]::FindVisibleForProcess($process.Id)
    if ($window -ne [IntPtr]::Zero) {
        [void][HvcUiBenchmark.NativeWindow]::RequestClose($window)
    }
    if (-not $process.WaitForExit(5000)) {
        throw 'O GPUI não concluiu o shutdown gracioso após o benchmark de latência.'
    }
    $report | ConvertTo-Json -Depth 8
    if ($EnforceThresholds -and -not $report.passed) {
        throw "Gate de latência reprovado: P95=$($report.p95Ms) ms; P99=$($report.p99Ms) ms."
    }
}
finally {
    if ($null -ne $process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit()
    }
    foreach ($entry in @(
        @{ Name = 'HVC_DATA_DIR'; Value = $previousDataDirectory },
        @{ Name = 'HVC_BENCHMARK_UI_REPORT'; Value = $previousReport },
        @{ Name = 'HVC_BENCHMARK_UI_SAMPLES'; Value = $previousSamples },
        @{ Name = 'HVC_BENCHMARK_UI_WARMUP'; Value = $previousWarmup }
    )) {
        if ($null -eq $entry.Value) {
            Remove-Item "Env:$($entry.Name)" -ErrorAction SilentlyContinue
        }
        else {
            Set-Item "Env:$($entry.Name)" $entry.Value
        }
    }
    $resolvedRuntimeRoot = (Resolve-Path -LiteralPath $runtimeRoot).Path
    $resolvedRuntimeDirectory = (Resolve-Path -LiteralPath $runtimeDirectory).Path
    if ($resolvedRuntimeDirectory.StartsWith(
        $resolvedRuntimeRoot + [System.IO.Path]::DirectorySeparatorChar,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        Remove-Item -LiteralPath $resolvedRuntimeDirectory -Recurse -Force
    }
}
