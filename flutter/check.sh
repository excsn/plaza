#!/usr/bin/env bash
# Green means all three Dart packages, conformance included.
#
# The conformance suite reads fixtures written by the Rust side, so a wire
# change fails here rather than silently in an app. Regenerate them with:
#   PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_wire --features msgpack,json --test dart_fixtures
set -e

here="$(cd "$(dirname "$0")" && pwd)"

for pkg in plaza_wire plaza_client plaza_client_utils plaza_ws; do
  echo "== $pkg"
  # e2e needs a server; ./e2e.sh runs those.
  (cd "$here/$pkg" && dart pub get >/dev/null && dart test --exclude-tags e2e)
done

echo "== plaza_flame"
(cd "$here/plaza_flame" && flutter pub get >/dev/null && flutter test)

# The example runs against LoopbackSocket, so it needs no server and no display.
# An example nothing executes is documentation wearing a .dart extension.
echo "== plaza_flame/example"
(cd "$here/plaza_flame/example" && flutter pub get >/dev/null && flutter test)
