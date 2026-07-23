#!/usr/bin/env bash
# Build the playground to wasm and serve it, handling every bit of ceremony:
# installs the wasm target if missing, builds, copies the artifact next to the
# page, optionally shrinks it, and starts a static server.
#
# Usage: ./serve.sh [port]   (default 8080)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
port="${1:-8080}"

# 1. Ensure the wasm target is present.
if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "==> installing wasm32-unknown-unknown target"
  rustup target add wasm32-unknown-unknown
fi

# 2. Build the release wasm.
echo "==> building (release wasm)"
( cd "$root" && cargo build -p rollback_playground --target wasm32-unknown-unknown --release )

# 3. Place the artifact next to index.html.
cp "$root/target/wasm32-unknown-unknown/release/rollback_playground.wasm" "$here/static/rollback_playground.wasm"

# 4. Shrink it if binaryen is available (optional; skipped silently otherwise).
if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> optimising with wasm-opt"
  wasm-opt -Oz "$here/static/rollback_playground.wasm" -o "$here/static/rollback_playground.wasm"
fi

# 5. Serve the static directory.
echo
echo "==> serving http://localhost:$port   (Ctrl-C to stop)"
cd "$here/static"
if command -v basic-http-server >/dev/null 2>&1; then
  exec basic-http-server -a "127.0.0.1:$port" .
else
  # Force the wasm MIME type: some Python installs serve .wasm as
  # application/octet-stream, which browsers refuse to stream-compile.
  exec python3 -c "
import http.server, socketserver, sys
handler = http.server.SimpleHTTPRequestHandler
handler.extensions_map['.wasm'] = 'application/wasm'
socketserver.TCPServer(('127.0.0.1', int(sys.argv[1])), handler).serve_forever()
" "$port"
fi
