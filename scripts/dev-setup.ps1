param(
    [switch]$Template
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$DefaultActiveRepoUrl = "https://github.com/ethereum/EIPs.git"

function Say {
    param([string]$Message)

    Write-Host $Message
}

function Die {
    param([string]$Message)

    throw "error: $Message"
}

function ConvertTo-NormalizedDirectoryPath {
    param([string]$Directory)

    $trimmed = $Directory.Trim('"')
    $trimChars = [char[]]@([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)

    try {
        return ([System.IO.Path]::GetFullPath($trimmed)).TrimEnd($trimChars)
    } catch {
        return $trimmed.TrimEnd($trimChars)
    }
}

function Add-PathNote {
    param([string]$InstallDir)

    $target = ConvertTo-NormalizedDirectoryPath -Directory $InstallDir
    foreach ($pathNote in $script:PathNotes) {
        $candidate = ConvertTo-NormalizedDirectoryPath -Directory $pathNote
        if ([string]::Equals($candidate, $target, [System.StringComparison]::OrdinalIgnoreCase)) {
            return
        }
    }

    $script:PathNotes += $InstallDir
}

function Move-DirectoryToFrontOfSessionPath {
    param([string]$InstallDir)

    $target = ConvertTo-NormalizedDirectoryPath -Directory $InstallDir
    $remainingEntries = @()

    if (-not [string]::IsNullOrWhiteSpace($env:Path)) {
        foreach ($entry in ($env:Path -split ";")) {
            if ([string]::IsNullOrWhiteSpace($entry)) {
                continue
            }

            $candidate = ConvertTo-NormalizedDirectoryPath -Directory $entry
            if ([string]::Equals($candidate, $target, [System.StringComparison]::OrdinalIgnoreCase)) {
                continue
            }

            $remainingEntries += $entry
        }
    }

    $newEntries = @($InstallDir)
    if ($remainingEntries.Count -gt 0) {
        $newEntries += $remainingEntries
    }

    $updatedPath = [string]::Join(";", $newEntries)
    if (-not [string]::Equals($env:Path, $updatedPath, [System.StringComparison]::Ordinal)) {
        $env:Path = $updatedPath

        Add-PathNote -InstallDir $InstallDir
    }
}

function Find-ZolaOnPath {
    foreach ($commandName in @("zola", "zola.exe")) {
        $commands = @(Get-Command -Name $commandName -CommandType Application -ErrorAction SilentlyContinue)
        if ($commands.Count -gt 0) {
            return $commands[0].Source
        }
    }

    return $null
}

function Assert-InstallDirWritable {
    param([string]$InstallDir)

    $probePath = $null

    try {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        $probeName = ".build-eips-write-test-{0}.tmp" -f ([System.Guid]::NewGuid().ToString("N"))
        $probePath = Join-Path -Path $InstallDir -ChildPath $probeName
        [System.IO.File]::WriteAllText($probePath, "")
        Remove-Item -LiteralPath $probePath -Force
        $probePath = $null
    } catch {
        Die ("install directory cannot be created or written ({0}): {1}" -f $InstallDir, $_.Exception.Message)
    } finally {
        if (($null -ne $probePath) -and (Test-Path -LiteralPath $probePath)) {
            Remove-Item -LiteralPath $probePath -Force -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-ReleaseDownload {
    param(
        [string]$Url,
        [string]$Destination
    )

    $previousProgressPreference = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing
    } catch {
        Die ("failed to download {0}: {1}" -f $Url, $_.Exception.Message)
    } finally {
        $ProgressPreference = $previousProgressPreference
    }
}

function Assert-FileSha256 {
    param(
        [string]$ArchivePath,
        [string]$ExpectedHash,
        [string]$ArchiveName
    )

    $actualHash = (Get-FileHash -Algorithm SHA256 -Path $ArchivePath).Hash.ToLowerInvariant()
    if ($actualHash -ne $ExpectedHash.ToLowerInvariant()) {
        Die "checksum mismatch for $ArchiveName"
    }
}

function Get-ZolaReleaseAsset {
    $architecture = $env:PROCESSOR_ARCHITECTURE
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        $architecture = "unknown"
    }
    if (($architecture -eq "x86") -and (-not [string]::IsNullOrWhiteSpace($env:PROCESSOR_ARCHITEW6432))) {
        $architecture = $env:PROCESSOR_ARCHITEW6432
    }

    switch ($architecture.ToUpperInvariant()) {
        "AMD64" {
            return @{
                ArchiveName = "zola-v0.22.1-x86_64-pc-windows-msvc.zip"
                Hash = "2c8b368f5abdf2b2478748f9549a761fd6599238e18948eccb76a7cae51f5dc1"
            }
        }
        default {
            Die "Unsupported platform Windows/$architecture for automatic Zola install. Install Zola 0.22.1 manually from https://github.com/getzola/zola/releases and ensure it is on PATH."
        }
    }
}

function Install-Zola {
    param(
        [string]$InstallDir,
        [string]$ZolaPath
    )

    $asset = Get-ZolaReleaseAsset
    $archiveName = $asset.ArchiveName
    $archiveHash = $asset.Hash
    $releaseBaseUrl = "https://github.com/getzola/zola/releases/download/v0.22.1"
    $tmpRoot = Join-Path -Path ([System.IO.Path]::GetTempPath()) -ChildPath ("zola-" + [System.Guid]::NewGuid().ToString("N"))
    $archivePath = Join-Path -Path $tmpRoot -ChildPath $archiveName
    $extractDir = Join-Path -Path $tmpRoot -ChildPath "extract"

    try {
        Assert-InstallDirWritable -InstallDir $InstallDir

        New-Item -ItemType Directory -Path $extractDir -Force | Out-Null

        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

        Say "Installing zola 0.22.1 from $releaseBaseUrl/$archiveName"
        Invoke-ReleaseDownload -Url "$releaseBaseUrl/$archiveName" -Destination $archivePath
        Assert-FileSha256 -ArchivePath $archivePath -ExpectedHash $archiveHash -ArchiveName $archiveName

        Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir -Force

        $extractedZola = Join-Path -Path $extractDir -ChildPath "zola.exe"
        if (-not (Test-Path -LiteralPath $extractedZola -PathType Leaf)) {
            Die "zola release archive did not contain expected zola.exe"
        }

        try {
            Move-Item -LiteralPath $extractedZola -Destination $ZolaPath -Force
        } catch {
            Die ("zola.exe is in use. Close any running zola process and re-run this script. Details: {0}" -f $_.Exception.Message)
        }

        return $ZolaPath
    } catch {
        Die ("failed to install zola: {0}" -f $_.Exception.Message)
    } finally {
        if (Test-Path -LiteralPath $tmpRoot) {
            Remove-Item -LiteralPath $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Get-ZolaVersionInfo {
    param([string]$ZolaPath)

    try {
        $output = & $ZolaPath --version 2>$null
        if ($LASTEXITCODE -ne 0) {
            return $null
        }
    } catch {
        return $null
    }

    $fields = @(($output -join " ") -split "\s+" | Where-Object { $_.Length -gt 0 })
    if ($fields.Count -lt 2) {
        return $null
    }

    $versionToken = $fields[1]
    if ($versionToken -notmatch "^([0-9]+)\.([0-9]+)\.([0-9]+)(.*)$") {
        return $null
    }

    return @{
        VersionToken = $versionToken
        Version = [version]("{0}.{1}.{2}" -f $Matches[1], $Matches[2], $Matches[3])
        Suffix = $Matches[4]
    }
}

function Get-ZolaVersionRelation {
    param([hashtable]$VersionInfo)

    $minimumVersion = [version]"0.22.1"
    if ($VersionInfo.Version -lt $minimumVersion) {
        return "below"
    }
    if ($VersionInfo.Version -gt $minimumVersion) {
        return "newer"
    }
    if (-not [string]::IsNullOrEmpty($VersionInfo.Suffix)) {
        return "below"
    }

    return "equal"
}

function Get-DefaultInstallPaths {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        Die "LOCALAPPDATA is not set; cannot determine the user-local Zola install directory"
    }

    $installDir = Join-Path -Path (Join-Path -Path $env:LOCALAPPDATA -ChildPath "build-eips") -ChildPath "bin"
    $zolaPath = Join-Path -Path $installDir -ChildPath "zola.exe"

    return @{
        InstallDir = $installDir
        ZolaPath = $zolaPath
    }
}

function Install-PinnedZola {
    $defaultPaths = Get-DefaultInstallPaths
    $installedZola = Install-Zola -InstallDir $defaultPaths.InstallDir -ZolaPath $defaultPaths.ZolaPath
    Move-DirectoryToFrontOfSessionPath -InstallDir $defaultPaths.InstallDir

    return $installedZola
}

function Initialize-Zola {
    $zolaPath = Find-ZolaOnPath
    if ($null -eq $zolaPath) {
        $defaultPaths = Get-DefaultInstallPaths
        if (Test-Path -LiteralPath $defaultPaths.ZolaPath -PathType Leaf) {
            $zolaPath = $defaultPaths.ZolaPath
            Move-DirectoryToFrontOfSessionPath -InstallDir $defaultPaths.InstallDir
        }
    }

    if ($null -eq $zolaPath) {
        return (Install-PinnedZola)
    }

    $versionInfo = Get-ZolaVersionInfo -ZolaPath $zolaPath
    if ($null -eq $versionInfo) {
        Say "Found zola with unparseable version output. Installing zola 0.22.1."
        return (Install-PinnedZola)
    }

    $relation = Get-ZolaVersionRelation -VersionInfo $versionInfo
    switch ($relation) {
        "below" {
            Say ("Found zola {0} below supported 0.22.1. Installing zola 0.22.1." -f $versionInfo.VersionToken)
            return (Install-PinnedZola)
        }
        "equal" {
            Say ("Using existing zola {0} at {1}" -f $versionInfo.VersionToken, $zolaPath)
            return $zolaPath
        }
        "newer" {
            Say ("Found zola {0}. build-eips is tested with zola 0.22.1 or newer. Continuing with the installed version." -f $versionInfo.VersionToken)
            return $zolaPath
        }
    }
}

function ConvertTo-PowerShellQuotedPath {
    param([string]$Path)

    return "'{0}'" -f ($Path -replace "'", "''")
}

function Resolve-ConfiguredPath {
    param(
        [string]$PathValue,
        [string]$BaseDir
    )

    if ([System.IO.Path]::IsPathRooted($PathValue)) {
        $candidate = $PathValue
    } else {
        $candidate = Join-Path -Path $BaseDir -ChildPath $PathValue
    }

    try {
        return (Resolve-Path -LiteralPath $candidate).ProviderPath
    } catch {
        return [System.IO.Path]::GetFullPath($candidate)
    }
}

function Resolve-ActiveRepoRoot {
    param(
        [string]$InvocationDir,
        [string]$WorkspaceRoot
    )

    $activeRepoExplicit = $false
    if (-not [string]::IsNullOrWhiteSpace($env:ACTIVE_REPO_ROOT)) {
        $activeRepoExplicit = $true
        if ([System.IO.Path]::IsPathRooted($env:ACTIVE_REPO_ROOT)) {
            $activeRepoCandidate = $env:ACTIVE_REPO_ROOT
        } else {
            $activeRepoCandidate = Join-Path -Path $InvocationDir -ChildPath $env:ACTIVE_REPO_ROOT
        }
    } else {
        $activeRepoCandidate = Join-Path -Path $WorkspaceRoot -ChildPath "EIPs"
    }

    if ($activeRepoExplicit) {
        try {
            $resolved = (Resolve-Path -LiteralPath $activeRepoCandidate).ProviderPath
        } catch {
            Die "configured ACTIVE_REPO_ROOT does not exist: $activeRepoCandidate. Fix ACTIVE_REPO_ROOT or unset it to let this script clone EIPs."
        }

        if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
            Die "configured ACTIVE_REPO_ROOT is not a directory: $resolved. Fix ACTIVE_REPO_ROOT or unset it to let this script clone EIPs."
        }
        if (-not (Test-Path -LiteralPath (Join-Path -Path $resolved -ChildPath ".git"))) {
            Die "configured ACTIVE_REPO_ROOT is not a git checkout: $resolved. Fix ACTIVE_REPO_ROOT or unset it to let this script clone EIPs."
        }

        return @{
            Path = $resolved
            Explicit = $true
        }
    }

    if (-not (Test-Path -LiteralPath $activeRepoCandidate)) {
        $gitCommand = Get-Command git -CommandType Application -ErrorAction SilentlyContinue
        if ($null -eq $gitCommand) {
            Die "need git to clone default proposal repo from $DefaultActiveRepoUrl"
        }

        Say "No active proposal repo found at $activeRepoCandidate."
        Say "Cloning default proposal repo from $DefaultActiveRepoUrl"
        Say "This may take a few minutes..."
        & git clone $DefaultActiveRepoUrl $activeRepoCandidate
        $GitCloneExitCode = $LASTEXITCODE
        if ($GitCloneExitCode -ne 0) {
            Die "failed to clone default proposal repo from $DefaultActiveRepoUrl to $activeRepoCandidate"
        }
        Say "Cloned default proposal repo to $activeRepoCandidate"
    }

    try {
        $resolved = (Resolve-Path -LiteralPath $activeRepoCandidate).ProviderPath
    } catch {
        Die "default active proposal repo path does not exist after clone: $activeRepoCandidate"
    }

    if (-not (Test-Path -LiteralPath $resolved -PathType Container)) {
        Die "default active proposal repo path exists but is not a directory: $resolved. Remove it or set ACTIVE_REPO_ROOT."
    }
    if (-not (Test-Path -LiteralPath (Join-Path -Path $resolved -ChildPath ".git"))) {
        Die "default active proposal repo path exists but is not a git checkout: $resolved. Remove it or set ACTIVE_REPO_ROOT."
    }

    return @{
        Path = $resolved
        Explicit = $false
    }
}

$PathNotes = @()

$InvocationDir = (Get-Location).ProviderPath
$ScriptDir = (Resolve-Path -LiteralPath $PSScriptRoot).ProviderPath
$PreprocessorRoot = (Resolve-Path -LiteralPath (Split-Path -Path $ScriptDir -Parent)).ProviderPath

if (-not [string]::IsNullOrWhiteSpace($env:WORKSPACE_ROOT)) {
    $WorkspaceRoot = Resolve-ConfiguredPath -PathValue $env:WORKSPACE_ROOT -BaseDir $InvocationDir
} else {
    $WorkspaceRoot = (Resolve-Path -LiteralPath (Split-Path -Path $PreprocessorRoot -Parent)).ProviderPath
}

$ActiveRepoInfo = Resolve-ActiveRepoRoot -InvocationDir $InvocationDir -WorkspaceRoot $WorkspaceRoot
$ActiveRepoRoot = $ActiveRepoInfo["Path"]
$ActiveRepoExplicit = $ActiveRepoInfo["Explicit"]
$BuildEipsPath = Join-Path -Path $PreprocessorRoot -ChildPath "target\debug\build-eips.exe"

Say "Workspace root: $WorkspaceRoot"
Say "Active proposal repo: $ActiveRepoRoot"
Say "Local build-eips: $BuildEipsPath"
Say "If PowerShell blocks this script, run:"
Say "  powershell -ExecutionPolicy Bypass -File .\scripts\dev-setup.ps1"

$cargoCommand = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
if ($null -eq $cargoCommand) {
    Die "need cargo to build local build-eips. Install Rust from https://rustup.rs/ and re-run this script."
}

Say "Building local build-eips"
$CargoExitCode = 0
Push-Location -LiteralPath $PreprocessorRoot
try {
    & cargo build
    $CargoExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}
if ($CargoExitCode -ne 0) {
    Die "cargo build failed with exit code $CargoExitCode"
}

if (-not (Test-Path -LiteralPath $BuildEipsPath -PathType Leaf)) {
    Die "local build-eips was not built at $BuildEipsPath"
}

$ZolaPath = Initialize-Zola

Say "Bootstrapping local contributor workspace"
if ($Template) {
    & $BuildEipsPath -C $ActiveRepoRoot init $WorkspaceRoot --platform-dev --template
    $WorkspaceInitExitCode = $LASTEXITCODE
} else {
    & $BuildEipsPath -C $ActiveRepoRoot init $WorkspaceRoot --platform-dev
    $WorkspaceInitExitCode = $LASTEXITCODE
}
if ($WorkspaceInitExitCode -ne 0) {
    Die "build-eips init failed with exit code $WorkspaceInitExitCode"
}

Say "Running build-eips doctor"
& $BuildEipsPath -C $ActiveRepoRoot doctor
$WorkspaceDoctorExitCode = $LASTEXITCODE
if ($WorkspaceDoctorExitCode -ne 0) {
    Say "Warning: build-eips doctor reported issues above. Fix them before relying on local build-eips commands."
}

$WorkspaceDocPath = Join-Path -Path $WorkspaceRoot -ChildPath "WORKSPACE.md"
Say ""
if (Test-Path -LiteralPath $WorkspaceDocPath -PathType Leaf) {
    Say "Workspace docs: $WorkspaceDocPath"
} else {
    Say "Warning: workspace docs were not found at $WorkspaceDocPath after build-eips init"
}

if ($PathNotes.Count -gt 0) {
    Say ""
    Say 'Updated PATH for this PowerShell session only:'
    foreach ($pathNote in $PathNotes) {
        Say "  $pathNote"
    }
    Say "To make this permanent, add the listed directory or directories to your user Path in Windows Environment Variables."
}

$TargetDebugDir = Split-Path -Path $BuildEipsPath -Parent

Say ""
Say "Next commands:"
Say ("  cd {0}" -f (ConvertTo-PowerShellQuotedPath -Path $PreprocessorRoot))
Say "  cargo test"
Say ("  {0} -C {1} check" -f (ConvertTo-PowerShellQuotedPath -Path $BuildEipsPath), (ConvertTo-PowerShellQuotedPath -Path $ActiveRepoRoot))
Say ("  {0} -C {1} build" -f (ConvertTo-PowerShellQuotedPath -Path $BuildEipsPath), (ConvertTo-PowerShellQuotedPath -Path $ActiveRepoRoot))
Say ("  {0} -C {1} serve" -f (ConvertTo-PowerShellQuotedPath -Path $BuildEipsPath), (ConvertTo-PowerShellQuotedPath -Path $ActiveRepoRoot))
Say ""
Say "For shorter one-off local commands in this PowerShell session, run:"
Say ("  `$env:Path = {0} + ';' + `$env:Path" -f (ConvertTo-PowerShellQuotedPath -Path $TargetDebugDir))
if ($Template) {
    $TemplateRoot = Join-Path -Path $WorkspaceRoot -ChildPath "template"
    Say ""
    Say "Template check:"
    Say ("  {0} -C {1} check" -f (ConvertTo-PowerShellQuotedPath -Path $BuildEipsPath), (ConvertTo-PowerShellQuotedPath -Path $TemplateRoot))
}
