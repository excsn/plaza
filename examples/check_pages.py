#!/usr/bin/env python3
"""Checks the browser pages' frame handling against the Rust.

The pages share one frame layer, `wire/js/plaza_protocol.js`, each serving its
own copy beside the page (`sync_protocol_js.sh` refreshes them). Nothing
compiles a page and nothing ran one, so every way it can be wrong is silent,
and one of them shipped wrong. This checks the canonical file's constants
against `plaza_wire::frame::Kind`, every copy against the canonical bytes, and
the three page-level things that have actually bitten.

**A codec that encodes the wrong container.** `parlour_game`'s page built its ops
batch with `Object.keys([op])`, which is `["0"]`, so a play went out as a one-key
map where the server wanted a sequence. Every play was discarded and the turn
timeout played for the player, which reads exactly like a game rule rather than a
bug. The fix is one branch; the reason it survived is that the page's decoder was
exercised constantly and its encoder never.

**A kind byte that drifts.** The constants live in `plaza_protocol.js`, and each
page serves a copy. This ties the canonical file to `plaza_wire::frame::Kind`
and each copy to the canonical file.

**A unit variant nobody handles.** Serde writes a fieldless variant as a bare
string, not a one-entry map, so a page reading `op.Something` drops it with no
trace. Pages whose server has no fieldless variant are fine until someone adds
one, which is when this check earns its place.

    ./check_pages.py

Requires `node` only for the codec execution, which is the part that has to run
JavaScript to mean anything.
"""

import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
FIXTURES = ROOT / "flutter" / "fixtures"

failures = []


def fail(where, message):
  failures.append(f"{where}: {message}")


def rust_kinds():
  """The discriminants in `plaza_wire::frame::Kind`, which the pages copy."""
  source = (ROOT / "wire" / "src" / "frame.rs").read_text()
  body = re.search(r"pub enum Kind \{(.*?)\n\}", source, re.S)
  if not body:
    sys.exit("could not find `pub enum Kind` in wire/src/frame.rs")
  return {m.group(1).upper(): int(m.group(2)) for m in re.finditer(r"^\s*(\w+) = (\d+),", body.group(1), re.M)}


def page_kinds(text):
  """`KIND_OPS = 0` in any of the shapes the pages declare it."""
  return {m.group(1): int(m.group(2)) for m in re.finditer(r"\bKIND_(\w+)\s*=\s*(\d+)", text)}


def unit_variants(example):
  """The `*Op*` enums an example defines, and which of their variants are fieldless.

  A bare `Name,` line inside the enum body. Doc comments and attributes are
  skipped, which is why this reads lines rather than splitting on commas.
  """
  found, seen = {}, []
  src = HERE / example / "src"
  if not src.is_dir():
    return seen, found
  for path in sorted(src.rglob("*.rs")):
    text = path.read_text()
    for enum in re.finditer(r"pub enum (\w*Op\w*)\s*\{(.*?)\n\}", text, re.S):
      seen.append(enum.group(1))
      units = [line.strip().rstrip(",") for line in enum.group(2).split("\n")
               if re.match(r"^\s*[A-Z]\w*\s*,\s*$", line)]
      if units:
        found[enum.group(1)] = units
  return seen, found


def extract(text, name):
  """One top-level `function name(...) { ... }`, by brace balance."""
  start = text.find(f"function {name}(")
  if start < 0:
    return None
  depth = 0
  for i in range(text.index("{", start), len(text)):
    if text[i] == "{":
      depth += 1
    elif text[i] == "}":
      depth -= 1
      if depth == 0:
        return text[start:i + 1]
  return None


