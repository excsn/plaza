#!/usr/bin/env sh
# Refreshes each page's copy of wire/js/plaza_protocol.js. check_pages.py
# fails on a copy that drifts from the canonical file.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
src="$here/../wire/js/plaza_protocol.js"

for page in "$here"/*/static/index.html; do
  if grep -q 'plaza_protocol\.js' "$page"; then
    cp "$src" "$(dirname "$page")/plaza_protocol.js"
    echo "synced $(dirname "$page")/plaza_protocol.js"
  fi
done
