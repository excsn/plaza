#!/usr/bin/env bash
# The zero-ceremony path: open the duel floor as a native desktop window. With
# no arguments this is `--role host`, which plays *and* stands up the server, so
# it also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# CUBE_YARD_FEATURES adds cargo features; the second physics backend is one:
#   CUBE_YARD_FEATURES=rapier ./run-native.sh --physics rapier
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p cube_yard --bin cube_yard --release --manifest-path "$root/Cargo.toml" \
  ${CUBE_YARD_FEATURES:+--features "$CUBE_YARD_FEATURES"} -- "$@"