def check_codec(page, text):
  """Runs the page's own codec against fixtures the Rust side wrote.

  The point is that the code under test is the code the browser runs, lifted out
  of the page rather than reimplemented beside it.
  """
  decode, encode = extract(text, "mpDecode"), extract(text, "mpEncode")
  if not decode or not encode:
    return fail(page, "has mpDecode/mpEncode but they could not be extracted")

  if not shutil.which("node"):
    return fail(page, "node is needed to execute the page codec and is not installed")

  harness = f"""
{decode}
{encode}
const fs = require('fs');
const fail = (m) => {{ console.log('FAIL ' + m); }};

for (const name of {json.dumps(sorted(p.stem.replace('.named', '') for p in FIXTURES.glob('*.named.msgpack')))}) {{
  const bytes = new Uint8Array(fs.readFileSync('{FIXTURES}/' + name + '.named.msgpack'));
  let decoded;
  try {{ decoded = mpDecode(bytes); }} catch (e) {{ fail(name + ' did not decode: ' + e.message); continue; }}
  const json = JSON.parse(fs.readFileSync('{FIXTURES}/' + name + '.json', 'utf8'));
  if (JSON.stringify(decoded) !== JSON.stringify(json)) {{
    fail(name + ' decoded to something other than its json twin');
  }}
}}

// The shape the bug was in: a batch is a sequence, not a one-key map.
const batch = Uint8Array.from(mpEncode([{{ PlayCard: 9 }}]));
if ((batch[0] & 0xf0) !== 0x90) {{
  fail('an ops batch encoded as 0x' + batch[0].toString(16) + ', which is not a msgpack array');
}}
if (JSON.stringify(mpDecode(batch)) !== JSON.stringify([{{ PlayCard: 9 }}])) {{
  fail('an ops batch did not survive its own round trip');
}}
"""
  with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False) as f:
    f.write(harness)
    script = f.name
  try:
    out = subprocess.run(["node", script], capture_output=True, text=True)
  finally:
    Path(script).unlink(missing_ok=True)
  if out.returncode != 0:
    return fail(page, f"the codec harness threw: {out.stderr.strip().splitlines()[-1] if out.stderr.strip() else '?'}")
  for line in out.stdout.splitlines():
    if line.startswith("FAIL "):
      fail(page, line[5:])


def main():
  kinds = rust_kinds()

  canonical = ROOT / "wire" / "js" / "plaza_protocol.js"
  declared = page_kinds(canonical.read_text())
  for name, value in declared.items():
    if name not in kinds:
      fail("plaza_protocol.js", f"declares KIND_{name}, which plaza_wire::frame::Kind does not have")
    elif kinds[name] != value:
      fail("plaza_protocol.js", f"KIND_{name} is {value}, but Kind::{name.capitalize()} is {kinds[name]}")
  for name in sorted(set(kinds) - set(declared)):
    fail("plaza_protocol.js", f"is missing KIND_{name}, which plaza_wire::frame::Kind has")

  pages = sorted(HERE.glob("*/static/index.html"))
  if not pages:
    sys.exit("no example pages found; is this running from examples/?")

  checked = 0
  for path in pages:
    example = path.parts[-3]
    text = path.read_text()
    shared = "plaza_protocol.js" in text
    inline = page_kinds(text)
    if not shared and not inline:
      continue  # A wasm page: its framing is Rust, and check_js_imports.py covers it.
    checked += 1

    if shared:
      copy = path.parent / "plaza_protocol.js"
      if not copy.is_file():
        fail(example, "loads plaza_protocol.js but ships no copy beside the page")
      elif copy.read_bytes() != canonical.read_bytes():
        fail(example, "plaza_protocol.js copy differs from wire/js; run sync_protocol_js.sh")

    for name, value in inline.items():
      if name not in kinds:
        fail(example, f"declares KIND_{name}, which plaza_wire::frame::Kind does not have")
      elif kinds[name] != value:
        fail(example, f"KIND_{name} is {value}, but Kind::{name.capitalize()} is {kinds[name]}")

    enums, units = unit_variants(example)
    if not enums:
      # Zero unit variants and no enum found are the same answer from here, and
      # only one of them is a pass. Every op enum is named `*Op*` today; one
      # named otherwise would make this check silently vacuous.
      fail(example, "has a page but no `*Op*` enum was found, so its variants were never checked")
    if units and not re.search(r"\b(opName|variantName)\b", text):
      named = ", ".join(f"{k}::{v}" for k, vs in units.items() for v in vs)
      fail(example, f"reads ops without a variant-name helper, and {named} is a bare string on the wire")

    if "mpDecode" in text or "mpEncode" in text:
      check_codec(example, text)

  if failures:
    print(f"{len(failures)} problem(s) in the browser pages:\n")
    for f in failures:
      print(f"  {f}")
    sys.exit(1)
  print(f"{checked} browser pages check out")


if __name__ == "__main__":
  main()
