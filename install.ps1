param(
    [string]$Version = $env:LOG_SCOUTER_VERSION,
    [string]$Repo = $env:LOG_SCOUTER_REPO,
    [string]$InstallDir = $env:LOG_SCOUTER_INSTALL_DIR,
    [string]$Proxy = $env:LOG_SCOUTER_PROXY,
    [switch]$FromSource,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Version)) { $Version = "latest" }
if ([string]::IsNullOrWhiteSpace($Repo)) { $Repo = "mangosteen-lab/log-scouter" }
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\log-scouter\bin"
}

$BinName = "logscout.exe"
$LegacyBinName = "scout.exe"
$AppName = "log-scouter"

function Invoke-LogScouterDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $parameters = @{
        Uri = $Uri
        OutFile = $OutFile
    }
    if ($PSVersionTable.PSVersion.Major -lt 6) {
        $parameters.UseBasicParsing = $true
    }
    if (-not [string]::IsNullOrWhiteSpace($Proxy)) {
        $parameters.Proxy = $Proxy
    }
    Invoke-WebRequest @parameters
}

function Get-LogScouterTarget {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch.ToString()) {
        "X64" { return "x86_64-pc-windows-msvc" }
        "Arm64" { return "aarch64-pc-windows-msvc" }
        default { throw "Unsupported Windows architecture: $arch" }
    }
}

function Get-ReleaseUrl {
    param([Parameter(Mandatory = $true)][string]$Asset)

    if ($Version -eq "latest") {
        return "https://github.com/$Repo/releases/latest/download/$Asset"
    }
    return "https://github.com/$Repo/releases/download/$Version/$Asset"
}

function Add-InstallDirToPath {
    param([Parameter(Mandatory = $true)][string]$Dir)

    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @()
    if (-not [string]::IsNullOrWhiteSpace($current)) {
        $parts = $current -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    }

    $normalized = $Dir.TrimEnd('\')
    $alreadyPresent = $false
    foreach ($part in $parts) {
        if ([string]::Equals($part.TrimEnd('\'), $normalized, [StringComparison]::OrdinalIgnoreCase)) {
            $alreadyPresent = $true
            break
        }
    }

    if (-not $alreadyPresent) {
        $newPath = if ($parts.Count -gt 0) { ($parts + $Dir) -join ';' } else { $Dir }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        if (($env:Path -split ';') -notcontains $Dir) {
            $env:Path = "$env:Path;$Dir"
        }
        Write-Host "Added $Dir to the user PATH. Open a new terminal if logscout is not found."
    }
}

function Install-Binary {
    param([Parameter(Mandatory = $true)][string]$Source)

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $destination = Join-Path $InstallDir $BinName
    Copy-Item -Force -Path $Source -Destination $destination
    $legacy = Join-Path $InstallDir $LegacyBinName
    if (Test-Path $legacy) {
        Remove-Item -Force $legacy
    }
    Add-InstallDirToPath -Dir $InstallDir
    Write-Host "Installed logscout to $destination"
}

function Install-FromRelease {
    $target = Get-LogScouterTarget
    $asset = "$AppName-$target.zip"
    $url = Get-ReleaseUrl -Asset $asset
    $temp = Join-Path ([IO.Path]::GetTempPath()) ("log-scouter-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $temp | Out-Null

    try {
        $archive = Join-Path $temp $asset
        $extract = Join-Path $temp "extract"
        New-Item -ItemType Directory -Force -Path $extract | Out-Null

        Write-Host "Downloading $url"
        Invoke-LogScouterDownload -Uri $url -OutFile $archive
        Expand-Archive -Force -Path $archive -DestinationPath $extract

        $binary = Get-ChildItem -Path $extract -Recurse -Filter $BinName | Select-Object -First 1
        if (-not $binary) {
            throw "Release archive did not contain $BinName"
        }
        Install-Binary -Source $binary.FullName
    }
    finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path $temp
    }
}

function Install-FromSource {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    $rustupCargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (-not $cargo -and (Test-Path $rustupCargo)) {
        $env:Path = "$(Split-Path $rustupCargo);$env:Path"
        $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    }
    if (-not $cargo) {
        throw "cargo is required. Install Rust from https://rustup.rs/ and retry."
    }

    $temp = Join-Path ([IO.Path]::GetTempPath()) ("log-scouter-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $temp | Out-Null

    try {
        $cargoRoot = Join-Path $temp "cargo-root"
        $args = @(
            "install",
            "--git", "https://github.com/$Repo.git",
            "--bin", "logscout",
            "--root", $cargoRoot,
            "--locked",
            "--force"
        )
        if ($Version -ne "latest") {
            $args = @(
                "install",
                "--git", "https://github.com/$Repo.git",
                "--tag", $Version,
                "--bin", "logscout",
                "--root", $cargoRoot,
                "--locked",
                "--force"
            )
        }

        Write-Host "Building $Repo from source with cargo"
        & cargo @args
        if ($LASTEXITCODE -ne 0) {
            throw "cargo install failed"
        }

        $source = Join-Path $cargoRoot "bin\logscout.exe"
        if (-not (Test-Path $source)) {
            throw "cargo install did not produce $source"
        }
        Install-Binary -Source $source
    }
    finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path $temp
    }
}

if ($FromSource) {
    Install-FromSource
}
else {
    try {
        Install-FromRelease
    }
    catch {
        Write-Warning "No matching prebuilt release asset was found; falling back to cargo install. $($_.Exception.Message)"
        Install-FromSource
    }
}
