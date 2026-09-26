#Requires -Version 5.1
<#
.SYNOPSIS
    Runs the DSH Tauri desktop shell.

.DESCRIPTION
    Launches the built `dsh-tauri-desktop.exe` (installed by the MSI/NSIS
    installer, or a locally built binary). The shell shows a single normal
    window with the dsh web UI; it spawns and owns `dsh --profile web` in
    the background for its lifetime and stops it when the window closes.
    Use -Stop to close a running instance's window (triggering its normal
    shutdown, which stops the wrapped `dsh` process) rather than killing it.

.PARAMETER ExePath
    Path to dsh-tauri-desktop.exe. Defaults to the standard per-user install
    location, then the default WiX MSI (Program Files) location, then a
    local release build under target/release.

.PARAMETER DshCliPath
    Optional path to the `dsh` executable/script the shell should launch.
    Passed through as DSH_CLI_PATH; omit to resolve `dsh` from PATH.

.PARAMETER WebHost
    Loopback host the wrapped web profile binds to. Default 127.0.0.1.
    Aliased as -Host for convenience; $Host is a reserved PowerShell
    automatic variable, so the parameter itself cannot be named Host.

.PARAMETER Port
    Port the wrapped web profile binds to. Default 5175.

.PARAMETER Stop
    Close a running dsh-tauri-desktop.exe instance's window instead of
    starting one. This triggers the shell's own close handling, which stops
    the wrapped `dsh` process; if the window does not close within 10s, its
    whole process tree is force-stopped as a fallback.

.EXAMPLE
    .\Run-DshDesktop.ps1

.EXAMPLE
    .\Run-DshDesktop.ps1 -Port 5180 -DshCliPath 'C:\tools\dsh\dsh.cmd'

.EXAMPLE
    .\Run-DshDesktop.ps1 -Stop
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$DshCliPath,
    [Alias('Host')]
    [string]$WebHost = '127.0.0.1',
    [string]$Port = '5175',
    [switch]$Stop
)

$ErrorActionPreference = 'Stop'
$processName = 'dsh-tauri-desktop'

if ($Stop) {
    $running = Get-Process -Name $processName -ErrorAction SilentlyContinue
    if (-not $running) {
        Write-Host "$processName is not running."
        exit 0
    }
    foreach ($process in $running) {
        # CloseMainWindow sends a normal close request, so the shell's own
        # CloseRequested handler runs and stops the dsh child it owns.
        # Stop-Process -Force would skip that handler and leak the child.
        [void]$process.CloseMainWindow()
    }
    $exited = $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    $stillRunning = Get-Process -Name $processName -ErrorAction SilentlyContinue
    if ($stillRunning) {
        Write-Warning "$processName did not close within 10s; force-stopping its process tree."
        foreach ($process in $stillRunning) {
            # taskkill /T kills the shell's own dsh/cmd.exe/node subtree too;
            # Stop-Process -Force would kill only the shell itself and leak them.
            & taskkill.exe /PID $process.Id /T /F | Out-Null
        }
    }
    Write-Host "Stopped $processName."
    exit 0
}

if (-not $ExePath) {
    $installed = Join-Path $env:LOCALAPPDATA 'DeepSeek Harness\dsh-tauri-desktop.exe'
    $programFiles = Join-Path ${env:ProgramFiles} 'DeepSeek Harness\dsh-tauri-desktop.exe'
    $programFilesX86 = Join-Path ${env:ProgramFiles(x86)} 'DeepSeek Harness\dsh-tauri-desktop.exe'
    $localBuild = Join-Path $PSScriptRoot '..\target\release\dsh-tauri-desktop.exe'
    $candidates = @($installed, $programFiles, $programFilesX86)
    $found = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($found) {
        $ExePath = $found
    } elseif (Test-Path $localBuild) {
        $ExePath = (Resolve-Path $localBuild).Path
    } else {
        throw "dsh-tauri-desktop.exe not found. Install it or pass -ExePath explicitly. Checked: $($candidates -join ', ') and '$localBuild'."
    }
}

if (-not (Test-Path $ExePath)) {
    throw "ExePath '$ExePath' does not exist."
}

if (Get-Process -Name $processName -ErrorAction SilentlyContinue) {
    Write-Host "$processName is already running."
    exit 0
}

$env:DSH_WEB_HOST = $WebHost
$env:DSH_WEB_PORT = $Port
if ($DshCliPath) {
    $env:DSH_CLI_PATH = $DshCliPath
}

Start-Process -FilePath $ExePath
Write-Host "Started $processName (web profile bound to ${WebHost}:${Port}; the shell navigates its own window to dsh web's announced, authenticated URL)."
