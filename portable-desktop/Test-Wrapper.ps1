#Requires -Version 5.1
<# .SYNOPSIS Checks authentication, fragmented readiness output, timeout, early exit, and descendant cleanup through the compiled executable. #>
[CmdletBinding()]
param([string]$Shell)
$ErrorActionPreference = 'Stop'
if (-not $Shell) { $Shell = Join-Path $PSScriptRoot 'bin\portable-web-shell.exe' }
$testRoot = Join-Path $PSScriptRoot ('tmp\test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $testRoot | Out-Null
$node = (Get-Command node.exe -ErrorAction Stop).Source
$passed = 0
foreach ($mode in @('ready', 'denied', 'timeout', 'early-exit')) {
    $caseRoot = Join-Path $testRoot $mode
    New-Item -ItemType Directory -Force -Path $caseRoot | Out-Null
    Copy-Item -LiteralPath $node -Destination (Join-Path $caseRoot 'node.exe')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'tests\backend.mjs') -Destination (Join-Path $caseRoot 'backend.mjs')
    $configuration = [ordered]@{
        schemaVersion = 1; name = 'Wrapper behavior test'; program = 'node.exe'
        arguments = @('{root}/backend.mjs', $mode); workingDirectory = '.'; environment = @{ WRAPPER_TEST_HOME = '{root}/owned-home' }
        readinessPrefix = 'dsh web: '; requireToken = $true; startupTimeoutSeconds = 3
    }
    $configPath = Join-Path $caseRoot 'launch.json'
    [IO.File]::WriteAllText($configPath, ($configuration | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    $process = Start-Process -FilePath $Shell -ArgumentList ('"' + $configPath + '" --smoke') -Wait -PassThru -WindowStyle Hidden
    if (($mode -eq 'ready') -ne ($process.ExitCode -eq 0)) { throw "$mode returned unexpected exit code $($process.ExitCode). See $caseRoot\data\logs" }
    $pidFiles = @(Get-ChildItem -LiteralPath $caseRoot -Filter 'pid-*.txt')
    if ($pidFiles.Count -ne 2) { throw "$mode did not start both fixture processes; a startup failure cannot establish cleanup behavior." }
    foreach ($pidFile in $pidFiles) {
        $backendId = [int](Get-Content -LiteralPath $pidFile.FullName)
        $remaining = Get-Process -Id $backendId -ErrorAction SilentlyContinue
        if ($remaining) {
            $remaining | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
            if (Get-Process -Id $backendId -ErrorAction SilentlyContinue) { throw "Owned process $backendId survived $mode" }
        }
    }
    if ($mode -eq 'ready') {
        $logs = Get-ChildItem -LiteralPath (Join-Path $caseRoot 'data\logs') -File
        if (Select-String -LiteralPath $logs.FullName -SimpleMatch 'fixture-token') { throw 'Launch token leaked into the backend log.' }
    }
    $passed++
    Write-Host "PASS: $mode (HTTP behavior and owned process cleanup)"
}
Write-Host "$passed executable behavior checks passed. Test evidence: $testRoot"
