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

cargo test --workspace --features "plaza_client_utils/net-sim,plaza_ws/native,plaza_ws/json,plaza_session/actix_host,plaza_wire/build,plaza_wire/msgpack,plaza_lobby/cache"
cd examples
CARGO_TARGET_DIR="$(cd .. && pwd)/target" cargo test --workspace

# Nothing compiles the browser pages, so every way their hand-written frame
# handling can be wrong is silent. One of them shipped wrong.
echo "--- the browser pages ---"
./check_pages.py

# A workspace check unifies features across its members, so a crate that uses
# something it never declared compiles anyway, on a neighbour's enable. Only a
# per-package check says whether a manifest is honest. Three examples were
# under-declared this way at once, all of them building green until asked alone.
echo "--- each package on its own ---"
for manifest in */Cargo.toml; do
  pkg=$(grep -m1 '^name' "$manifest" | cut -d'"' -f2)
  CARGO_TARGET_DIR="$(cd .. && pwd)/target" cargo check -q -p "$pkg" \
    || { echo "$pkg does not build alone: its manifest is missing a feature it uses"; exit 1; }
done
