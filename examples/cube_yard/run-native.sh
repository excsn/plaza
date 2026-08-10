#!/usr/bin/env bash
# The zero-ceremony path: open the cube yard as a native desktop window. With
# no arguments this is `--role host`, which plays *and* stands up the server, so
# it also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# --encoding full|packed|budgeted|delta picks how much the wire is asked to
# carry; --snap turns on quantise-both-sides. See the README for what each costs.
#
# CUBE_YARD_FEATURES passes extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p cube_yard --bin cube_yard --release --manifest-path "$root/Cargo.toml" \
  ${CUBE_YARD_FEATURES:+--features "$CUBE_YARD_FEATURES"} -- "$@"
