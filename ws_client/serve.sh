#!/usr/bin/env bash
# Builds the browser echo spike and serves it, plus a local echo server to talk
# to. Two processes, one command, because the whole point is a round trip.
#
# Usage: ./serve.sh [page-port] [echo-port]   (defaults 8090 and 9001)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
page_port="${1:-8090}"
echo_port="${2:-9001}"

if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "==> installing wasm32-unknown-unknown target"
  rustup target add wasm32-unknown-unknown
fi

echo "==> building the browser spike"
( cd "$root" && cargo build -p plaza_ws --no-default-features --features miniquad \
    --example echo_web --target wasm32-unknown-unknown --release )
cp "$root/target/wasm32-unknown-unknown/release/examples/echo_web.wasm" "$here/static/"
cp "$here/js/plaza_ws.js" "$here/static/"

# Catch a name mismatch here rather than as a page that loads and does nothing:
# miniquad stubs out imports nothing provides, so this is the only loud failure
# available.
echo "==> checking the wasm's imports against the plugin"
"$here/check_js_imports.py" "$here/static/echo_web.wasm" "$here/js/plaza_ws.js"

echo "==> starting the echo server on ws://127.0.0.1:$echo_port"
( cd "$root" && cargo run -q -p plaza_ws --features native --example echo_server -- "$echo_port" ) &
echo_pid=$!
trap 'kill "$echo_pid" 2>/dev/null || true' EXIT

echo
echo "==> open http://localhost:$page_port   (green means it worked)"
cd "$here/static"
if command -v basic-http-server >/dev/null 2>&1; then
  exec basic-http-server -a "127.0.0.1:$page_port" .
else
  exec python3 -c "
import http.server, socketserver
class H(http.server.SimpleHTTPRequestHandler):
  extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, '.wasm': 'application/wasm'}
socketserver.TCPServer.allow_reuse_address = True
socketserver.TCPServer(('127.0.0.1', $page_port), H).serve_forever()
"
fi
