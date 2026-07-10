#!/usr/bin/env bash
# Remove the `logscout` command installed by scripts/install.sh.
set -euo pipefail

BIN_NAME="logscout"
LEGACY_BIN_NAME="scout"
INSTALL_DIR="${LOG_SCOUTER_INSTALL_DIR:-$HOME/.local/bin}"
PURGE=0

usage() {
    cat <<'EOF'
Uninstall Log Scouter.

Usage:
  uninstall.sh [--install-dir <dir>] [--purge]

By default this removes only the installed logscout binary. --purge also removes
the user-level reusable filter/schema library at ~/.log-scouter. Project-local
state in <project>/.logscouter is never removed.
EOF
}

die() {
    echo "log-scouter uninstall: $*" >&2
    exit 1
}

expand_path() {
    case "$1" in
        "~") printf '%s\n' "$HOME" ;;
        "~/"*) printf '%s/%s\n' "$HOME" "${1#~/}" ;;
        *) printf '%s\n' "$1" ;;
    esac
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --install-dir)
            [ "$#" -ge 2 ] || die "--install-dir requires a value"
            INSTALL_DIR="$2"
            shift 2
            ;;
        --purge)
            PURGE=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

INSTALL_DIR="$(expand_path "$INSTALL_DIR")"
BINARY="$INSTALL_DIR/$BIN_NAME"

if [ -e "$BINARY" ]; then
    rm -f "$BINARY"
    echo "Removed $BINARY"
else
    echo "$BINARY was not installed"
fi

LEGACY_BINARY="$INSTALL_DIR/$LEGACY_BIN_NAME"
if [ -e "$LEGACY_BINARY" ]; then
    rm -f "$LEGACY_BINARY"
    echo "Removed legacy $LEGACY_BINARY"
fi

if [ "$PURGE" -eq 1 ]; then
    USER_DIR="$HOME/.log-scouter"
    if [ -d "$USER_DIR" ]; then
        rm -rf "$USER_DIR"
        echo "Removed $USER_DIR"
    else
        echo "$USER_DIR was not present"
    fi
fi
