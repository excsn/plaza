# `plaza_ws`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

One client-side WebSocket interface across a desktop, a browser, and in-process. [`plaza_session`](../session/) covers the server and is tokio/actix by construction, so it cannot help a client and least of all a browser one. This is the other half.

## Install

```toml
[dependencies]
plaza_ws = { version = "0.6", features = ["native", "loopback"] }
```

## The shape, and why

```rust,ignore
let mut events = Vec::new();
// once per frame
socket.poll(&mut events);
for event in events.drain(..) {
  match event {
    Event::Open => {}
    Event::Message(bytes) => {}
    Event::Text(text) => {}
    Event::Closed(reason) => {}
  }
}
```

`poll` is non-blocking and drains into a caller-owned buffer. That is the one ergonomic decision the crate makes, and it is made for frame-loop applications: a macroquad program is a synchronous `loop { ...; next_frame().await }` with nowhere to put a future, so an `async fn recv()` would be the natural Rust API and unusable there. Taking the buffer rather than returning one keeps a per-frame call allocation-free, matching how `plaza_server_utils` hands back its results.

Everything an application can decide for itself is left to it: reconnection policy, backoff, heartbeats, its own message framing. This is a pipe.

`SendJson` (feature `json`) adds `send_json` by blanket impl over `Socket`, as an **extension trait** rather than a trait method. That is not stylistic: a generic method cannot be called through a trait object, and `Box<dyn Socket>` is exactly what an application holds when its transport is chosen by feature flag.

## Backends

| feature | where | underneath | dependencies |
|---|---|---|---|
| `loopback` | anywhere | in-process channels | none |
| `native` | desktop | `tungstenite` on a worker thread | `tungstenite` |
| `miniquad` | browser, under macroquad | our own JS plugin | none |

`connect` picks whichever real backend this build has for its target, and the choice is never ambiguous: `native` exists only off wasm and `miniquad` only on it, so enabling both (the normal shape for a crate shipping a desktop and a browser client) still leaves exactly one per target. `connect_boxed` is the same choice as a `Box<dyn Socket>`, and it exists in every build: with no backend it reports "no socket backend compiled in" at runtime, because an offline teaching build still has to compile its connect path.

They compose. A listen-server that also plays enables `native` **and** `loopback` and talks to both through the same trait.

## The pump (feature `pump`)

Every client of a `plaza_session` server repeats the same loop: schedule a ping, split each message on its kind byte, answer the server's probes, feed pongs to the clock estimators, check the `Hello` against its own protocol, and hand everything else to the application. `pump::FramePump` is that loop written once, over any `WireCodec`. What comes out of `poll` is only what the application owns:

```rust,ignore
let mut pump = FramePump::connect(&url, MsgPackCodec, PROTOCOL)?;
let mut arrivals = Vec::new();
// once per frame
pump.poll(now_ms, &mut arrivals);
for arrival in arrivals.drain(..) {
  match arrival {
    Arrival::Opened => { /* ask to join */ }
    Arrival::Ops(frame) => { /* decode frame.body() with your codec */ }
    Arrival::Mismatch { ours, theirs } => { /* a stale build: see mismatch_message */ }
    Arrival::Closed(reason) => { /* say why */ }
  }
}
```

It owns the socket and a `plaza_client_utils::Timeline`, so the round trip, the clock fit and the newest-stamp floor are read from it, and it counts every byte both ways so a bandwidth panel diffs its counters instead of taping a meter to every call site. `drain`/`digest` are the two halves of `poll`, split so a resume-backlog trim can run between them. `scripted::ScriptedSocket` (feature `scripted`, for `dev-dependencies`) is the test double the pump's own tests use: feed events in, read sent bytes out.

### `loopback` is not a shortcut

A host that plays has one player who is not on the network. Giving that player a different code path is how the two drift apart: the local one skips serialization, skips the ordering the wire imposes, and quietly becomes the only client that is never wrong. `loopback::pair()` hands it a real `Socket`, so the host is exercising the same client the joiners run. Bytes are copied exactly as they would be over a socket; what it lacks is latency, which is the point, because impairment should be a deliberate choice rather than an accident of being local.

### `native` runs on a thread, deliberately

`tungstenite` is blocking, a frame loop cannot block, and an async runtime would drag tokio into a program whose job is to render at 60 fps. So one thread owns the socket and talks to the frame loop over channels. It uses a non-blocking stream rather than a blocking read, because `tungstenite::WebSocket` has no split and a thread parked in `read()` would hold the socket for as long as the peer stayed quiet.

### `miniquad`, and why not gloo-net

`web-sys`, and therefore `gloo-net` and `tokio-tungstenite-wasm`, needs `wasm-bindgen`: `wasm-bindgen-cli` rewrites the module and ships its own JS to instantiate it. miniquad's `mq_js_bundle.js` builds its own import object, lets plugins extend it, and instantiates the raw module itself. Both want to own instantiation, so under macroquad the wasm-bindgen route does not work.

It fails in the worst way available. The bundle calls `add_missing_functions_stabs`, which replaces any import nothing provides with a stub, so such a build **loads happily and then silently does nothing**. There is no error to read.

So this backend is a handful of `extern "C"` declarations against [`js/plaza_ws.js`](js/plaza_ws.js), which needs no crate at all. The two crates that do use miniquad's plugin mechanism, `sapp-jsutils` and `quad-net`, are barely maintained, and the mechanism is small enough not to need them.

Because a missing import is silent, [`check_js_imports.py`](check_js_imports.py) parses the built wasm's import section and fails loudly if the plugin does not satisfy it. `serve.sh` runs it on every build, and it is worth running on any bundle that uses this backend.

An application that is **not** built on macroquad has the opposite problem and wants the wasm-bindgen route. That is a natural fourth backend behind a `web` feature; it is deliberately absent until something needs it rather than shipped untested.

## Using it in a macroquad page

Order matters. The bundle defines `miniquad_add_plugin`, the plugin registers against it, and `load()` instantiates once both are in place:

```html
<script src="mq_js_bundle.js"></script>
<script src="plaza_ws.js"></script>
<script>load("your_app.wasm");</script>
```

## Verifying it

```sh
# desktop round trip
cargo run -p plaza_ws --features native --example echo_server -- 9001
cargo run -p plaza_ws --features native --example echo -- ws://127.0.0.1:9001

# browser round trip: builds, checks imports, serves the page and an echo server
cd ws_client && ./serve.sh          # then open http://localhost:8090
```

The browser spike is a macroquad app on purpose: miniquad's loader is the thing under test, and a plain wasm module would prove nothing about the case that matters. The screen is the assertion, and green means open, binary, text and close all arrived.
