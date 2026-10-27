# Offline installer tests. Never download a release or update the user's PATH.
$ErrorActionPreference = 'Stop'
$ProjectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$TestRoot = Join-Path (Join-Path $ProjectRoot 'target') ('installer-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TestRoot | Out-Null
$Fixture = Join-Path $TestRoot 'fixture'
New-Item -ItemType Directory -Path $Fixture | Out-Null
Set-Content -LiteralPath (Join-Path $Fixture 'ippin.exe') -Value 'fixture-binary'
$FixtureZip = Join-Path $TestRoot 'archive.zip'
Compress-Archive -Path "$Fixture/*" -DestinationPath $FixtureZip
$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$AssetName = if ($Arch -eq 'ARM64') { 'ippin_windows_arm64.zip' } else { 'ippin_windows_amd64.zip' }
$FixtureHash = Join-Path $TestRoot 'checksums.txt'
$Hash = (Get-FileHash -LiteralPath $FixtureZip -Algorithm SHA256).Hash
$Release = @{ tag_name = 'v2.0.0'; assets = @(@{ name = $AssetName; browser_download_url = 'fixture-archive' }, @{ name = 'checksums.txt'; browser_download_url = 'fixture-hash' }) }
function Invoke-RestMethod {
    param($Uri, $Headers)
    if ($Uri -eq 'https://raw.githubusercontent.com/chi2l3s/ippin/main/install.ps1') {
        return Get-Content -LiteralPath $Installer -Raw
    }
    return $Release
}
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing)
    if ($Uri -eq 'fixture-archive') { Copy-Item -LiteralPath $FixtureZip -Destination $OutFile }
    elseif ($Uri -eq 'fixture-hash') { Copy-Item -LiteralPath $FixtureHash -Destination $OutFile }
    else { throw "Unexpected network request: $Uri" }
}
function Assert-True { param($Condition, $Message) if (-not $Condition) { throw $Message } }
function Assert-Failure {
    param([scriptblock]$Action, $Message)
    $Failed = $false
    try { & $Action } catch { $Failed = $true }
    Assert-True $Failed $Message
}
$Installer = Join-Path $ProjectRoot 'install.ps1'
$Destination = Join-Path $TestRoot 'installed'
$PathBefore = [Environment]::GetEnvironmentVariable('Path', 'User')
$SessionPathBefore = $env:Path
$ProgressBefore = $ProgressPreference
try {
    Set-Content -LiteralPath $FixtureHash -Value "$Hash  $AssetName"
    & $Installer -InstallDir $Destination -NoPathUpdate
    $Installed = Join-Path $Destination 'ippin.exe'
    Assert-True ((Get-Content -LiteralPath $Installed -Raw).Trim() -eq 'fixture-binary') 'Verified archive was not installed'

    Set-Content -LiteralPath $Installed -Value 'old-binary'
    Set-Content -LiteralPath $FixtureHash -Value ((('0' * 64) + '  ' + $AssetName))
    Assert-Failure { & $Installer -InstallDir $Destination -NoPathUpdate } 'Incorrect SHA256 must fail'
    Assert-True ((Get-Content -LiteralPath $Installed -Raw).Trim() -eq 'old-binary') 'Checksum failure changed existing binary'

    Set-Content -LiteralPath $FixtureHash -Value @("$Hash  $AssetName", "$Hash  $AssetName")
    Assert-Failure { & $Installer -InstallDir $Destination -NoPathUpdate } 'Duplicate checksum must fail'
    Set-Content -LiteralPath $FixtureHash -Value "$Hash  $AssetName"
    & $Installer -InstallDir $Destination -NoPathUpdate
    Assert-True ((Get-Content -LiteralPath $Installed -Raw).Trim() -eq 'fixture-binary') 'Atomic upgrade failed'

    # The downloaded script also accepts parameters as a ScriptBlock, without a local file.
    & ([scriptblock]::Create((Invoke-RestMethod 'https://raw.githubusercontent.com/chi2l3s/ippin/main/install.ps1'))) -InstallDir $Destination -NoPathUpdate
    Assert-True ((Get-Content -LiteralPath $Installed -Raw).Trim() -eq 'fixture-binary') 'Downloaded script invocation failed'
    # Exercise the exact one-line irm | iex form; fail at checksum verification
    # before it can write to the default install directory or update PATH.
    Set-Content -LiteralPath $FixtureHash -Value ((('0' * 64) + '  ' + $AssetName))
    $PipelineError = $null
    try { Invoke-RestMethod 'https://raw.githubusercontent.com/chi2l3s/ippin/main/install.ps1' | Invoke-Expression }
    catch { $PipelineError = $_.Exception.Message }
    Assert-True ($PipelineError -like '*SHA256 mismatch*') 'irm | iex must reach archive verification'
    Assert-True ($ProgressPreference -eq $ProgressBefore) 'Pipeline install changed ProgressPreference'

    $Release.tag_name = 'v1.1.0'
    Assert-Failure { & $Installer -InstallDir $Destination -NoPathUpdate } 'Legacy Go release must not be installed'
    $Release.tag_name = 'v2.0.0'
    $Release.assets = @()
    Assert-Failure { & $Installer -InstallDir $Destination -NoPathUpdate } 'Missing release assets must fail'
    Assert-True ([Environment]::GetEnvironmentVariable('Path', 'User') -eq $PathBefore) 'Tests changed user PATH'
    Assert-True ($env:Path -eq $SessionPathBefore) 'Tests changed current-session PATH'
    Write-Output 'Installer: 8 scenarios passed (install, hash failure, duplicate hash, upgrade, downloaded script, irm | iex, legacy release, missing asset).'
} finally {
    $AllowedRoot = [IO.Path]::GetFullPath((Join-Path $ProjectRoot 'target')).TrimEnd('\') + '\'
    $ResolvedTestRoot = [IO.Path]::GetFullPath($TestRoot)
    if (-not $ResolvedTestRoot.StartsWith($AllowedRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe test cleanup target' }
    Remove-Item -LiteralPath $ResolvedTestRoot -Recurse -Force
}
