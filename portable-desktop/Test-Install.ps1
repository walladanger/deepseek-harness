#Requires -Version 5.1
<# .SYNOPSIS Checks Windows long ZIP paths, traversal rejection, and archive handle disposal through the installer module. #>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Archive.psm1') -Force
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$root = Join-Path $PSScriptRoot ('tmp\archive-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $root | Out-Null
$longName = ('a' * 80) + '/' + ('b' * 80) + '/' + ('c' * 80) + '/original.txt'
$names = @($longName, '../outside.txt', 'C:/escape.txt', 'file.txt:stream')
for ($index = 0; $index -lt $names.Count; $index++) {
    $archivePath = Join-Path $root "input-$index.zip"
    $archive = [IO.Compression.ZipFile]::Open($archivePath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $writer = [IO.StreamWriter]::new($archive.CreateEntry($names[$index]).Open())
        try { $writer.Write('source content') } finally { $writer.Dispose() }
    } finally { $archive.Dispose() }
    $destination = Join-Path $root "output-$index"
    $rejected = $false
    try { Expand-SourceArchive $archivePath $destination }
    catch {
        if ($_.Exception.Message -notlike '*unsafe Windows path*') { throw }
        $rejected = $true
    }
    if ($index -eq 0) {
        $file = '\\?\' + (Join-Path $destination $longName.Replace('/', '\'))
        if ($rejected -or [IO.File]::ReadAllText($file) -ne 'source content') { throw 'Long Windows source paths did not extract.' }
    } elseif (-not $rejected) { throw "Unsafe archive entry was accepted: $($names[$index])" }
    $handle = [IO.File]::Open($archivePath, 'Open', 'ReadWrite', 'None')
    $handle.Dispose()
    Write-Host "PASS: archive case $index and exclusive handle release"
}
if (Test-Path (Join-Path $root 'outside.txt')) { throw 'The archive escaped its extraction directory.' }
Import-Module (Join-Path $PSScriptRoot 'Native.psm1') -Force
$node = (Get-Command node.exe -ErrorAction Stop).Source
$nativeLog = Join-Path $root 'native.log'
$fixture = Join-Path $root 'native fixture.js'
[IO.File]::WriteAllText($fixture, 'process.stdout.write("stdout evidence\n");process.stderr.write("stderr evidence\n");if(process.argv[2]==="fail"){process.stderr.write("nonzero evidence\n");process.exit(7)}')
Invoke-LoggedNative $node @($fixture) $nativeLog
$failed = $false
try { Invoke-LoggedNative $node @($fixture, 'fail') $nativeLog }
catch { if ($_.Exception.Message -notlike '*exit code 7*') { throw }; $failed = $true }
$output = [IO.File]::ReadAllText($nativeLog)
if (-not $failed -or $output -notmatch 'stdout evidence' -or $output -notmatch 'stderr evidence' -or $output -notmatch 'nonzero evidence') { throw 'Native exit checks or diagnostic capture failed.' }
Write-Host 'PASS: native stdout, stderr, and nonzero exit behavior'
# The expected native exit 7 was asserted above; callers must receive the suite's successful exit.
exit 0
