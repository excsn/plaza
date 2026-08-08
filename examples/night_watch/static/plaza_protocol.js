// plaza_protocol: the frame layer of a plaza server, for JavaScript clients.
//
// A frame is one kind byte, then the encoded body. On a text socket the kind
// rides as the first character and the body is JSON; on a binary socket the
// kind is the first byte and the body is whatever codec the server declared.
// An unknown kind is skipped, never an error: that rule is what lets a server
// add frame kinds without breaking deployed clients.
//
// Ops are serde externally-tagged enums. A struct variant arrives as a
// one-key object, a *unit* variant as a bare string; a client that only ever
// reads `op.Something` silently drops every unit variant. Use `opName`/`opBody`.
//
// Browser: <script src="plaza_protocol.js"></script>, then wire a socket:
//
//   ws.onmessage = (e) => onJsonFrame(ws, e.data, (op) => { ... });
//   ws.send(jsonFrame(KIND_OPS, [{ MovePaddle: { target_y: 12 } }]));
//
// Node: const { onJsonFrame, jsonFrame, KIND_OPS } = require("./plaza_protocol.js");
// works against any WebSocket with browser-shaped send/readyState (e.g. `ws`).
//
// See README.md beside this file for the full contract and versioning.
"use strict";

const PLAZA_PROTOCOL_JS_VERSION = "0.1.0";

// Frame kinds, mirroring plaza_wire::frame::Kind. Pinned to the Rust enum by
// examples/check_pages.py.
const KIND_OPS = 0;
const KIND_HELLO = 1;
const KIND_PING = 2;
const KIND_PONG = 3;

const opName = (op) => (typeof op === "string" ? op : Object.keys(op)[0]);
const opBody = (op) => (typeof op === "string" ? {} : op[opName(op)]);

function jsonFrame(kind, value) {
  return String.fromCharCode(kind) + JSON.stringify(value);
}

// One received text frame: answers pings, hands each op to `onOp`, skips the
// rest. `onHello` is optional; the body is the server's ProtocolVersion, which
// a served page usually has nothing to compare against.
function onJsonFrame(sock, data, onOp, onHello) {
  const kind = data.charCodeAt(0);
  if (kind === KIND_PING) {
    // Echo the stamp untouched: this is what lets the server report a round
    // trip for a browser client at all.
    const ping = JSON.parse(data.slice(1));
    if (sock.readyState === 1) {
      sock.send(jsonFrame(KIND_PONG, { origin: ping.origin, responder: null }));
    }
    return;
  }
  if (kind === KIND_HELLO) {
    if (onHello) onHello(JSON.parse(data.slice(1)));
    return;
  }
  if (kind !== KIND_OPS) return;
  let ops;
  try {
    ops = JSON.parse(data.slice(1));
  } catch (e) {
    console.error("undecodable ops frame", e);
    return;
  }
  (ops || []).forEach(onOp);
}

function binaryFrame(kind, body) {
  const framed = new Uint8Array(body.length + 1);
  framed[0] = kind;
  framed.set(body, 1);
  return framed;
}

// `onJsonFrame` for a binary socket (set `binaryType = 'arraybuffer'`).
// `codec` supplies the body encoding: { encode: value -> byte array,
// decode: Uint8Array -> value }.
function onBinaryFrame(sock, data, codec, onOp, onHello) {
  const bytes = new Uint8Array(data);
  const kind = bytes[0];
  const body = bytes.subarray(1);
  if (kind === KIND_PING) {
    const ping = codec.decode(body);
    if (sock.readyState === 1) {
      const reply = codec.encode({ origin: ping.origin, responder: null });
      sock.send(binaryFrame(KIND_PONG, Uint8Array.from(reply)));
    }
    return;
  }
  if (kind === KIND_HELLO) {
    if (onHello) onHello(codec.decode(body));
    return;
  }
  if (kind !== KIND_OPS) return;
  let ops;
  try {
    ops = codec.decode(body);
  } catch (e) {
    console.error("undecodable ops frame", e);
    return;
  }
  (ops || []).forEach(onOp);
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = {
    PLAZA_PROTOCOL_JS_VERSION,
    KIND_OPS,
    KIND_HELLO,
    KIND_PING,
    KIND_PONG,
    opName,
    opBody,
    jsonFrame,
    onJsonFrame,
    binaryFrame,
    onBinaryFrame,
  };
}
