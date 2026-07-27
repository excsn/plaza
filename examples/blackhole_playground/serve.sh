#!/usr/bin/env bash
# Build the browser client to wasm and host it. Unlike a plain static server, the
# host binary serves the page, the wasm, *and* the WebSocket arena on one port, so
# a joiner gets a single URL and there is a real server to connect to. That is the
# whole point of the listen-server shape.
#
# Usage: ./serve.sh [port]   (default 8080)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$here/../.." && pwd)/target"
port="${1:-8080}"

# 1. Ensure the wasm target is present.
if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "==> installing wasm32-unknown-unknown target"
  rustup target add wasm32-unknown-unknown
fi

# 2. Build the browser client. `--no-default-features --features web` is required:
#    the default set pulls in the native socket (tungstenite) and the actix server,
#    and neither compiles to wasm. `web` is the browser client alone.
echo "==> building browser client (release wasm)"
( cd "$root" && cargo build -p blackhole_playground --target wasm32-unknown-unknown --release --no-default-features --features web )

# 3. Place the artifact next to index.html. It is gitignored: a build product.
cp "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/blackhole_playground.wasm" "$here/static/blackhole_playground.wasm"

# 3b. Fail loudly if the page's JS does not satisfy every import the wasm asks
#    for. miniquad's loader stubs a missing import instead of erroring, so a
#    renamed function or a forgotten <script> tag produces a page that loads and
#    silently does nothing, which is exactly the bug this check exists to catch.
python3 "$root/../ws_client/check_js_imports.py" "$here/static/blackhole_playground.wasm" "$root/../ws_client/js/plaza_ws.js"

# 4. Shrink it if binaryen is available (optional; skipped silently otherwise).
if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> optimising with wasm-opt"
  wasm-opt -Oz "$here/static/blackhole_playground.wasm" -o "$here/static/blackhole_playground.wasm"
fi

# 5. Host it. One process serves the page, the wasm, and /ws, and prints the local
#    and LAN URLs. actix serves .wasm with the right MIME type itself, so there is
#    no static-server ceremony to get wrong.
echo
echo "==> hosting on port $port   (Ctrl-C to stop)"
exec cargo run -p blackhole_playground --release --manifest-path "$root/Cargo.toml" -- \
  --role headless --bind "0.0.0.0:$port" --serve "$here/static"
