#Requires -Version 5.1
<#
.SYNOPSIS
Install, update, verify, or launch an upstream Web application through the reusable native shell.
.PARAMETER Action
Plan reads the recipe without downloading or changing files. Install and Update prepare and verify a revision before activating it.
.PARAMETER Project
Local project recipe. The default downloads DeepSeek's official master branch.
.PARAMETER InstallRoot
Folder containing the shell, upstream revisions, runtime, caches, logs, and application data.
#>
[CmdletBinding()]
param(
    [ValidateSet('Plan', 'Install', 'Update', 'Run', 'RepairShortcut', 'Verify')]
    [string]$Action = 'Install',
    [string]$Project,
    [string]$InstallRoot,
    [string]$Ref,
    [switch]$NoShortcut
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
if (-not $Project) { $Project = Join-Path $PSScriptRoot 'projects\deepseek.json' }

function Write-JsonFile {
    param([string]$Path, [object]$Value)
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 15) + "`n", [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Invoke-Native {
    param([string]$Program, [string[]]$Arguments)
    Invoke-LoggedNative -Program $Program -Arguments $Arguments -Log $script:NativeLog
}

function Get-Json {
    param([string]$Url)
    $accept = if (([Uri]$Url).Host -eq 'api.github.com') { 'application/vnd.github+json' } else { 'application/json' }
    Invoke-RestMethod -Uri $Url -TimeoutSec 60 -Headers @{ 'User-Agent' = 'Portable-Web-Shell'; Accept = $accept }
}

function Save-Download {
    param([string]$Url, [string]$Path)
    Write-Host "Downloading $Url"
    Invoke-WebRequest -UseBasicParsing -Uri $Url -TimeoutSec 180 -OutFile $Path -Headers @{ 'User-Agent' = 'Portable-Web-Shell' }
}

Import-Module (Join-Path $PSScriptRoot 'Archive.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'Native.psm1') -Force

function Ensure-WebView {
    $locations = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, $env:LOCALAPPDATA)
    foreach ($location in $locations) {
        if ($location -and (Test-Path (Join-Path $location 'Microsoft\EdgeWebView\Application'))) { return }
    }
    $installer = Join-Path $script:Root 'runtime\MicrosoftEdgeWebview2Setup.exe'
    Save-Download 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' $installer
    $result = Start-Process -FilePath $installer -ArgumentList '/silent', '/install' -Wait -PassThru -WindowStyle Hidden
    if ($result.ExitCode -ne 0) { throw "WebView2 installation failed ($($result.ExitCode)). Run $installer to finish installing the required Windows runtime." }
}

function Ensure-BuildTools {
    $locator = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $locator) {
        $installation = & $locator -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($installation) { return }
    }
    Write-Host 'Building upstream DeepSeek from source requires Microsoft C++ Build Tools.'
    $answer = Read-Host 'Install them with winget now? Enter Y to install, D to open the official download, or N to stop'
    if ($answer -eq 'D') {
        Start-Process 'https://visualstudio.microsoft.com/visual-cpp-build-tools/'
        throw 'Install the Desktop development with C++ workload, then run Install.cmd again.'
    }
    if ($answer -ne 'Y') { throw 'Build Tools installation was declined. Install.cmd can be run again when ready.' }
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if (-not $winget) {
        Start-Process 'https://visualstudio.microsoft.com/visual-cpp-build-tools/'
        throw 'winget is unavailable. The official Build Tools download is open; install the C++ workload and rerun Install.cmd.'
    }
    Invoke-Native $winget.Source @('install', '--id', 'Microsoft.VisualStudio.2022.BuildTools', '--exact', '--accept-package-agreements', '--accept-source-agreements', '--override', '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')
}

function Ensure-Node {
    $nodeFolder = Join-Path $script:Root 'runtime\node'
    $nodeExecutable = Join-Path $nodeFolder 'node.exe'
    if (Test-Path $nodeExecutable) {
        $version = & $nodeExecutable --version
        if ($LASTEXITCODE -eq 0 -and $version -match "^v$($script:Recipe.nodeMajor)\.") { return $nodeExecutable }
        throw 'The contained Node runtime does not match this project recipe. Use a new installation root for a different Node major version.'
    }
    $releases = Get-Json 'https://nodejs.org/dist/index.json'
    $release = $null
    foreach ($candidate in $releases) {
        if ($candidate.version -match "^v$($script:Recipe.nodeMajor)\." -and $candidate.files -contains 'win-x64-zip') {
            $release = $candidate
            break
        }
    }
    if (-not $release) { throw 'No supported Windows x64 Node release was found.' }
    $archiveName = "node-$($release.version)-win-x64.zip"
    $archive = Join-Path $script:Root "runtime\$archiveName"
    $baseUrl = "https://nodejs.org/dist/$($release.version)"
    $checksums = (Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/SHASUMS256.txt").Content
    $checksumLine = $checksums -split "`n" | Where-Object { $_ -match ("^[a-f0-9]{64}\s+" + [Regex]::Escape($archiveName) + '\s*$') }
    if (@($checksumLine).Count -ne 1) { throw 'Node download checksum is missing or ambiguous.' }
    Save-Download "$baseUrl/$archiveName" $archive
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne ($checksumLine -split '\s+')[0]) { throw 'Node download checksum does not match the official release.' }
    $extraction = Join-Path $script:Root ('runtime\node-download-' + [Guid]::NewGuid().ToString('N'))
    Expand-Archive -LiteralPath $archive -DestinationPath $extraction
    Move-Item -LiteralPath (Join-Path $extraction "node-$($release.version)-win-x64") -Destination $nodeFolder
    return $nodeExecutable
}

function Set-ContainedEnvironment {
    param([string]$Node)
    $values = @{
        PATH = ((Split-Path -Parent $Node) + ';' + $env:PATH)
        npm_config_cache = (Join-Path $script:Root 'data\npm-cache')
        PNPM_CONFIG_STORE_DIR = (Join-Path $script:Root 'data\pnpm-store')
        PNPM_HOME = (Join-Path $script:Root 'runtime\pnpm-home')
        TEMP = (Join-Path $script:Root 'data\temp')
        TMP = (Join-Path $script:Root 'data\temp')
        DSH_TELEMETRY_DISABLED = '1'
        APPDATA = (Join-Path $script:Root 'data\appdata')
        LOCALAPPDATA = (Join-Path $script:Root 'data\localappdata')
        HOME = (Join-Path $script:Root 'data\home')
        USERPROFILE = (Join-Path $script:Root 'data\home')
        XDG_CACHE_HOME = (Join-Path $script:Root 'data\cache')
        ELECTRON_CACHE = (Join-Path $script:Root 'data\electron-cache')
        PLAYWRIGHT_BROWSERS_PATH = (Join-Path $script:Root 'data\playwright-browsers')
        PUPPETEER_CACHE_DIR = (Join-Path $script:Root 'data\puppeteer-cache')
        GIT_CEILING_DIRECTORIES = $script:Root
    }
    foreach ($key in $values.Keys) {
        if ($key -notin @('PATH', 'DSH_TELEMETRY_DISABLED', 'GIT_CEILING_DIRECTORIES')) { New-Item -ItemType Directory -Force -Path $values[$key] | Out-Null }
    }
    foreach ($key in $values.Keys) {
        $script:SavedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $values[$key], 'Process')
    }
}

