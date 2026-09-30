#!/usr/bin/env bash
# Build the page, then serve it and the door on http://127.0.0.1:8083.
#
# Usage: ./wasm-serve.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"

"$here/wasm-build.sh"
exec cargo run -p plaza_example_door_policy --bin serve --manifest-path "$root/Cargo.toml"
