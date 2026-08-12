#!/usr/bin/env bash
# The zero-ceremony path: open 3DGoW as a native desktop window. With no
# arguments this is `--role host`, which plays *and* stands up the zone, so it
# also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# Arrows or WASD walk, Q and E change floor, 1 casts, 2 parties with the nearest
# character and 3 leaves. Walking two floors away from a party member is the
# thing worth doing: their body leaves the world and their entry stays, with a
# bearing and a floor offset, which is what the second relevance channel buys.
#
# GOW_3D_FEATURES passes extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p gow_3d --bin gow_3d --release --manifest-path "$root/Cargo.toml" \
  ${GOW_3D_FEATURES:+--features "$GOW_3D_FEATURES"} -- "$@"
