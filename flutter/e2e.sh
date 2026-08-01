#!/usr/bin/env bash
# Drives the Dart client against a real plaza server over a real socket.
#
# Everything else is tested against LoopbackSocket, which proves the lifecycle
# and never the wire.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"

# The examples are their own workspace, so the target dir has to be named rather
# than assumed: this built into examples/target and then ran $root/target/debug,
# which is whatever the last root build left there. A stale server passed a suite
# that should have failed. `CARGO_TARGET_DIR` is what ../check.sh uses, so the two
# scripts share one build.
cd "$root/examples"
CARGO_TARGET_DIR="$root/target" cargo build -q -p plaza_example_lobby_world
"$root/target/debug/plaza_example_lobby_world" >/tmp/plaza_e2e_server.log 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT

for _ in $(seq 1 40); do
  if curl -sf -o /dev/null http://127.0.0.1:8090/; then break; fi
  sleep 0.25
done

cd "$here/plaza_ws"
dart pub get >/dev/null
dart test --tags e2e

# The example runs against the same server, because an example nothing executes
# is documentation that compiles rather than an example that works. Its exit codes
# are the assertion: 0 played, 2 refused on a version skew.
echo "== example/lobby_client.dart"
dart run example/lobby_client.dart --seconds 1 >/dev/null

set +e
dart run example/lobby_client.dart --protocol 1 --seconds 1 >/dev/null 2>&1
skew=$?
set -e
if [ "$skew" -ne 2 ]; then
  echo "the skew path exited $skew, expected 2" >&2
  exit 1
fi
echo "   both paths behaved"
