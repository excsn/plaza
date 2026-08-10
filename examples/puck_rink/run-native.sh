#!/usr/bin/env bash
# The zero-ceremony path: open the rink as a native desktop window. With
# no arguments this is `--role host`, which plays *and* stands up the server, so
# it also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# PUCK_RINK_FEATURES adds cargo features; the second physics backend is one:
#   PUCK_RINK_FEATURES=rapier ./run-native.sh --physics rapier
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p puck_rink --bin puck_rink --release --manifest-path "$root/Cargo.toml" \
  ${PUCK_RINK_FEATURES:+--features "$PUCK_RINK_FEATURES"} -- "$@"
