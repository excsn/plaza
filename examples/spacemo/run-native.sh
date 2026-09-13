#!/usr/bin/env bash
# Opens the spacemo as a native desktop window. With no arguments this is
# `--role host`, which plays *and* stands up the server, so it also serves the
# browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else or `--role headless` for
# the deployable server.
#
# There are no encoding flags: relevance strategy and bit packing are host dials
# on the panel, because they change who you are told about and the difference
# only shows while the volume keeps moving.
#
#
# SPACEMO_FEATURES passes extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p spacemo --bin spacemo --release --manifest-path "$root/Cargo.toml" \
  ${SPACEMO_FEATURES:+--features "$SPACEMO_FEATURES"} -- "$@"
