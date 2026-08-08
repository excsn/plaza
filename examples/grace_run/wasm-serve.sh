#!/usr/bin/env bash
# Build the browser client, then host it.
#
# One process serves the page, the wasm, *and* the WebSocket battlefield on one
# port, so a joiner gets a single URL and there is a real server to connect to.
#
# The build is wasm-build.sh, called here rather than duplicated, so the two
# scripts cannot drift into building differently.
#
# Usage: ./wasm-serve.sh [port]   (default 8098)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
port="${1:-8098}"

"$here/wasm-build.sh"

# actix serves .wasm with the right MIME type itself, so there is no static-server
# ceremony to get wrong.
echo
echo "==> hosting on port $port   (Ctrl-C to stop)"
exec cargo run -p grace_run --bin grace_run --release --manifest-path "$root/Cargo.toml" -- \
  --role headless --bind "0.0.0.0:$port" --serve "$here/static"
