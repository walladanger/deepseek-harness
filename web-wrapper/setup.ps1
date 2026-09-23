param(
  [switch]$SkipHarnessInstall,
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$WrapperDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoDir = Split-Path -Parent $WrapperDir
$ShortcutPath = Join-Path ([Environment]::GetFolderPath("Desktop")) "DeepSeek Harness Web Wrapper.lnk"
$RunScript = Join-Path $WrapperDir "run.ps1"

function Invoke-Step {
  param(
    [string]$Name,
    [scriptblock]$Script
  )

  Write-Host ""
  Write-Host $Name
  & $Script
}

Invoke-Step "Preparing wrapper data folders" {
  New-Item -ItemType Directory -Force -Path (Join-Path $WrapperDir "data") | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $WrapperDir "data\dsh-home") | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $WrapperDir "data\agents-home") | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $WrapperDir "data\logs") | Out-Null
}

Invoke-Step "Installing wrapper dependencies" {
  Push-Location $WrapperDir
  try {
    npm install
    npm run build
  }
  finally {
    Pop-Location
  }
}

if (-not $SkipHarnessInstall) {
  Invoke-Step "Installing Harness workspace dependencies" {
    Push-Location $RepoDir
    try {
      npx pnpm@11.7.0 install
    }
    finally {
      Pop-Location
    }
  }
}

if (-not $SkipBuild) {
  Invoke-Step "Building Harness library artifacts" {
    Push-Location $RepoDir
    try {
      npx pnpm@11.7.0 run build:lib
    }
    finally {
      Pop-Location
    }
  }

  Invoke-Step "Building Harness web assets" {
    Push-Location $RepoDir
    try {
      npx pnpm@11.7.0 run build:web
    }
    finally {
      Pop-Location
    }
  }
}

Invoke-Step "Creating desktop shortcut" {
  $Shell = New-Object -ComObject WScript.Shell
  $Shortcut = $Shell.CreateShortcut($ShortcutPath)
  $Shortcut.TargetPath = "powershell.exe"
  $Shortcut.Arguments = "-ExecutionPolicy Bypass -NoProfile -File `"$RunScript`""
  $Shortcut.WorkingDirectory = $WrapperDir
  $Shortcut.IconLocation = "powershell.exe,0"
  $Shortcut.Save()
}

Write-Host ""
Write-Host "DeepSeek Harness Web Wrapper is ready."
Write-Host "Shortcut: $ShortcutPath"
