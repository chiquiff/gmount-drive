#!/usr/bin/env bash
# Build helper for GMount Drive.
set -e
cd "$(dirname "$0")"
source "$HOME/.cargo/env" 2>/dev/null || true
export RUST_MIN_STACK=268435456
echo ">> rustc: $(rustc --version)"
echo ">> Building (the first time takes a few minutes)..."
cargo build "$@"
echo ">> BUILD OK ✅  Binary at: target/debug/gdrive-mount"
