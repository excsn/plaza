#!/usr/bin/env python3
"""Checks a wasm bundle's imports against the JS plugin that must satisfy them.

This exists because of one specific failure mode. miniquad's loader calls
`add_missing_functions_stabs`, which replaces any import nothing provides with a
stub. So a typo in a function name, or a plugin script the page forgot to
include, does not produce an error: the module loads, runs, and silently does
nothing. That is far harder to diagnose than a link failure, so it is worth a
check that fails loudly at build time.

    ./check_js_imports.py static/echo_web.wasm js/plaza_ws.js
"""

import re
import sys


def uleb(data, i):
  result = shift = 0
  while True:
    byte = data[i]
    i += 1
    result |= (byte & 0x7F) << shift
    if not byte & 0x80:
      return result, i
    shift += 7


def wasm_imports(path):
  """Every (module, name) the module asks the host for."""
  data = open(path, "rb").read()
  if data[:4] != b"\0asm":
    sys.exit(f"{path} is not a wasm module")
  found, i = [], 8
  while i < len(data):
    section_id = data[i]
    i += 1
    size, i = uleb(data, i)
    end = i + size
    if section_id == 2:
      count, j = uleb(data, i)
      for _ in range(count):
        length, j = uleb(data, j)
        module = data[j : j + length].decode()
        j += length
        length, j = uleb(data, j)
        name = data[j : j + length].decode()
        j += length
        kind = data[j]
        j += 1
        if kind == 0:
          _, j = uleb(data, j)
        elif kind == 1:
          j += 1
          limits = data[j]
          j += 1
          _, j = uleb(data, j)
          if limits == 1:
            _, j = uleb(data, j)
        elif kind == 2:
          limits = data[j]
          j += 1
          _, j = uleb(data, j)
          if limits == 1:
            _, j = uleb(data, j)
        elif kind == 3:
          j += 2
        found.append((module, name))
    i = end
  return found


def main():
  if len(sys.argv) != 3:
    sys.exit(__doc__)
  wasm_path, js_path = sys.argv[1], sys.argv[2]

  js = open(js_path).read()
  provided = set(re.findall(r"importObject\.env\.(\w+)\s*=", js))
  # Only ours: miniquad's own imports are satisfied by its bundle, not by us.
  wanted = {name for _, name in wasm_imports(wasm_path) if name.startswith("plaza_ws_")}

  missing = sorted(wanted - provided)
  if missing:
    print(f"{len(missing)} import(s) the wasm needs and {js_path} does not provide:")
    for name in missing:
      print(f"  {name}")
    print("\nminiquad would stub these out and the socket would silently do nothing.")
    sys.exit(1)

  # Not an error: the linker drops imports for functions the binary never calls,
  # so a plugin is expected to offer more than any one build uses.
  unused = sorted(provided - wanted)
  print(f"ok: {len(wanted)} import(s) satisfied" + (f", {len(unused)} unused in this build" if unused else ""))


if __name__ == "__main__":
  main()
