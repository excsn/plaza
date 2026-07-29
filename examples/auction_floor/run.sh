#!/usr/bin/env bash
# Stands the auction floor up on http://127.0.0.1:8091.
#
# Works from anywhere: the examples are their own workspace, so a bare
# `cargo run -p plaza_example_auction_floor` only resolves from inside them.
#
# Usage: ./run.sh [-- <cargo args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p plaza_example_auction_floor --manifest-path "$root/Cargo.toml" "$@"