function New-Launch {
    param([string]$Revision, [switch]$Verification)
    $data = if ($Verification) { 'data/verification' } else { 'data' }
    foreach ($directory in @('dsh-home', 'agents-home', 'workspace', 'appdata', 'localappdata', 'temp')) {
        New-Item -ItemType Directory -Force -Path (Join-Path $script:Root "$data/$directory") | Out-Null
    }
    return [ordered]@{
        schemaVersion = 1
        name = $script:Recipe.name
        program = 'runtime/node/node.exe'
        arguments = @("{root}/$script:PnpmRelative/node_modules/pnpm/bin/pnpm.cjs") + @($script:Recipe.launchArguments)
        workingDirectory = "app/versions/$Revision"
        environment = [ordered]@{
            PATH = '{root}/runtime/node;' + $env:PATH
            DSH_HOME = "{root}/$data/dsh-home"
            DSH_AGENTS_HOME = "{root}/$data/agents-home"
            DSH_TELEMETRY_DISABLED = '1'
            npm_config_cache = '{root}/data/npm-cache'
            PNPM_CONFIG_STORE_DIR = '{root}/data/pnpm-store'
            PNPM_HOME = '{root}/runtime/pnpm-home'
            APPDATA = "{root}/$data/appdata"
            LOCALAPPDATA = "{root}/$data/localappdata"
            TEMP = "{root}/$data/temp"
            TMP = "{root}/$data/temp"
            HOME = "{root}/$data/home"
            USERPROFILE = "{root}/$data/home"
            XDG_CACHE_HOME = "{root}/$data/cache"
            ELECTRON_CACHE = "{root}/$data/electron-cache"
            PLAYWRIGHT_BROWSERS_PATH = "{root}/$data/playwright-browsers"
            PUPPETEER_CACHE_DIR = "{root}/$data/puppeteer-cache"
            GIT_CEILING_DIRECTORIES = '{root}'
        }
        readinessPrefix = $script:Recipe.readinessPrefix
        requireToken = [bool]$script:Recipe.requireToken
        startupTimeoutSeconds = 90
    }
}

