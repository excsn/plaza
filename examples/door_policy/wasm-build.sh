#!/usr/bin/env bash
# Build the browser page to wasm and put it next to index.html.
#
# Usage: ./wasm-build.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$here/../.." && pwd)/target"

if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "The wasm32-unknown-unknown target is not installed. Install it with:" >&2
  echo "  rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi

# From the examples workspace, so its .cargo/config.toml supplies the link flag
# rustc 1.98.1 needs for the imports plaza_ws.js provides.
echo "==> building the page (release wasm)"
( cd "$root" && cargo build -p plaza_example_door_policy --bin door_page --target wasm32-unknown-unknown --release --no-default-features --features web )

cp "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/door_page.wasm" "$here/static/door_page.wasm"

# miniquad's loader stubs a missing import instead of failing, so check them.
python3 "$root/../ws_client/check_js_imports.py" "$here/static/door_page.wasm" "$root/../ws_client/js/plaza_ws.js"

echo "==> $here/static/door_page.wasm"
