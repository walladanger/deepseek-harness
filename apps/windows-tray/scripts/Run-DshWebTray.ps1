#Requires -Version 5.1
<#
.SYNOPSIS
    Runs the DSH Web tray wrapper with no visible window or console.

.DESCRIPTION
    Launches the built `dsh-windows-tray.exe` (installed by the MSI/NSIS
    installer, or a locally built binary) fully hidden: no console, no
    taskbar window. The wrapper itself spawns `dsh --profile web` in the
    background and exposes only a tray icon. Use -Stop to terminate a
    running instance.

.PARAMETER ExePath
    Path to dsh-windows-tray.exe. Defaults to the standard per-user install
    location, falling back to a local release build under target/release.

.PARAMETER DshCliPath
    Optional path to the `dsh` executable/script the wrapper should launch.
    Passed through as DSH_CLI_PATH; omit to resolve `dsh` from PATH.

.PARAMETER Host
    Loopback host the wrapped web profile binds to. Default 127.0.0.1.

.PARAMETER Port
    Port the wrapped web profile binds to. Default 5175.

.PARAMETER Stop
    Stop a running dsh-windows-tray.exe instance instead of starting one.

.EXAMPLE
    .\Run-DshWebTray.ps1

.EXAMPLE
    .\Run-DshWebTray.ps1 -Port 5180 -DshCliPath 'C:\tools\dsh\dsh.cmd'

.EXAMPLE
    .\Run-DshWebTray.ps1 -Stop
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$DshCliPath,
    [string]$Host = '127.0.0.1',
    [string]$Port = '5175',
    [switch]$Stop
)

$ErrorActionPreference = 'Stop'
$processName = 'dsh-windows-tray'

if ($Stop) {
    $running = Get-Process -Name $processName -ErrorAction SilentlyContinue
    if (-not $running) {
        Write-Host "$processName is not running."
        exit 0
    }
    $running | Stop-Process -Force
    Write-Host "Stopped $processName."
    exit 0
}

if (-not $ExePath) {
    $installed = Join-Path $env:LOCALAPPDATA 'DSH Web Tray\dsh-windows-tray.exe'
    $localBuild = Join-Path $PSScriptRoot '..\target\release\dsh-windows-tray.exe'
    if (Test-Path $installed) {
        $ExePath = $installed
    } elseif (Test-Path $localBuild) {
        $ExePath = (Resolve-Path $localBuild).Path
    } else {
        throw "dsh-windows-tray.exe not found. Install it or pass -ExePath explicitly. Checked: '$installed' and '$localBuild'."
    }
}

if (-not (Test-Path $ExePath)) {
    throw "ExePath '$ExePath' does not exist."
}

if (Get-Process -Name $processName -ErrorAction SilentlyContinue) {
    Write-Host "$processName is already running."
    exit 0
}

$env:DSH_WEB_HOST = $Host
$env:DSH_WEB_PORT = $Port
if ($DshCliPath) {
    $env:DSH_CLI_PATH = $DshCliPath
}

# WindowStyle Hidden keeps the wrapper (and, transitively, the dsh web
# process it spawns) off the taskbar and without a console window.
Start-Process -FilePath $ExePath -WindowStyle Hidden
Write-Host "Started $processName (web profile at http://${Host}:${Port})."
