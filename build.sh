#!/usr/bin/env bash
# Build the vectis-crdt Wasm package.
# Prerequisites: cargo, wasm-pack (https://rustwasm.github.io/wasm-pack/)
#
# Usage:
#   ./build.sh          # builds release Wasm → pkg/
#   ./build.sh dev      # builds dev (non-optimised) Wasm → pkg/
#   ./build.sh test     # runs native unit tests

set -euo pipefail

MODE="${1:-release}"

case "$MODE" in
  release)
    echo "Building release Wasm package..."
    ~/.cargo/bin/wasm-pack build --target web --out-dir pkg --release -- --features wasm
    echo "Done → pkg/"
    ;;
  dev)
    echo "Building dev Wasm package..."
    ~/.cargo/bin/wasm-pack build --target web --out-dir pkg --dev -- --features wasm
    echo "Done → pkg/"
    ;;
  test)
    echo "Running native unit tests..."
    cargo test
    ;;
  test-wasm)
    echo "Running Wasm tests in headless Chrome..."
    wasm-pack test --headless --chrome
    ;;
  *)
    echo "Usage: $0 [release|dev|test|test-wasm]"
    exit 1
    ;;
esac
