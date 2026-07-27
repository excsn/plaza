#!/usr/bin/env bash
# The zero-ceremony path: open the playground as a native desktop window. No wasm
# target, no server, no browser. Same code as the wasm build.
#
# Usage: ./run-native.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p rollback_playground --release --manifest-path "$root/Cargo.toml"
