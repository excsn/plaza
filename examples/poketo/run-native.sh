#!/usr/bin/env bash
# The zero-ceremony path: open the poketo as a native desktop window. With
# no arguments this is `--role host`, which plays *and* stands up the server, so
# it also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# Arrows or WASD walk a tile at a time. Stepping onto the wrong tile starts a
# battle, where 1 strikes and 2 guards; nothing else is a control, because a
# turn-based battle has nothing to hold down.
#
#
# POKETO_FEATURES passes extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p poketo --bin poketo --release --manifest-path "$root/Cargo.toml" \
  ${POKETO_FEATURES:+--features "$POKETO_FEATURES"} -- "$@"
