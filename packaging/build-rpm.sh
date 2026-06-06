#!/usr/bin/env bash
# Builds an .rpm package of GMount Drive (Fedora/RHEL) using cargo-generate-rpm.
# The package metadata (assets + Fedora dependencies) lives in Cargo.toml under
# [package.metadata.generate-rpm]. Run: bash packaging/build-rpm.sh
#
# NOTE: cargo-generate-rpm builds the .rpm on any OS (including Ubuntu), but it can only be
# *installed/tested* on Fedora/RHEL. Treat this artifact as community-tested until verified there.
set -e
cd "$(dirname "$0")/.."   # project root

echo ">> Building release…"
source "$HOME/.cargo/env" 2>/dev/null || true
export RUST_MIN_STACK=268435456
cargo build --release

if ! command -v cargo-generate-rpm >/dev/null 2>&1; then
    echo ">> Installing cargo-generate-rpm (one time)…"
    cargo install cargo-generate-rpm
fi

echo ">> Generating the .rpm…"
cargo generate-rpm

echo ">> ✅ .rpm built under: target/generate-rpm/"
ls -1 target/generate-rpm/*.rpm 2>/dev/null || true
echo "   Install on Fedora with:  sudo dnf install ./target/generate-rpm/*.rpm"
