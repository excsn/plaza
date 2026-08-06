#!/usr/bin/env bash
# Stands the lobby up on http://127.0.0.1:8092. Tables are spawned per match.
#
# One script rather than the three the playgrounds carry, because there is no
# wasm step: the browser client here is plain HTML served by the same actix app
# as the sockets, so there is nothing to build separately.
#
# What this does buy over `cargo run -p plaza_example_parlour_game` is working
# from anywhere. The examples are their own workspace, so that command only
# resolves from inside `examples/`.
#
# Usage: ./run.sh [-- <cargo args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p plaza_example_parlour_game --manifest-path "$root/Cargo.toml" "$@"
