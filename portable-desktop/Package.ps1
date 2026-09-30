#Requires -Version 5.1
<# .SYNOPSIS Packages only the compiled shell and redistributable installer files, excluding installations and credentials. #>
[CmdletBinding()]
param([string]$Destination, [string]$Archive)
$ErrorActionPreference = 'Stop'
if (-not $Destination) { $Destination = Join-Path $PSScriptRoot ('tmp\bundle-' + [Guid]::NewGuid().ToString('N')) }
if (-not $Archive) { $Archive = Join-Path $PSScriptRoot 'portable-desktop-windows-x64.zip' }
if (Test-Path $Destination) { throw 'Choose a new packaging destination; existing folders are not overwritten.' }
if (-not (Test-Path (Join-Path $PSScriptRoot 'bin\portable-web-shell.exe'))) { throw 'Build-Shell.ps1 must succeed before packaging.' }
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
foreach ($pattern in @('*.cmd', '*.ps1', '*.psm1', 'README*.md', 'LICENSE', 'THIRD-PARTY-NOTICES.txt')) {
    Get-ChildItem -Path (Join-Path $PSScriptRoot $pattern) -File | Copy-Item -Destination $Destination
}
foreach ($directory in @('bin', 'projects', 'tests')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $directory) -Destination $Destination -Recurse
}
Compress-Archive -Path (Join-Path $Destination '*') -DestinationPath $Archive -Force
Write-Host "Bundle: $Destination"
Write-Host "ZIP: $Archive"
