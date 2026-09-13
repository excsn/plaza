#!/usr/bin/env bash
# Opens 3DGoW as a native desktop window. With no arguments this is
# `--role host`, which plays *and* stands up the zone, so it also serves the
# browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else or `--role headless` for
# the deployable server.
#
# Arrows or WASD move, space jumps, tab cycles a beast to fight, 1 2 and 3 cast
# Strike, Bolt and Mend, P parties with the nearest adventurer and O leaves.
# Walk away from a party member until they drop out of view: their body leaves
# the world and their party entry stays, through the second relevance channel.
#
# GOW_3D_FEATURES passes extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p gow_3d --bin gow_3d --release --manifest-path "$root/Cargo.toml" \
  ${GOW_3D_FEATURES:+--features "$GOW_3D_FEATURES"} -- "$@"
