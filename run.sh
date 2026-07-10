#!/usr/bin/env bash
# Build and launch the Rust log viewer.
#   ./run.sh [FOLDER] [FILE ...]
# With no arguments it opens the bundled ./examples folder.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # rustup installs Cargo here on machines without a system Rust toolchain.
    . "$HOME/.cargo/env"
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required. Install Rust from https://rustup.rs/ and retry." >&2
    exit 127
fi

if [ "$#" -eq 0 ]; then
    exec cargo run --release -- examples
else
    exec cargo run --release -- "$@"
fi
