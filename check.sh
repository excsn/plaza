#!/bin/sh
# Green means both workspaces: the examples are the libraries' integration
# suite, so a library change is not verified by the root workspace alone.
#
# The feature list exists because the split ended feature unification with the
# examples: without it, tests gated behind these features silently stop
# running. --all-features cannot replace it, since the miniquad backend is
# wasm-only. `plaza_client_utils/net-sim` also carries the net_sim half of the
# Dart behaviour vectors, so dropping it would stop asserting them.
set -e

cargo test --workspace --features "plaza_client_utils/net-sim,plaza_ws/native,plaza_ws/json,plaza_session/actix_host,plaza_wire/build,plaza_wire/msgpack"
cd examples
CARGO_TARGET_DIR="$(cd .. && pwd)/target" cargo test --workspace
