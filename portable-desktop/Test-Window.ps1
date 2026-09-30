#Requires -Version 5.1
<# .SYNOPSIS Exercises the actual local chrome and its native controls through Windows UI Automation. #>
[CmdletBinding()]
param([string]$Shell)
$ErrorActionPreference = 'Stop'
if (-not $Shell) { $Shell = Join-Path $PSScriptRoot 'bin\portable-web-shell.exe' }
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$caseRoot = Join-Path $PSScriptRoot ('tmp\window-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $caseRoot | Out-Null
Copy-Item -LiteralPath (Get-Command node.exe).Source -Destination (Join-Path $caseRoot 'node.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'tests\backend.mjs') -Destination (Join-Path $caseRoot 'backend.mjs')
$configuration = [ordered]@{
    schemaVersion = 1; name = 'Wrapper window test'; program = 'node.exe'
    arguments = @('{root}/backend.mjs', 'ready'); workingDirectory = '.'; environment = @{}
    readinessPrefix = 'dsh web: '; requireToken = $true; startupTimeoutSeconds = 15
}
$configPath = Join-Path $caseRoot 'launch.json'
[IO.File]::WriteAllText($configPath, ($configuration | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
$process = Start-Process -FilePath $Shell -ArgumentList ('"' + $configPath + '"') -PassThru -WindowStyle Hidden
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(40)
    $root = $null
    $ready = $null
    while ([DateTime]::UtcNow -lt $deadline -and -not $process.HasExited) {
        $process.Refresh()
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
            $root = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
            $ready = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Ready'))
            if ($ready) { break }
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) {
        if ($root) { $root.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name } | Select-Object -First 30 | Write-Host }
        throw "Local chrome never reported Ready. See $caseRoot\data\logs"
    }
    $content = $null
    while ([DateTime]::UtcNow -lt $deadline -and -not $content) {
        $content = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Original application fixture'))
        if (-not $content) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $content) { throw 'The original application HTML did not render in its own WebView.' }
    $window = $root.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern)
    $expected = @('Maximized', 'Normal', 'Minimized')
    $index = 0
    foreach ($name in @('Maximize or restore', 'Maximize or restore', 'Minimize')) {
        $button = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $name))
        if (-not $button) { throw "Missing local control: $name" }
        $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 300
        if ($window.Current.WindowVisualState.ToString() -ne $expected[$index]) { throw "$name did not change the native window state." }
        $index++
        Write-Host "$name : $($window.Current.WindowVisualState)"
    }
    $second = Start-Process -FilePath $Shell -ArgumentList ('"' + $configPath + '"') -PassThru -WindowStyle Hidden
    try {
        if (-not $second.WaitForExit(10000) -or $second.ExitCode -ne 0) { throw 'A second launch did not hand off to the existing installation.' }
        $restoreDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while ($window.Current.WindowVisualState -ne [Windows.Automation.WindowVisualState]::Normal -and [DateTime]::UtcNow -lt $restoreDeadline) { Start-Sleep -Milliseconds 100 }
        if ($window.Current.WindowVisualState -ne [Windows.Automation.WindowVisualState]::Normal) { throw 'Launching the shortcut again did not restore the minimized window.' }
    } finally { if (-not $second.HasExited) { $second.Kill(); $second.WaitForExit() }; $second.Dispose() }
    $close = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Close'))
    $close.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    if (-not $process.WaitForExit(10000)) { throw 'The local close control did not exit the shell.' }
    foreach ($marker in Get-ChildItem -LiteralPath $caseRoot -Filter 'pid-*.txt') {
        $backendId = [int](Get-Content -LiteralPath $marker.FullName)
        $remaining = Get-Process -Id $backendId -ErrorAction SilentlyContinue
        if ($remaining) { $remaining | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue }
        if (Get-Process -Id $backendId -ErrorAction SilentlyContinue) { throw "Closing the native window left process $backendId running" }
    }
    Write-Host "PASS: native window ready, controls, and close cleanup. Evidence: $caseRoot"
} finally {
    if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
}
