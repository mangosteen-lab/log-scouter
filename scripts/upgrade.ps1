param(
    [string]$Version = $env:LOG_SCOUTER_VERSION,
    [string]$Repo = $env:LOG_SCOUTER_REPO,
    [string]$InstallDir = $env:LOG_SCOUTER_INSTALL_DIR,
    [string]$Proxy = $env:LOG_SCOUTER_PROXY,
    [switch]$FromSource
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Repo)) { $Repo = "mangosteen-lab/log-scouter" }
$branch = if ($env:LOG_SCOUTER_INSTALL_BRANCH) { $env:LOG_SCOUTER_INSTALL_BRANCH } else { "master" }
# A checkout runs its own installer; `irm | iex` has no $PSScriptRoot and falls through to
# the download. "scripts\install.ps1" covers being run from the repo root, as upgrade.sh does.
$localCandidates = if ($PSScriptRoot) {
    @(
        (Join-Path $PSScriptRoot "install.ps1"),
        (Join-Path (Join-Path $PSScriptRoot "scripts") "install.ps1")
    )
} else {
    @()
}
$localInstaller = $localCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1

if ($localInstaller) {
    & $localInstaller -Version $Version -Repo $Repo -InstallDir $InstallDir -Proxy $Proxy -FromSource:$FromSource -Force
    exit $LASTEXITCODE
}

$url = "https://raw.githubusercontent.com/$Repo/$branch/scripts/install.ps1"
$parameters = @{ Uri = $url }
if ($PSVersionTable.PSVersion.Major -lt 6) {
    $parameters.UseBasicParsing = $true
}
if (-not [string]::IsNullOrWhiteSpace($Proxy)) {
    $parameters.Proxy = $Proxy
}

$script = (Invoke-WebRequest @parameters).Content
$installer = [scriptblock]::Create($script)
& $installer -Version $Version -Repo $Repo -InstallDir $InstallDir -Proxy $Proxy -FromSource:$FromSource -Force
