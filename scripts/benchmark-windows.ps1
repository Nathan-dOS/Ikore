[CmdletBinding()]
param(
    [string]$GpuiBinaryPath = '',
    [string]$TauriBinaryPath = '',
    [string]$DatasetPath = '',
    [string]$OutputPath = '',
    [ValidateRange(10, 50)]
    [int]$Iterations = 10,
    [ValidateRange(1, 30)]
    [int]$IdleSeconds = 3,
    [ValidateRange(2, 60)]
    [int]$StartupTimeoutSeconds = 15,
    [switch]$EnforceThresholds
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($GpuiBinaryPath)) {
    $GpuiBinaryPath = Join-Path $repositoryRoot 'target\release\hard-vibe-code.exe'
}
if ([string]::IsNullOrWhiteSpace($TauriBinaryPath)) {
    $TauriBinaryPath = Join-Path $repositoryRoot 'target\release\hvc-tauri-legacy.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot 'target\stage6\windows-comparison.json'
}
$gpuiBinary = (Resolve-Path -LiteralPath $GpuiBinaryPath).Path
$tauriBinary = (Resolve-Path -LiteralPath $TauriBinaryPath).Path
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)

$stageRoot = Join-Path $repositoryRoot 'target\stage6'
$runtimeRoot = Join-Path $stageRoot 'benchmark-runtime'
$emptyDataset = Join-Path $stageRoot 'empty-dataset'
New-Item -ItemType Directory -Path $runtimeRoot -Force | Out-Null
New-Item -ItemType Directory -Path $emptyDataset -Force | Out-Null
New-Item -ItemType Directory -Path ([System.IO.Path]::GetDirectoryName($resolvedOutput)) -Force | Out-Null
if ([string]::IsNullOrWhiteSpace($DatasetPath)) {
    $DatasetPath = $emptyDataset
}
$dataset = (Resolve-Path -LiteralPath $DatasetPath).Path

