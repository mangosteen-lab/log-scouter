#!/usr/bin/env bash
# Install the latest Log Scouter release as the `scout` command.
set -euo pipefail

APP_NAME="log-scouter"
BIN_NAME="scout"
DEFAULT_REPO="mangosteen-lab/log-scouter"

REPO="${LOG_SCOUTER_REPO:-$DEFAULT_REPO}"
VERSION="${LOG_SCOUTER_VERSION:-latest}"
INSTALL_DIR="${LOG_SCOUTER_INSTALL_DIR:-$HOME/.local/bin}"
FROM_SOURCE=0

usage() {
    cat <<'EOF'
Install Log Scouter.

Usage:
  install.sh [--version <tag>] [--repo <owner/repo>] [--install-dir <dir>] [--from-source]

Environment:
  LOG_SCOUTER_REPO         GitHub repository, default mangosteen-lab/log-scouter
  LOG_SCOUTER_VERSION      Release tag, default latest
  LOG_SCOUTER_INSTALL_DIR  Destination directory, default ~/.local/bin
  LOG_SCOUTER_CURL_OPTS    Extra curl flags used by this script, e.g. "-x http://proxy:8080"
EOF
}

die() {
    echo "log-scouter install: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required"
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
        --version)
            [ "$#" -ge 2 ] || die "--version requires a value"
            VERSION="$2"
            shift 2
            ;;
        --repo)
            [ "$#" -ge 2 ] || die "--repo requires a value"
            REPO="$2"
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || die "--install-dir requires a value"
            INSTALL_DIR="$2"
            shift 2
            ;;
        --from-source)
            FROM_SOURCE=1
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
TMP_DIR=""

cleanup() {
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT

curl_to_file() {
    local url="$1"
    local output="$2"
    local curl_args=(-fL --retry 3 --retry-delay 2)

    if [ -n "${LOG_SCOUTER_CURL_OPTS:-}" ]; then
        # shellcheck disable=SC2206
        local extra_args=( ${LOG_SCOUTER_CURL_OPTS} )
        curl_args+=("${extra_args[@]}")
    fi

    curl "${curl_args[@]}" "$url" -o "$output"
}

detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$arch" in
        x86_64|amd64) arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *) return 1 ;;
    esac

    case "$os" in
        Linux) printf '%s-unknown-linux-gnu\n' "$arch" ;;
        Darwin) printf '%s-apple-darwin\n' "$arch" ;;
        *) return 1 ;;
    esac
}

install_binary() {
    local src="$1"
    mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
    install -m 0755 "$src" "$INSTALL_DIR/$BIN_NAME" || die "could not install $BIN_NAME to $INSTALL_DIR"
}

release_url_for() {
    local asset="$1"
    if [ "$VERSION" = "latest" ]; then
        printf 'https://github.com/%s/releases/latest/download/%s\n' "$REPO" "$asset"
    else
        printf 'https://github.com/%s/releases/download/%s/%s\n' "$REPO" "$VERSION" "$asset"
    fi
}

install_from_release() {
    local target asset url archive extract_dir binary_path
    target="$(detect_target)" || return 1
    asset="${APP_NAME}-${target}.tar.gz"
    url="$(release_url_for "$asset")"

    command -v curl >/dev/null 2>&1 || return 1
    command -v tar >/dev/null 2>&1 || return 1
    TMP_DIR="$(mktemp -d)"
    archive="$TMP_DIR/$asset"
    extract_dir="$TMP_DIR/extract"
    mkdir -p "$extract_dir"

    echo "Downloading $url"
    curl_to_file "$url" "$archive" || return 1
    tar -xzf "$archive" -C "$extract_dir" || return 1

    binary_path="$(find "$extract_dir" -type f -name "$BIN_NAME" | head -n 1)"
    [ -n "$binary_path" ] || die "release archive did not contain $BIN_NAME"
    install_binary "$binary_path"
}

install_from_source() {
    local cargo_root source_bin
    if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
        # rustup installs Cargo here on machines without a system Rust toolchain.
        . "$HOME/.cargo/env"
    fi
    need_cmd cargo

    TMP_DIR="$(mktemp -d)"
    cargo_root="$TMP_DIR/cargo-root"
    mkdir -p "$cargo_root"

    echo "Building $REPO from source with cargo"
    if [ "$VERSION" = "latest" ]; then
        cargo install --git "https://github.com/$REPO.git" --bin "$BIN_NAME" --root "$cargo_root" --locked --force
    else
        cargo install --git "https://github.com/$REPO.git" --tag "$VERSION" --bin "$BIN_NAME" --root "$cargo_root" --locked --force
    fi

    source_bin="$cargo_root/bin/$BIN_NAME"
    [ -x "$source_bin" ] || die "cargo install did not produce $source_bin"
    install_binary "$source_bin"
}

if [ "$FROM_SOURCE" -eq 1 ]; then
    install_from_source
else
    if ! install_from_release; then
        echo "No matching prebuilt release asset was found; falling back to cargo install." >&2
        cleanup
        TMP_DIR=""
        install_from_source
    fi
fi

echo "Installed $BIN_NAME to $INSTALL_DIR/$BIN_NAME"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo "Add $INSTALL_DIR to PATH to run '$BIN_NAME' from any shell."
        ;;
esac
