param(
    [string]$InstallDir = $env:LOG_SCOUTER_INSTALL_DIR,
    [switch]$Purge,
    [switch]$KeepPath
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\log-scouter\bin"
}

$binary = Join-Path $InstallDir "logscout.exe"
$legacyBinary = Join-Path $InstallDir "scout.exe"

if (Test-Path $binary) {
    Remove-Item -Force $binary
    Write-Host "Removed $binary"
}
else {
    Write-Host "$binary was not installed"
}

if (Test-Path $legacyBinary) {
    Remove-Item -Force $legacyBinary
    Write-Host "Removed legacy $legacyBinary"
}

if (-not $KeepPath) {
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not [string]::IsNullOrWhiteSpace($current)) {
        $normalized = $InstallDir.TrimEnd('\')
        $parts = $current -split ';' | Where-Object {
            -not [string]::IsNullOrWhiteSpace($_) -and
            -not [string]::Equals($_.TrimEnd('\'), $normalized, [StringComparison]::OrdinalIgnoreCase)
        }
        $newPath = $parts -join ';'
        if ($newPath -ne $current) {
            [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
            Write-Host "Removed $InstallDir from the user PATH"
        }
    }
}

if ($Purge) {
    $homeDir = if ($env:HOME) { $env:HOME } else { $env:USERPROFILE }
    $userData = Join-Path $homeDir ".log-scouter"
    if (Test-Path $userData) {
        Remove-Item -Recurse -Force $userData
        Write-Host "Removed $userData"
    }
    else {
        Write-Host "$userData was not present"
    }
}
