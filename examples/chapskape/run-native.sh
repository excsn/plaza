#!/usr/bin/env bash
# The zero-ceremony path: open ChapsKape as a native desktop window. With no
# arguments this is `--role host`, which plays *and* stands the world up, so it
# also serves the browser page and prints an address others can join at. Pass
# `--role client --connect <url>` to join someone else, or `--role headless` for
# the deployable server.
#
# Click somewhere to go there. Click a tree, a rock or a shoal to walk over and
# work it; click a brute to fight it; click something on the ground to pick it
# up. R runs, space stops, and clicking a square of your pack uses it while
# shift-clicking drops it. The thing worth doing is dropping something and
# watching the ring around it: it is yours alone until the timer runs out, and
# nobody else is even told it is there until then.
#
# `--bots N` seats more or fewer of the world's own. CHAPSKAPE_FEATURES passes
# extra cargo features through, if you add any.
#
# Usage: ./run-native.sh [<args>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p chapskape --bin chapskape --release --manifest-path "$root/Cargo.toml" \
  ${CHAPSKAPE_FEATURES:+--features "$CHAPSKAPE_FEATURES"} -- "$@"