if ($null -eq ('HvcWindowsBenchmark.NativeWindow' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace HvcWindowsBenchmark
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

function Get-ProcessTreeWorkingSetBytes([int]$RootProcessId) {
    $rows = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)
    do {
        $added = $false
        foreach ($row in $rows) {
            if ($ids.Contains([int]$row.ParentProcessId) -and $ids.Add([int]$row.ProcessId)) {
                $added = $true
            }
        }
    } while ($added)
    $total = [int64]0
    foreach ($processId in $ids) {
        $member = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($null -ne $member) {
            $member.Refresh()
            $total += $member.WorkingSet64
        }
    }
    return $total
}

function Get-Percentile([double[]]$Values, [double]$Quantile) {
    $sorted = @($Values | Sort-Object)
    $rank = [Math]::Ceiling($Quantile * $sorted.Count)
    return [double]$sorted[[Math]::Max(0, [Math]::Min($sorted.Count - 1, $rank - 1))]
}

function Invoke-BenchmarkRun([string]$Name, [string]$Binary, [int]$Index) {
    $dataDirectory = Join-Path $runtimeRoot ([guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $dataDirectory | Out-Null
    Get-ChildItem -LiteralPath $dataset -Force | Copy-Item -Destination $dataDirectory -Recurse -Force
    $previousDataDirectory = $env:HVC_DATA_DIR
    $process = $null
    try {
        $env:HVC_DATA_DIR = $dataDirectory
        $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        # O marco de cold start é uma janela Win32 realmente visível.
        $process = Start-Process -FilePath $Binary -PassThru -WindowStyle Normal
        $deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)
        $window = [IntPtr]::Zero
        $databasePath = Join-Path $dataDirectory 'hard-vibe-code.db'
        while ([DateTime]::UtcNow -lt $deadline) {
            Start-Sleep -Milliseconds 25
            $process.Refresh()
            if ($process.HasExited) {
                throw "$Name encerrou durante a inicialização (exit code $($process.ExitCode))."
            }
            $window = [HvcWindowsBenchmark.NativeWindow]::FindVisibleForProcess($process.Id)
            if ($window -ne [IntPtr]::Zero -and (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
                break
            }
        }
        $stopwatch.Stop()
        if ($window -eq [IntPtr]::Zero -or -not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
            throw "$Name não ficou pronto (janela + banco) em $StartupTimeoutSeconds segundos."
        }
        Start-Sleep -Seconds $IdleSeconds
        $workingSetBytes = Get-ProcessTreeWorkingSetBytes -RootProcessId $process.Id
        [void][HvcWindowsBenchmark.NativeWindow]::RequestClose($window)
        $gracefulShutdown = $process.WaitForExit(5000)
        if (-not $gracefulShutdown -and $Name -eq 'gpui') {
            throw "$Name não concluiu o shutdown gracioso em 5 segundos."
        }
        if (-not $gracefulShutdown) {
            Stop-Process -Id $process.Id -Force
            $process.WaitForExit()
        }
        return [pscustomobject]@{
            implementation = $Name
            iteration = $Index
            startupMs = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            idleWorkingSetBytes = $workingSetBytes
            idleWorkingSetMb = [Math]::Round($workingSetBytes / 1MB, 3)
            gracefulShutdown = $gracefulShutdown
            exitCode = $process.ExitCode
        }
    }
    finally {
        if ($null -ne $process -and -not $process.HasExited) {
            Stop-Process -Id $process.Id -Force
            $process.WaitForExit()
        }
        if ($null -eq $previousDataDirectory) {
            Remove-Item Env:HVC_DATA_DIR -ErrorAction SilentlyContinue
        }
        else {
            $env:HVC_DATA_DIR = $previousDataDirectory
        }
        $resolvedRuntimeRoot = (Resolve-Path -LiteralPath $runtimeRoot).Path
        $resolvedDataDirectory = (Resolve-Path -LiteralPath $dataDirectory).Path
        if ($resolvedDataDirectory.StartsWith(
            $resolvedRuntimeRoot + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
            Remove-Item -LiteralPath $resolvedDataDirectory -Recurse -Force
        }
    }
}

function Get-Summary([object[]]$Rows) {
    $startup = [double[]]@($Rows | ForEach-Object { $_.startupMs })
    $memory = [double[]]@($Rows | ForEach-Object { $_.idleWorkingSetMb })
    return [pscustomobject]@{
        startupP50Ms = [Math]::Round((Get-Percentile $startup 0.50), 3)
        startupP95Ms = [Math]::Round((Get-Percentile $startup 0.95), 3)
        idleMemoryP50Mb = [Math]::Round((Get-Percentile $memory 0.50), 3)
        idleMemoryP95Mb = [Math]::Round((Get-Percentile $memory 0.95), 3)
    }
}

$rows = [System.Collections.Generic.List[object]]::new()
for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
    $order = if ($iteration % 2 -eq 1) {
        @(
            [pscustomobject]@{ Name = 'tauri'; Binary = $tauriBinary }
            [pscustomobject]@{ Name = 'gpui'; Binary = $gpuiBinary }
        )
    }
    else {
        @(
            [pscustomobject]@{ Name = 'gpui'; Binary = $gpuiBinary }
            [pscustomobject]@{ Name = 'tauri'; Binary = $tauriBinary }
        )
    }
    foreach ($entry in $order) {
        $rows.Add((Invoke-BenchmarkRun -Name $entry.Name -Binary $entry.Binary -Index $iteration))
    }
}

$gpuiSummary = Get-Summary @($rows | Where-Object implementation -eq 'gpui')
$tauriSummary = Get-Summary @($rows | Where-Object implementation -eq 'tauri')
$displayRefreshRates = @(Get-CimInstance Win32_VideoController |
    Where-Object { $null -ne $_.CurrentRefreshRate -and $_.CurrentRefreshRate -gt 0 } |
    ForEach-Object { [int]$_.CurrentRefreshRate } |
    Sort-Object -Unique)
$startupReduction = 100 * (1 - $gpuiSummary.startupP50Ms / $tauriSummary.startupP50Ms)
$memoryReduction = 100 * (1 - $gpuiSummary.idleMemoryP50Mb / $tauriSummary.idleMemoryP50Mb)
$passed = $startupReduction -ge 40 -and $memoryReduction -ge 40
$report = [ordered]@{
    schemaVersion = 1
    measuredAt = [DateTime]::UtcNow.ToString('o')
    machine = [ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        processorCount = [System.Environment]::ProcessorCount
        displayRefreshRatesHz = $displayRefreshRates
    }
    dataset = $dataset
    methodology = 'Pronto = janela visível + SQLite criado; working set da árvore de processos após idle; ordem alternada; P50 usado no gate.'
    iterationsPerImplementation = $Iterations
    binaries = [ordered]@{
        gpui = [ordered]@{ path = $gpuiBinary; executableBytes = (Get-Item $gpuiBinary).Length }
        tauri = [ordered]@{ path = $tauriBinary; executableBytes = (Get-Item $tauriBinary).Length }
    }
    summaries = [ordered]@{ gpui = $gpuiSummary; tauri = $tauriSummary }
    reductions = [ordered]@{
        startupP50Percent = [Math]::Round($startupReduction, 3)
        idleMemoryP50Percent = [Math]::Round($memoryReduction, 3)
    }
    thresholdPercent = 40
    passed = $passed
    runs = $rows
}
$report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $resolvedOutput -Encoding utf8
$report | ConvertTo-Json -Depth 10
if ($EnforceThresholds -and -not $passed) {
    throw "Gate comparativo reprovado: startup=$([Math]::Round($startupReduction, 2))%; memória=$([Math]::Round($memoryReduction, 2))%."
}
