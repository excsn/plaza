# plaza_protocol.js

The frame layer of a plaza server, for JavaScript clients: one dependency-free file that speaks the kind byte, answers pings, and reads serde's externally-tagged ops correctly. It is the official JS counterpart of `plaza_wire::frame`; if you are writing a browser or Node client without the Rust/wasm stack (`plaza_ws`) or the Dart stack (`plaza_client`), this is the file you build on.

## Getting it

Copy the file. It is a single classic script with no dependencies and no build step, so vendoring it into your project is the supported distribution: take `wire/js/plaza_protocol.js` from the plaza repo at the version you target and check it into your static assets. Every plaza example that ships a browser page serves its copy beside the page, so against a running example you can also fetch it as `/plaza_protocol.js` from the same origin.

## Using it

In a browser, load it before your own script and wire each socket's `onmessage`:

```html
<script src="plaza_protocol.js"></script>
<script>
  const ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onmessage = (e) => onJsonFrame(ws, e.data, (op) => {
    switch (opName(op)) {
      case 'Snapshot': render(opBody(op)); break;
    }
  });
  ws.send(jsonFrame(KIND_OPS, [{ MovePaddle: { target_y: 12 } }]));
</script>
```

In Node, `require` it and hand it any WebSocket with browser-shaped `send`/`readyState`, such as the `ws` package:

```js
const { onJsonFrame, jsonFrame, opName, opBody, KIND_OPS } = require('./plaza_protocol.js');
const ws = new WebSocket('ws://localhost:8090/ws');
ws.on('message', (data) => onJsonFrame(ws, data.toString(), handleOp));
```

For a server whose session declares a binary codec, set `binaryType = 'arraybuffer'` and use `onBinaryFrame`/`binaryFrame`, supplying the body codec yourself as `{ encode, decode }`. The file does not ship a MessagePack codec; `examples/parlour_game/static/index.html` carries a minimal named-MessagePack one you can lift.

## What it covers, and what it doesn't

It covers the frame contract: the kind byte ahead of every message, the skip-unknown-kinds rule that makes additive protocol changes safe, answering `Ping` so the server can measure your round trip, the optional `Hello` carrying the server's protocol version, and the `opName`/`opBody` helpers without which every fieldless enum variant is silently dropped (serde writes those as bare strings, not one-key maps). It does not cover reconnection, op scheduling, prediction, or any application codec beyond JSON; those live in your client or in the bigger client stacks.

## Versioning

`PLAZA_PROTOCOL_JS_VERSION` in the file is the artifact's own version and moves semver-style with the file's API. It is not the protocol version in `Hello`: that number is your application's, produced by `plaza_wire::build` from your op type definitions, and this file cannot know it. A served page usually has nothing to compare a `Hello` against and skips it; a shipped Node client can pass `onHello` and decide for itself.

The kind byte values are mirrored from `plaza_wire::frame::Kind` and pinned by `examples/check_pages.py`, which fails if this file and the Rust enum disagree, and also verifies that every example page's copy is byte-identical to this one (`examples/sync_protocol_js.sh` refreshes the copies).
