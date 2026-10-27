[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\ippin",
    [switch]$NoPathUpdate
)

$ErrorActionPreference = 'Stop'
$Repo = 'chi2l3s/ippin'
function Write-InstallStep {
    param([int]$Step, [string]$Message)
    Write-Host ('  [{0}/4] ' -f $Step) -ForegroundColor Cyan -NoNewline
    Write-Host $Message
}
Write-Host ''
Write-Host '  IPPin' -ForegroundColor Cyan -NoNewline
Write-Host ' / Windows installer' -ForegroundColor DarkGray
Write-Host ''
if ($env:OS -ne 'Windows_NT') { throw 'This installer supports Windows only. Use cargo install --path . on other platforms.' }
if ($Version -ne 'latest' -and $Version -notmatch '^v\d+\.\d+\.\d+([.-][A-Za-z0-9.-]+)?$') { throw 'Version must be latest or a version tag such as v2.0.0.' }

$NativeArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$Arch = switch ($NativeArch) {
    'AMD64' { 'amd64' }
    'ARM64' { 'arm64' }
    default { throw "Unsupported Windows architecture: $NativeArch" }
}
$ReleaseUrl = if ($Version -eq 'latest') { "https://api.github.com/repos/$Repo/releases/latest" } else { "https://api.github.com/repos/$Repo/releases/tags/$Version" }
Write-InstallStep 1 "Finding release ($Version / $Arch)"
$Release = Invoke-RestMethod -Uri $ReleaseUrl -Headers @{ 'User-Agent' = 'ippin-installer' }
$ReleaseTag = [regex]::Match([string]$Release.tag_name, '^v?([0-9]+)\.([0-9]+)\.([0-9]+)(?:[-+][0-9A-Za-z.-]+)?$')
if (-not $ReleaseTag.Success -or [long]$ReleaseTag.Groups[1].Value -lt 2) {
    throw 'A Rust release (v2.0.0 or newer) is required. Publish the Rust release first, or select one with -Version v2.0.0.'
}
$AssetName = "ippin_windows_$Arch.zip"
$Asset = @($Release.assets | Where-Object { $_.name -eq $AssetName })
$Checksums = @($Release.assets | Where-Object { $_.name -eq 'checksums.txt' })
if ($Asset.Count -ne 1 -or $Checksums.Count -ne 1) { throw "Release does not contain $AssetName and checksums.txt. Install a Rust release (v2.0.0 or newer)." }

$WorkDir = Join-Path ([IO.Path]::GetTempPath()) ('ippin-install-' + [Guid]::NewGuid().ToString('N'))
$WorkDir = [IO.Path]::GetFullPath($WorkDir)
$TempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
if (-not $WorkDir.StartsWith($TempRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid temporary directory.' }
New-Item -ItemType Directory -Path $WorkDir | Out-Null
try {
    $PreviousProgressPreference = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    $ZipPath = Join-Path $WorkDir $AssetName
    $HashPath = Join-Path $WorkDir 'checksums.txt'
    Write-InstallStep 2 "Downloading $AssetName"
    Invoke-WebRequest -UseBasicParsing -Uri $Asset[0].browser_download_url -OutFile $ZipPath
    Invoke-WebRequest -UseBasicParsing -Uri $Checksums[0].browser_download_url -OutFile $HashPath
    Write-InstallStep 3 'Verifying SHA256'
    $Pattern = '^([a-fA-F0-9]{64})\s+\*?' + [regex]::Escape($AssetName) + '$'
    $HashLines = @(Get-Content -LiteralPath $HashPath | Where-Object { $_ -match $Pattern })
    if ($HashLines.Count -ne 1) { throw 'Archive checksum missing or ambiguous.' }
    $ExpectedHash = [regex]::Match($HashLines[0], $Pattern).Groups[1].Value
    $ActualHash = (Get-FileHash -LiteralPath $ZipPath -Algorithm SHA256).Hash
    if ($ActualHash -ne $ExpectedHash) { throw 'SHA256 mismatch; nothing has been installed.' }

    $ExtractDir = Join-Path $WorkDir 'extracted'
    Expand-Archive -LiteralPath $ZipPath -DestinationPath $ExtractDir
    $Binary = Join-Path $ExtractDir 'ippin.exe'
    if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { throw 'Archive does not contain ippin.exe.' }
    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    Write-InstallStep 4 "Installing to $InstallDir"
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $TargetBinary = Join-Path $InstallDir 'ippin.exe'
    $StagedBinary = Join-Path $InstallDir ('ippin-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        Copy-Item -LiteralPath $Binary -Destination $StagedBinary
        if (Test-Path -LiteralPath $TargetBinary) { [IO.File]::Replace($StagedBinary, $TargetBinary, [System.Management.Automation.Language.NullString]::Value) }
        else { [IO.File]::Move($StagedBinary, $TargetBinary) }
    } finally {
        if (Test-Path -LiteralPath $StagedBinary) { Remove-Item -LiteralPath $StagedBinary -Force }
    }
    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $Entries = @($UserPath -split ';' | Where-Object { $_ })
    if (-not $NoPathUpdate -and -not ($Entries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        [Environment]::SetEnvironmentVariable('Path', (($Entries + $InstallDir) -join ';'), 'User')
    }
    if (-not $NoPathUpdate) {
        $CurrentEntries = @($env:Path -split ';' | Where-Object { $_ })
        if (-not ($CurrentEntries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
            $env:Path = (($CurrentEntries + $InstallDir) -join ';')
        }
    }
    Write-Host ''
    Write-Host '  Installed successfully.' -ForegroundColor Green
    Write-Host "  $TargetBinary" -ForegroundColor DarkGray
    Write-Host ''
    Write-Host '  Next steps (with your VPN enabled):' -ForegroundColor Cyan
    Write-Host '    ippin mode http://127.0.0.1:2080'
    Write-Host '    ippin lock'
    Write-Host '    ippin service install'
    Write-Host ''
    Write-Host '  Set the mode to your actual VPN proxy port, or use: ippin mode tun' -ForegroundColor DarkGray
} finally {
    $ProgressPreference = $PreviousProgressPreference
    # WorkDir is the verified, unique child of the temporary root created above.
    if (Test-Path -LiteralPath $WorkDir) { Remove-Item -LiteralPath $WorkDir -Recurse -Force }
}