function Test-Launch {
    param([string]$Config)
    $result = Start-Process -FilePath (Join-Path $script:Root 'bin\portable-web-shell.exe') -ArgumentList ('"' + $Config + '" --smoke') -Wait -PassThru -WindowStyle Hidden
    if ($result.ExitCode -ne 0) { throw "The Web UI verification failed ($($result.ExitCode)); see $script:Root\data\logs. The previous active installation has been retained." }
}

function Repair-Shortcut {
    if ($NoShortcut) { return }
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) "$($script:Recipe.shortcutName).lnk"
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcutPath)
    $shortcut.TargetPath = Join-Path $script:Root 'bin\portable-web-shell.exe'
    $shortcut.Arguments = '"' + (Join-Path $script:Root 'launch.json') + '"'
    $shortcut.WorkingDirectory = $script:Root
    $shortcut.IconLocation = $shortcut.TargetPath + ',0'
    $shortcut.Save()
    Write-Host "Desktop shortcut: $shortcutPath"
}

$script:SavedEnvironment = @{}
$installLock = $null
$transcribing = $false
try {
    $script:Recipe = Get-Content -LiteralPath $Project -Raw | ConvertFrom-Json
    if ($script:Recipe.schemaVersion -ne 1 -or $script:Recipe.id -notmatch '^[a-z0-9][a-z0-9-]*$' -or $script:Recipe.repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'Invalid project recipe.' }
    if (-not $script:Recipe.PSObject.Properties['shortcutName']) { $script:Recipe | Add-Member -NotePropertyName shortcutName -NotePropertyValue $script:Recipe.name }
    if ($script:Recipe.shortcutName -isnot [string] -or [string]::IsNullOrWhiteSpace($script:Recipe.shortcutName) -or $script:Recipe.shortcutName.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0) { throw 'The project shortcut name must be a valid Windows filename.' }
    if ($Ref) { $script:Recipe.ref = $Ref }
    $script:Root = if ($InstallRoot) { [IO.Path]::GetFullPath($InstallRoot) } else { Join-Path $PSScriptRoot "installed\$($script:Recipe.id)" }
    if ($Action -eq 'Plan') {
        [pscustomobject]@{ Project = $script:Recipe.name; Upstream = $script:Recipe.repository; Ref = $script:Recipe.ref; InstallRoot = $script:Root; Actions = 'Download runtime, download upstream, build, verify Web UI, activate, create shortcut' }
        exit 0
    }
    $launchPath = Join-Path $script:Root 'launch.json'
    $shellPath = Join-Path $script:Root 'bin\portable-web-shell.exe'
    if ($Action -in @('Run', 'RepairShortcut', 'Verify')) {
        if (-not (Test-Path $launchPath) -or -not (Test-Path $shellPath)) { throw 'Run Install.cmd first, or specify the existing -InstallRoot.' }
        switch ($Action) {
            Run { Start-Process -FilePath $shellPath -ArgumentList ('"' + $launchPath + '"') -WorkingDirectory $script:Root -WindowStyle Hidden }
            RepairShortcut { Repair-Shortcut }
            Verify { Test-Launch $launchPath }
        }
        exit 0
    }
    foreach ($directory in @('runtime', 'app/versions', 'bin', 'data/logs', 'data/temp')) { New-Item -ItemType Directory -Force -Path (Join-Path $script:Root $directory) | Out-Null }
    $installLock = [IO.File]::Open((Join-Path $script:Root 'data\install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    $running = Get-Process -Name portable-web-shell -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $shellPath }
    if ($running) { throw 'Close this application before updating its installation.' }
    Start-Transcript -Path (Join-Path $script:Root ('data\logs\install-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log')) | Out-Null
    $transcribing = $true
    $script:NativeLog = Join-Path $script:Root ('data\logs\build-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log')
    $bundledShell = Join-Path $PSScriptRoot 'bin\portable-web-shell.exe'
    if (-not (Test-Path $bundledShell)) { & (Join-Path $PSScriptRoot 'Build-Shell.ps1') }
    if ($bundledShell -ne $shellPath) { Copy-Item -LiteralPath $bundledShell -Destination $shellPath -Force }
    Ensure-WebView
    if ($script:Recipe.buildTools) { Ensure-BuildTools }
    $node = Ensure-Node
    Set-ContainedEnvironment $node
    $commit = Get-Json ("https://api.github.com/repos/$($script:Recipe.repository)/commits/" + [Uri]::EscapeDataString($script:Recipe.ref))
    $revision = [string]$commit.sha
    if ($revision -notmatch '^[a-f0-9]{40}$') { throw 'GitHub returned an invalid commit identifier.' }
    $script:SavedEnvironment['DSH_CLIENT_COMMIT_HASH'] = $env:DSH_CLIENT_COMMIT_HASH
    $env:DSH_CLIENT_COMMIT_HASH = $revision
    Write-Host "Upstream revision: $revision"
    $sourceFolder = Join-Path $script:Root "app\versions\$revision"
    if (-not (Test-Path $sourceFolder)) {
        $archive = Join-Path $script:Root "data\temp\source-$revision.zip"
        Save-Download "https://codeload.github.com/$($script:Recipe.repository)/zip/$revision" $archive
        $extraction = Join-Path $script:Root ('data\temp\source-' + [Guid]::NewGuid().ToString('N'))
        Expand-SourceArchive $archive $extraction
        $folders = @(Get-ChildItem -LiteralPath $extraction -Directory)
        if ($folders.Count -ne 1) { throw 'Expected one upstream source directory in the archive.' }
        # Both targets are explicit children of this installation; no existing repository is moved.
        Move-Item -LiteralPath $folders[0].FullName -Destination $sourceFolder
    }
    $sourceManifest = Get-Content -LiteralPath (Join-Path $sourceFolder 'package.json') -Raw | ConvertFrom-Json
    if ($sourceManifest.packageManager -notmatch '^pnpm@([0-9]+\.[0-9]+\.[0-9]+)$') { throw 'The upstream package manager is unsupported by this recipe; update the recipe explicitly.' }
    $pnpmVersion = $Matches[1]
    $script:PnpmRelative = "runtime/pnpm-$pnpmVersion"
    $npmEntry = Join-Path $script:Root 'runtime\node\node_modules\npm\bin\npm-cli.js'
    Invoke-Native $node @($npmEntry, 'install', '--prefix', (Join-Path $script:Root $script:PnpmRelative), '--no-audit', '--no-fund', "pnpm@$pnpmVersion")
    $pnpmEntry = Join-Path $script:Root "$script:PnpmRelative/node_modules/pnpm/bin/pnpm.cjs"
    $relocated = $false
    $statePath = Join-Path $script:Root 'installed.json'
    if (Test-Path $statePath) { $previousState = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json; $relocated = $previousState.installationRoot -ne $script:Root }
    $dependencyState = Join-Path $script:Root 'data\dependency-root.json'
    if (Test-Path $dependencyState) { $previousDependencies = Get-Content -LiteralPath $dependencyState -Raw | ConvertFrom-Json; $relocated = $relocated -or $previousDependencies.installationRoot -ne $script:Root }
    if ($relocated -and (Test-Path (Join-Path $sourceFolder 'node_modules'))) {
        $modules = [IO.Path]::GetFullPath((Join-Path $sourceFolder 'node_modules'))
        $backup = [IO.Path]::GetFullPath((Join-Path $script:Root ('data\dependency-backups\' + $revision + '-' + [Guid]::NewGuid().ToString('N'))))
        $rootPrefix = $script:Root.TrimEnd('\') + '\'
        if (-not $modules.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase) -or -not $backup.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Dependency repair paths must remain inside this installation.' }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $backup) | Out-Null
        # Rename only the generated dependency directory; pnpm rebuilds its links without forcing other platforms.
        Move-Item -LiteralPath $modules -Destination $backup
    }
    Write-JsonFile $dependencyState ([ordered]@{ installationRoot = $script:Root })
    Push-Location $sourceFolder
    try {
        foreach ($step in $script:Recipe.installCommands) {
            $arguments = @($pnpmEntry) + @($step)
            Invoke-Native $node $arguments
        }
    }
    finally { Pop-Location }
    $verificationPath = Join-Path $script:Root 'launch.verify.json'
    Write-JsonFile $verificationPath (New-Launch $revision -Verification)
    Test-Launch $verificationPath
    Write-JsonFile (Join-Path $script:Root 'installed.json') ([ordered]@{ repository = $script:Recipe.repository; ref = $script:Recipe.ref; commit = $revision; version = $sourceManifest.version; installationRoot = $script:Root; node = (& $node --version); pnpm = $pnpmVersion })
    Write-JsonFile $launchPath (New-Launch $revision)
    Repair-Shortcut
    Write-Host "$($script:Recipe.name) is ready. Run it from the desktop shortcut or Run.cmd."
} catch {
    Write-Error -Message $_ -ErrorAction Continue
    Write-Host $_.ScriptStackTrace
    exit 1
} finally {
    foreach ($key in $script:SavedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($key, $script:SavedEnvironment[$key], 'Process') }
    if ($transcribing) { Stop-Transcript | Out-Null }
    if ($installLock) { $installLock.Dispose() }
}
