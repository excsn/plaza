#!/usr/bin/env bash
# Stands the lobby and its three arenas up on http://127.0.0.1:8090.
#
# One script rather than the three the playgrounds carry, because there is no
# wasm step: the browser client here is plain HTML embedded with `include_str!`
# and served by the same actix app as the sockets, so there is nothing to build
# separately and nothing to serve it with.
#
# What this does buy over `cargo run -p plaza_example_lobby_world` is working
# from anywhere. The examples are their own workspace, so that command only
# resolves from inside `examples/`.
#
# Usage: ./run.sh [-- <cargo args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p plaza_example_lobby_world --manifest-path "$root/Cargo.toml" "$@"
