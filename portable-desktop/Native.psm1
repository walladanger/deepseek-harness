#Requires -Version 5.1
function Invoke-LoggedNative {
    <#
    .SYNOPSIS
    Records native stdout and stderr while rejecting nonzero exit codes. Stderr alone is not failure.
    .PARAMETER Program
    Native executable to run.
    .PARAMETER Arguments
    Literal argument array; no command-shell concatenation.
    .PARAMETER Log
    Owned diagnostic file that receives UTF-8 command output.
    #>
    param([string]$Program, [string[]]$Arguments, [string]$Log)
    if (-not [IO.File]::Exists($Program)) { throw "Native executable is missing: $Program" }
    # Windows PowerShell turns redirected native stderr into error records; the exit code owns failure.
    $ErrorActionPreference = 'Continue'
    function Write-NativeLine {
        param([object]$Record)
        $ErrorActionPreference = 'Stop'
        $line = $Record.ToString()
        [IO.File]::AppendAllText($Log, $line + "`n", [Text.UTF8Encoding]::new($false))
        Write-Host $line
    }
    & $Program @Arguments 2>&1 | ForEach-Object {
        Write-NativeLine $_
    }
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE; see $Log" }
}
Export-ModuleMember -Function Invoke-LoggedNative
