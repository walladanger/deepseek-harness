#Requires -Version 5.1
<#
.SYNOPSIS
    Runs the DSH Tauri desktop shell.

.DESCRIPTION
    Launches the built `dsh-tauri-desktop.exe` (installed by the MSI/NSIS
    installer, or a locally built binary). The shell shows a single normal
    window with the dsh web UI; it spawns and owns `dsh --profile web` in
    the background for its lifetime and stops it when the window closes.
    Use -Stop to terminate a running instance without closing its window.

.PARAMETER ExePath
    Path to dsh-tauri-desktop.exe. Defaults to the standard per-user install
    location, falling back to a local release build under target/release.

.PARAMETER DshCliPath
    Optional path to the `dsh` executable/script the shell should launch.
    Passed through as DSH_CLI_PATH; omit to resolve `dsh` from PATH.

.PARAMETER Host
    Loopback host the wrapped web profile binds to. Default 127.0.0.1.

.PARAMETER Port
    Port the wrapped web profile binds to. Default 5175.

.PARAMETER Stop
    Stop a running dsh-tauri-desktop.exe instance instead of starting one.

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
$processName = 'dsh-tauri-desktop'

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
    $installed = Join-Path $env:LOCALAPPDATA 'DeepSeek Harness\dsh-tauri-desktop.exe'
    $localBuild = Join-Path $PSScriptRoot '..\target\release\dsh-tauri-desktop.exe'
    if (Test-Path $installed) {
        $ExePath = $installed
    } elseif (Test-Path $localBuild) {
        $ExePath = (Resolve-Path $localBuild).Path
    } else {
        throw "dsh-tauri-desktop.exe not found. Install it or pass -ExePath explicitly. Checked: '$installed' and '$localBuild'."
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

Start-Process -FilePath $ExePath
Write-Host "Started $processName (web profile at http://${Host}:${Port})."
