// plaza_protocol: the frame layer of a plaza server, for JavaScript clients.
//
// A frame is one kind byte, then the encoded body. On a text socket the kind
// rides as the first character and the body is JSON; on a binary socket the
// kind is the first byte and the body is whatever codec the server declared.
// An unknown kind is skipped rather than treated as an error, so a server can
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

const PLAZA_PROTOCOL_JS_VERSION = "0.3.0";

// Frame kinds, mirroring plaza_wire::frame::Kind. Pinned to the Rust enum by
// examples/check_pages.py.
const KIND_OPS = 0;
const KIND_HELLO = 1;
const KIND_PING = 2;
const KIND_PONG = 3;
const KIND_CREDENTIAL = 4;
const KIND_GOODBYE = 5;

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

// Presents a credential to a server whose route did not resolve identity:
// one Credential frame, sent from `onopen` right after `announceHello` and
// before anything else, since a data frame ahead of it closes the socket. The
// body is opaque to plaza: a string on a text socket, a byte array with
// `codec` on a binary one.
function announceCredential(sock, credential, codec) {
  if (codec) {
    sock.send(binaryFrame(KIND_CREDENTIAL, Uint8Array.from(credential)));
  } else {
    sock.send(String.fromCharCode(KIND_CREDENTIAL) + credential);
  }
}

// The server's Goodbye, written last before every close it orders: `code` is
// the close code and `detail` whatever the server added. Kept on the socket
// so `onclose` can read it through `closeCodeOf`, since a proxy may not carry
// the close frame's code through and the goodbye is the same number.
function keepGoodbye(sock, goodbye) {
  sock.plazaGoodbye = goodbye;
}

// The close code for an `onclose` event: the goodbye's if one arrived, else
// the event's own. 4000 to 4999 is the server refusing this client on
// purpose, so do not reconnect with the same credential; 1006 is the link
// failing with no close frame at all, which is worth reconnecting.
function closeCodeOf(sock, event) {
  return (sock.plazaGoodbye && sock.plazaGoodbye.code) || (event && event.code) || 0;
}

// The default reaction to a server's Hello: reload once when the page's stamped
// version differs from the server's. Guarded so a still-mismatched reload (a
// cached page, a proxy) degrades to a console error instead of a loop. No-op
// when either side's version is unknown or outside a browser.
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
// so a stamped page reacts to a redeploy without writing anything. A Goodbye
// goes to `onGoodbye` when given, else is kept for `closeCodeOf`.
function onJsonFrame(sock, data, onOp, onHello, onGoodbye) {
  const kind = data.charCodeAt(0);
  if (kind === KIND_GOODBYE) {
    (onGoodbye || ((g) => keepGoodbye(sock, g)))(JSON.parse(data.slice(1)));
    return;
  }
  if (kind === KIND_PING) {
    // Echo the stamp untouched so the server can measure a browser client's
    // round trip.
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
function onBinaryFrame(sock, data, codec, onOp, onHello, onGoodbye) {
  const bytes = new Uint8Array(data);
  const kind = bytes[0];
  const body = bytes.subarray(1);
  if (kind === KIND_GOODBYE) {
    (onGoodbye || ((g) => keepGoodbye(sock, g)))(codec.decode(body));
    return;
  }
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
    KIND_CREDENTIAL,
    KIND_GOODBYE,
    announceCredential,
    closeCodeOf,
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
