# Install the log-scouter "log-schema" skill for OpenAI Codex CLI.
#
#   irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-codex-skill.ps1 | iex
#
# Downloads SKILL.md (+ openai.yaml) into ${CODEX_HOME:-$HOME\.codex}\skills\log-schema\.
# Overrides: LOG_SCOUTER_REPO (owner/repo), LOG_SCOUTER_REF (branch/tag), CODEX_HOME,
# LOG_SCOUTER_PROXY.
param(
    [string]$Repo = $env:LOG_SCOUTER_REPO,
    [string]$Ref = $env:LOG_SCOUTER_REF,
    [string]$CodexHome = $env:CODEX_HOME,
    [string]$Proxy = $env:LOG_SCOUTER_PROXY
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Repo)) { $Repo = "mangosteen-lab/log-scouter" }
if ([string]::IsNullOrWhiteSpace($Ref)) { $Ref = "master" }
if ([string]::IsNullOrWhiteSpace($CodexHome)) { $CodexHome = Join-Path $HOME ".codex" }

$base = "https://raw.githubusercontent.com/$Repo/$Ref/skills/log-schema"
$dest = Join-Path $CodexHome "skills\log-schema"

function Invoke-SkillDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $parameters = @{ Uri = $Uri; OutFile = $OutFile }
    # Windows PowerShell 5.1 needs -UseBasicParsing; PowerShell 6+ dropped the switch.
    if ($PSVersionTable.PSVersion.Major -lt 6) {
        $parameters.UseBasicParsing = $true
    }
    if (-not [string]::IsNullOrWhiteSpace($Proxy)) {
        $parameters.Proxy = $Proxy
    }
    Invoke-WebRequest @parameters
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null

try {
    Invoke-SkillDownload -Uri "$base/SKILL.md" -OutFile (Join-Path $dest "SKILL.md")
} catch {
    throw "install-log-schema-codex-skill: could not download SKILL.md from $base : $_"
}

# openai.yaml is optional Codex metadata; do not fail the install if it is absent.
try {
    Invoke-SkillDownload -Uri "$base/openai.yaml" -OutFile (Join-Path $dest "openai.yaml")
} catch {
    Write-Verbose "openai.yaml not published at $base; skipping."
}

Write-Host "Installed the 'log-schema' skill to $dest"
Write-Host "Start a new Codex session, then ask it to generate a logscout schema from your log file."
