#Requires -Version 5.1
function Expand-SourceArchive {
    <#
    .SYNOPSIS
    Extracts a source ZIP with long Windows paths while rejecting traversal and invalid Windows entry names.
    .PARAMETER Archive
    Local ZIP to read. All archive handles close on success and failure.
    .PARAMETER Destination
    New extraction folder; existing files are never overwritten.
    #>
    param([string]$Archive, [string]$Destination)
    $ErrorActionPreference = 'Stop'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $base = '\\?\' + [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
    [IO.Directory]::CreateDirectory($base) | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead([IO.Path]::GetFullPath($Archive))
    try {
        foreach ($entry in $zip.Entries) {
            $relative = $entry.FullName.Replace('/', '\')
            if ([IO.Path]::IsPathRooted($relative) -or ($relative.Split('\') -contains '..') -or $relative -match '[<>:"|?*\x00-\x1F]') { throw 'Source archive contains an unsafe Windows path.' }
            # Extended Windows paths require backslashes even though ZIP entry names use forward slashes.
            $target = $base + $relative
            if ($relative.EndsWith('\')) { [IO.Directory]::CreateDirectory($target) | Out-Null }
            else {
                [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target)
            }
        }
    } finally { $zip.Dispose() }
}
Export-ModuleMember -Function Expand-SourceArchive
