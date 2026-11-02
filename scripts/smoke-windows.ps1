[CmdletBinding()]
param(
    [string]$BinaryPath = '',
    [ValidateRange(1, 60)]
    [int]$StartupTimeoutSeconds = 10
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $BinaryPath = Join-Path $repositoryRoot 'target\release\hard-vibe-code.exe'
}
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path

if ($null -eq ('HvcSmoke.NativeWindow' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace HvcSmoke
{
    public static class NativeWindow
    {
        private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);

        [DllImport("user32.dll")]
        private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);

        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetClassName(IntPtr window, StringBuilder className, int maxCount);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

        public static IntPtr FindForProcess(uint processId)
        {
            IntPtr result = IntPtr.Zero;
            EnumWindows(delegate(IntPtr window, IntPtr parameter)
            {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner == processId)
                {
                    // A debug build also owns a console; close the GPUI window.
                    var className = new StringBuilder(256);
                    if (GetClassName(window, className, className.Capacity) > 0 &&
                        className.ToString() == "Zed::Window")
                    {
                        result = window;
                        return false;
                    }
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

if (-not $resolvedBinary.StartsWith($repositoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "O binário do smoke test precisa estar dentro do workspace: $repositoryRoot"
}

$smokeRoot = Join-Path $repositoryRoot 'target\smoke-data'
New-Item -ItemType Directory -Path $smokeRoot -Force | Out-Null
$smokeDirectory = Join-Path $smokeRoot ([guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $smokeDirectory | Out-Null

$previousDataDirectory = $env:HVC_DATA_DIR
$process = $null
$gracefulShutdown = $false
$windowHandle = [IntPtr]::Zero

try {
    $env:HVC_DATA_DIR = $smokeDirectory
    $process = Start-Process -FilePath $resolvedBinary -PassThru -WindowStyle Hidden
    $deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)

    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
        if (-not $process.HasExited) {
            $windowHandle = [HvcSmoke.NativeWindow]::FindForProcess($process.Id)
        }
        if ($process.HasExited -or $windowHandle -ne [IntPtr]::Zero) {
            break
        }
    }

    if ($process.HasExited) {
        throw "O binário GPUI encerrou durante a inicialização (exit code $($process.ExitCode))."
    }
    if ($windowHandle -eq [IntPtr]::Zero) {
        throw "A janela GPUI não ficou disponível em $StartupTimeoutSeconds segundos."
    }

    $databasePath = Join-Path $smokeDirectory 'hard-vibe-code.db'
    if (-not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
        throw "O AppCore não inicializou o banco isolado esperado: $databasePath"
    }

    $closeRequested = [HvcSmoke.NativeWindow]::RequestClose($windowHandle)
    $gracefulShutdown = $closeRequested -and $process.WaitForExit(5000)
    if (-not $gracefulShutdown) {
        throw 'O binário não concluiu o shutdown gracioso dentro de 5 segundos.'
    }
    if ($process.ExitCode -ne 0) {
        throw "O binário encerrou com código inesperado: $($process.ExitCode)."
    }

    [pscustomobject]@{
        binary = $resolvedBinary
        database = $databasePath
        windowHandle = $windowHandle.ToInt64()
        gracefulShutdown = $gracefulShutdown
        exitCode = $process.ExitCode
    } | ConvertTo-Json -Compress
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

    $resolvedSmokeRoot = (Resolve-Path -LiteralPath $smokeRoot).Path
    $resolvedSmokeDirectory = (Resolve-Path -LiteralPath $smokeDirectory).Path
    if ($resolvedSmokeDirectory.StartsWith(
        $resolvedSmokeRoot + [System.IO.Path]::DirectorySeparatorChar,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        Remove-Item -LiteralPath $resolvedSmokeDirectory -Recurse -Force
    }
}
