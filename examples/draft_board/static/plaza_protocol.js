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

const PLAZA_PROTOCOL_JS_VERSION = "0.2.0";

// Frame kinds, mirroring plaza_wire::frame::Kind. Pinned to the Rust enum by
// examples/check_pages.py.
const KIND_OPS = 0;
const KIND_HELLO = 1;
const KIND_PING = 2;
const KIND_PONG = 3;

const opName = (op) => (typeof op === "string" ? op : Object.keys(op)[0]);
const opBody = (op) => (typeof op === "string" ? {} : op[opName(op)]);

// The protocol version this client speaks. A served page is stamped with it
// (`Host::protocol` injects `window.PLAZA_PROTOCOL`); anything else can set the
// global itself. 0 means "unknown", which announces nothing and agrees with
// everything, the same contract as `ProtocolVersion::UNKNOWN`.
function ownProtocol() {
  return (typeof globalThis !== "undefined" && globalThis.PLAZA_PROTOCOL) || 0;
}

// Announces this client's version, the same once-on-open Hello every plaza
// peer sends. Call from the socket's `onopen`. Silent when the version is
// unknown, mirroring the Rust session. Pass `codec` on a binary socket.
function announceHello(sock, codec) {
  const version = ownProtocol();
  if (!version) return;
  if (codec) {
    sock.send(binaryFrame(KIND_HELLO, Uint8Array.from(codec.encode(version))));
  } else {
    sock.send(jsonFrame(KIND_HELLO, version));
  }
}

// The default reaction to a server's Hello: reload once when the page provably
// outlived the server it was stamped by. Guarded so a still-mismatched reload
// (a cached page, a proxy) degrades to a console error instead of a loop.
// No-op when either side's version is unknown, and outside a browser.
function staleCheck(theirs) {
  const mine = ownProtocol();
  if (!mine || !theirs || theirs === mine) return;
  if (typeof location === "undefined") return;
  const key = "plaza-protocol-reload-" + theirs;
  if (typeof sessionStorage !== "undefined" && sessionStorage.getItem(key)) {
    console.error("server speaks protocol " + theirs + ", this page was served for " + mine + "; reloading did not resolve it");
    return;
  }
  if (typeof sessionStorage !== "undefined") sessionStorage.setItem(key, "1");
  location.reload();
}

function jsonFrame(kind, value) {
  return String.fromCharCode(kind) + JSON.stringify(value);
}

// One received text frame: answers pings, hands each op to `onOp`, skips the
// rest. The server's Hello goes to `onHello` when given, else to `staleCheck`,
// so a stamped page reacts to a redeploy without writing anything.
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
    (onHello || staleCheck)(JSON.parse(data.slice(1)));
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
    (onHello || staleCheck)(codec.decode(body));
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
    ownProtocol,
    announceHello,
    staleCheck,
    jsonFrame,
    onJsonFrame,
    binaryFrame,
    onBinaryFrame,
  };
}
