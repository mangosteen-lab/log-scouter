#!/usr/bin/env bash
# Convenience wrapper for scripts/upgrade.sh.
set -euo pipefail

DEFAULT_REPO="mangosteen-lab/log-scouter"
REPO="${LOG_SCOUTER_REPO:-$DEFAULT_REPO}"
BRANCH="${LOG_SCOUTER_INSTALL_BRANCH:-master}"

script_path="${BASH_SOURCE[0]:-$0}"

if [ -f "$script_path" ]; then
    script_dir="$(CDPATH= cd -- "$(dirname -- "$script_path")" >/dev/null 2>&1 && pwd || pwd)"
    for candidate in "$script_dir/scripts/upgrade.sh"; do
        if [ -f "$candidate" ] && [ "$candidate" != "$script_path" ]; then
            exec bash "$candidate" "$@"
        fi
    done
fi

curl_args=(-fsSL)
if [ -n "${LOG_SCOUTER_CURL_OPTS:-}" ]; then
    # shellcheck disable=SC2206
    extra_args=( ${LOG_SCOUTER_CURL_OPTS} )
    curl_args+=("${extra_args[@]}")
fi

url="https://raw.githubusercontent.com/$REPO/$BRANCH/scripts/upgrade.sh"
curl "${curl_args[@]}" "$url" | bash -s -- "$@"
