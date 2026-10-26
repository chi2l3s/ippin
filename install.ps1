$ErrorActionPreference = "Stop"
$Repo = "chi2l3s/ippin"
$InstallDir = "$env:LOCALAPPDATA\Programs\ippin"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$ReleaseUrl = "https://api.github.com/repos/$Repo/releases/latest"
$Release = Invoke-RestMethod -Uri $ReleaseUrl
$Asset = $Release.assets | Where-Object { $_.name -like "*windows_amd64.zip" } | Select-Object -First 1

$ZipPath = "$InstallDir\ippin.zip"
Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $ZipPath
Expand-Archive -Path $ZipPath -DestinationPath $InstallDir -Force
Remove-Item -Path $ZipPath

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
}

Write-Host "IPPin successfully installed to $InstallDir! Restart your terminal and run 'ippin'." -ForegroundColor Green