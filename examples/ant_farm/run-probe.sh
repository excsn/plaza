#!/usr/bin/env bash
# The traffic fleet: watchers with no window, against a running server.
#
# Usage: ./run-probe.sh [--connect <host:port>] [--watchers N] [--half N] [--drift F] [--secs N] [--draw]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="$(cd "$(dirname "$0")/../.." && pwd)/target"
exec cargo run -p plaza_example_ant_farm --release --manifest-path "$root/Cargo.toml" -- probe "$@"
