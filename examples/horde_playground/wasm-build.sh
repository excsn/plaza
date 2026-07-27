#!/usr/bin/env bash
# Build the browser client to wasm and put it next to index.html.
#
# Separate from wasm-serve.sh because a rebuild and a running server are two
# different things to want, and welding them together means you cannot do the
# first without the second. That matters more than it sounds: skip the combined
# script because you already have a server, and you are now debugging a stale
# artifact against new code, which reads as a protocol bug and is not one.
#
# Usage: ./wasm-build.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$here/../.." && pwd)/target"

# 1. Ensure the wasm target is present.
if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "==> installing wasm32-unknown-unknown target"
  rustup target add wasm32-unknown-unknown
fi

# 2. Build the browser client. `--no-default-features --features web` is required:
#    the default set pulls in the native socket (tungstenite) and the actix server,
#    and neither compiles to wasm. `web` is the browser client alone.
echo "==> building browser client (release wasm)"
( cd "$root" && cargo build -p horde_playground --target wasm32-unknown-unknown --release --no-default-features --features web )

# 3. Place the artifact next to index.html. It is gitignored: a build product.
cp "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/horde_playground.wasm" "$here/static/horde_playground.wasm"

# 3b. Fail loudly if the page's JS does not satisfy every import the wasm asks
#    for. miniquad's loader stubs a missing import instead of erroring, so a
#    renamed function or a forgotten <script> tag produces a page that loads and
#    silently does nothing, which is exactly the bug this check exists to catch.
python3 "$root/../ws_client/check_js_imports.py" "$here/static/horde_playground.wasm" "$root/../ws_client/js/plaza_ws.js"

# 4. Shrink it if binaryen is available (optional; skipped silently otherwise).
if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> optimising with wasm-opt"
  wasm-opt -Oz "$here/static/horde_playground.wasm" -o "$here/static/horde_playground.wasm"
fi

echo
echo "==> built ${here}/static"
