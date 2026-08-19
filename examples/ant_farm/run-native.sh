#!/usr/bin/env bash
# The zero-ceremony path: stand the colony up and open the observer window on
# it. Pass --connect <host:port> to watch a server already running somewhere
# else, in which case nothing is started locally. --half sizes the pane;
# every other argument goes to the server (--ants, --sites, --seed, --bind).
#
# Usage: ./run-native.sh [--ants N] [--half N] [--connect <host:port>]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"

connect=""
viewer_args=()
server_args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --connect) connect="$2"; shift 2 ;;
    --half) viewer_args+=(--half "$2"); shift 2 ;;
    *) server_args+=("$1"); shift ;;
  esac
done

cargo build -p plaza_example_ant_farm --release --features view --manifest-path "$root/Cargo.toml"

if [ -z "$connect" ]; then
  connect="127.0.0.1:4747"
  "$CARGO_TARGET_DIR/release/plaza_example_ant_farm" --bind "$connect" ${server_args[@]+"${server_args[@]}"} &
  server_pid=$!
  trap 'kill "$server_pid" 2>/dev/null || true' EXIT
fi

"$CARGO_TARGET_DIR/release/ant_farm_view" --connect "$connect" ${viewer_args[@]+"${viewer_args[@]}"}
