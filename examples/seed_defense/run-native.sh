#!/usr/bin/env bash
# The zero-ceremony path: open the arena as a native desktop window. With no
# arguments this is `--role host`, which plays *and* stands up the arena, so it
# also serves the browser page and prints an address others can join at. Pass
# `--role observer` to watch without a player, or `--role client --connect <url>`
# to join someone else. For the pure single-process teaching build with no
# networking compiled in, use `--no-default-features --features native,client`.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p seed_defense --release --manifest-path "$root/Cargo.toml" -- "$@"
