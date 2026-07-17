# Install the log-scouter "log-schema" skill for Claude Code.
#
#   irm https://raw.githubusercontent.com/mangosteen-lab/log-scouter/master/scripts/install-log-schema-claude-skill.ps1 | iex
#
# Downloads SKILL.md into ${CLAUDE_CONFIG_DIR:-$HOME\.claude}\skills\log-schema\.
# Overrides: LOG_SCOUTER_REPO (owner/repo), LOG_SCOUTER_REF (branch/tag), CLAUDE_CONFIG_DIR,
# LOG_SCOUTER_PROXY.
param(
    [string]$Repo = $env:LOG_SCOUTER_REPO,
    [string]$Ref = $env:LOG_SCOUTER_REF,
    [string]$ConfigDir = $env:CLAUDE_CONFIG_DIR,
    [string]$Proxy = $env:LOG_SCOUTER_PROXY
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Repo)) { $Repo = "mangosteen-lab/log-scouter" }
if ([string]::IsNullOrWhiteSpace($Ref)) { $Ref = "master" }
if ([string]::IsNullOrWhiteSpace($ConfigDir)) { $ConfigDir = Join-Path $HOME ".claude" }

$base = "https://raw.githubusercontent.com/$Repo/$Ref/skills/log-schema"
$dest = Join-Path $ConfigDir "skills\log-schema"

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
    throw "install-log-schema-claude-skill: could not download SKILL.md from $base : $_"
}

Write-Host "Installed the 'log-schema' skill to $dest"
Write-Host "Start a new Claude Code session, then ask it to generate a logscout schema from your log file."
