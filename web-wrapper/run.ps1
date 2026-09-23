$ErrorActionPreference = "Stop"

$WrapperDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$LogDir = Join-Path $WrapperDir "data\logs"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

$Stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$LogPath = Join-Path $LogDir "wrapper-$Stamp.log"

Push-Location $WrapperDir
try {
  npm run start *>&1 | Tee-Object -FilePath $LogPath
}
finally {
  Pop-Location
}
