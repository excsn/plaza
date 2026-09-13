#!/usr/bin/env bash
# Opens the playground as a native desktop window. It needs no wasm target,
# server or browser and runs the same code as the wasm build.
#
# Usage: ./run-native.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p netcode_playground --release --manifest-path "$root/Cargo.toml"
