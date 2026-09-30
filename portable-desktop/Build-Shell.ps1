#Requires -Version 5.1
<# .SYNOPSIS Builds the standalone Tauri shell once for distribution; installed users use the supplied executable. #>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$locator = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$tools = if (Test-Path $locator) { & $locator -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
if (-not $tools) {
    $answer = Read-Host 'Building the Tauri shell requires Microsoft C++ Build Tools. Enter Y to install using winget, D for the official download, or N to stop'
    if ($answer -eq 'Y' -and (Get-Command winget.exe -ErrorAction SilentlyContinue)) {
        & winget.exe install --id Microsoft.VisualStudio.2022.BuildTools --exact --accept-package-agreements --accept-source-agreements --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
        if ($LASTEXITCODE -ne 0) { throw 'C++ Build Tools installation failed.' }
    } elseif ($answer -eq 'D' -or $answer -eq 'Y') { Start-Process 'https://visualstudio.microsoft.com/visual-cpp-build-tools/'; throw 'Install the Desktop development with C++ workload, then retry.' }
    else { throw 'Build Tools installation was declined. Download the prebuilt wrapper bundle to avoid compiling the shell.' }
}
if (-not (Get-Command cargo.exe -ErrorAction SilentlyContinue)) {
    $answer = Read-Host 'Rust is required to build the shell. Enter Y to install through winget, D for the official download, or N to stop'
    if ($answer -eq 'Y' -and (Get-Command winget.exe -ErrorAction SilentlyContinue)) {
        & winget.exe install --id Rustlang.Rustup --exact --accept-package-agreements --accept-source-agreements
        if ($LASTEXITCODE -ne 0) { throw 'Rust installation failed.' }
        $env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:PATH
    } elseif ($answer -eq 'D' -or $answer -eq 'Y') { Start-Process 'https://www.rust-lang.org/tools/install'; throw 'Finish Rust installation, then run Build-Shell.ps1 again.' }
    else { throw 'Rust installation was declined. Download the prebuilt wrapper bundle to avoid compiling the shell.' }
}
Push-Location (Join-Path $PSScriptRoot 'shell')
try {
    & cargo.exe build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed. On Windows install the Microsoft Desktop development with C++ workload, then retry.' }
    New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot 'bin') | Out-Null
    Copy-Item -LiteralPath 'target\release\portable-web-shell.exe' -Destination (Join-Path $PSScriptRoot 'bin\portable-web-shell.exe') -Force
} finally { Pop-Location }
