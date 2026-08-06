// plaza_ws: a WebSocket for macroquad/miniquad pages.
//
// Include this after mq_js_bundle.js and before load():
//
//   <script src="mq_js_bundle.js"></script>
//   <script src="plaza_ws.js"></script>
//   <script>load("your_app.wasm");</script>
//
// It registers as a miniquad plugin, which is the supported way to add imports:
// the bundle calls register_plugin(importObject) before instantiating, then
// instantiates the raw module itself. That is also why wasm-bindgen crates
// (gloo-net, web-sys, tokio-tungstenite-wasm) cannot be used here. They need
// wasm-bindgen-cli to rewrite the module and their own loader to instantiate it,
// and miniquad already owns instantiation. Worse, the bundle stubs out imports
// nothing provides, so such a build loads and then silently does nothing.
//
// Messages are queued and drained by the Rust side once per frame. Nothing here
// calls into wasm: JS never re-enters the module, it only answers when asked,
// which keeps the whole thing free of reentrancy questions.
"use strict";

(function () {
  // handle -> { ws, queue, state, }. Slots are never reused, so a stale handle
  // reads as closed rather than as somebody else's socket.
  const sockets = [];

  const CONNECTING = 0, OPEN = 1, CLOSED = 2;
  const KIND_NONE = 0, KIND_OPEN = 1, KIND_BINARY = 2, KIND_TEXT = 3, KIND_CLOSED = 4;

  const decoder = new TextDecoder("utf-8");
  const encoder = new TextEncoder();

  function read_utf8(ptr, len) {
    return decoder.decode(new Uint8Array(wasm_memory.buffer, ptr, len));
  }

  function slot_of(handle) {
    return sockets[handle];
  }

  function plaza_ws_connect(url_ptr, url_len) {
    const url = read_utf8(url_ptr, url_len);
    const slot = { ws: null, queue: [], state: CONNECTING };
    sockets.push(slot);
    const handle = sockets.length - 1;

    let ws;
    try {
      ws = new WebSocket(url);
    } catch (e) {
      // A malformed URL throws synchronously. Report it the same way a failed
      // connection arrives, so the Rust side has one path for "it did not work".
      slot.state = CLOSED;
      slot.queue.push({ kind: KIND_CLOSED, bytes: encoder.encode(String(e)), code: -1 });
      return handle;
    }

    ws.binaryType = "arraybuffer";
    slot.ws = ws;

    ws.onopen = function () {
      slot.state = OPEN;
      slot.queue.push({ kind: KIND_OPEN, bytes: new Uint8Array(0), code: 0 });
    };
    ws.onmessage = function (ev) {
      if (typeof ev.data === "string") {
        slot.queue.push({ kind: KIND_TEXT, bytes: encoder.encode(ev.data), code: 0 });
      } else {
        slot.queue.push({ kind: KIND_BINARY, bytes: new Uint8Array(ev.data), code: 0 });
      }
    };
    ws.onclose = function (ev) {
      slot.state = CLOSED;
      // A negative code marks an unclean close. The distinction matters because
      // an application reconnects after a failure and not after a goodbye, and
      // the browser gives us wasClean for exactly this.
      slot.queue.push({
        kind: KIND_CLOSED,
        bytes: encoder.encode(ev.reason || ""),
        code: ev.wasClean ? ev.code : -(ev.code || 1006),
      });
    };
    // onerror carries no detail in browsers and is always followed by onclose,
    // so there is nothing useful to report from it.
    ws.onerror = function () {};

    return handle;
  }

  function plaza_ws_send_binary(handle, ptr, len) {
    const slot = slot_of(handle);
    if (!slot || slot.state !== OPEN) return 0;
    // Copy: the view is onto wasm memory, which can be detached by a later
    // allocation growing it, and send() may be asynchronous.
    slot.ws.send(new Uint8Array(wasm_memory.buffer, ptr, len).slice());
    return 1;
  }

  function plaza_ws_send_text(handle, ptr, len) {
    const slot = slot_of(handle);
    if (!slot || slot.state !== OPEN) return 0;
    slot.ws.send(read_utf8(ptr, len));
    return 1;
  }

  function plaza_ws_peek(handle) {
    const slot = slot_of(handle);
    if (!slot || slot.queue.length === 0) return KIND_NONE;
    return slot.queue[0].kind;
  }

  function plaza_ws_peek_len(handle) {
    const slot = slot_of(handle);
    if (!slot || slot.queue.length === 0) return 0;
    return slot.queue[0].bytes.length;
  }

  function plaza_ws_peek_code(handle) {
    const slot = slot_of(handle);
    if (!slot || slot.queue.length === 0) return 0;
    return slot.queue[0].code;
  }

  // Copies the front event's payload into wasm memory and pops it. The caller
  // has already asked how long it is, so there is no truncation case.
  function plaza_ws_take(handle, ptr) {
    const slot = slot_of(handle);
    if (!slot || slot.queue.length === 0) return 0;
    const event = slot.queue.shift();
    if (event.bytes.length > 0) {
      new Uint8Array(wasm_memory.buffer, ptr, event.bytes.length).set(event.bytes);
    }
    return event.bytes.length;
  }

  function plaza_ws_state(handle) {
    const slot = slot_of(handle);
    if (!slot) return CLOSED;
    return slot.state;
  }

  function plaza_ws_close(handle) {
    const slot = slot_of(handle);
    if (!slot || slot.state === CLOSED) return;
    if (slot.ws) slot.ws.close(1000, "");
  }

  // Where this page came from, as a WebSocket URL. A browser client that
  // hardcoded 127.0.0.1 works only on the machine hosting it, which is the one
  // case where you did not need a network. Deriving it means the page served by
  // a host is already pointed at that host, over wss:// if the page was secure.
  function plaza_ws_page_url(ptr) {
    const url = (location.protocol === "https:" ? "wss:" : "ws:") + "//" + location.host + "/ws";
    const bytes = encoder.encode(url);
    new Uint8Array(wasm_memory.buffer, ptr, bytes.length).set(bytes);
    return bytes.length;
  }

  function plaza_ws_page_url_len() {
    const url = (location.protocol === "https:" ? "wss:" : "ws:") + "//" + location.host + "/ws";
    return encoder.encode(url).length;
  }

  function register_plugin(importObject) {
    importObject.env.plaza_ws_page_url = plaza_ws_page_url;
    importObject.env.plaza_ws_page_url_len = plaza_ws_page_url_len;
    importObject.env.plaza_ws_connect = plaza_ws_connect;
    importObject.env.plaza_ws_send_binary = plaza_ws_send_binary;
    importObject.env.plaza_ws_send_text = plaza_ws_send_text;
    importObject.env.plaza_ws_peek = plaza_ws_peek;
    importObject.env.plaza_ws_peek_len = plaza_ws_peek_len;
    importObject.env.plaza_ws_peek_code = plaza_ws_peek_code;
    importObject.env.plaza_ws_take = plaza_ws_take;
    importObject.env.plaza_ws_state = plaza_ws_state;
    importObject.env.plaza_ws_close = plaza_ws_close;
  }

  miniquad_add_plugin({ register_plugin: register_plugin, version: 1, name: "plaza_ws" });
})();
