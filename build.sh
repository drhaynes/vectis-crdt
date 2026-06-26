#!/usr/bin/env bash
# Build and test vectis-crdt.
# Prerequisites for demo builds: cargo, wasm-pack (https://rustwasm.github.io/wasm-pack/)
#
# Usage:
#   ./build.sh test     # runs native unit tests
#   ./build.sh demo     # builds dev Rust/Wasm browser demo → wasm_demo/pkg/
#   ./build.sh demo:release # builds release Rust/Wasm browser demo → wasm_demo/pkg/

set -euo pipefail

MODE="${1:-release}"

case "$MODE" in
  test)
    echo "Running native unit tests..."
    cargo test
    ;;
  demo)
    echo "Building dev Rust/Wasm browser demo..."
    (cd wasm_demo && wasm-pack build --target web --out-dir pkg --dev)
    echo "Done → wasm_demo/pkg/"
    ;;
  demo:release)
    echo "Building release Rust/Wasm browser demo..."
    (cd wasm_demo && wasm-pack build --target web --out-dir pkg --release)
    echo "Done → wasm_demo/pkg/"
    ;;
  *)
    echo "Usage: $0 [test|demo|demo:release]"
    exit 1
    ;;
esac
