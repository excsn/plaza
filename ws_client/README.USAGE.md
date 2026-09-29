# Usage Guide: plaza_ws

How to hold a WebSocket in a frame loop: polling it, choosing a backend for desktop or browser or in-process, running the pump that handles a plaza server's protocol for you, wiring the browser plugin into a macroquad page and testing without a network.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [Connecting and Polling](#connecting-and-polling)
    *   [The Pump](#the-pump)
*   [Holding a Socket](#holding-a-socket)
    *   [Polling Once a Frame](#polling-once-a-frame)
    *   [Sending](#sending)
    *   [Closing](#closing)
    *   [Holding One Behind a Feature Flag](#holding-one-behind-a-feature-flag)
*   [Choosing a Backend](#choosing-a-backend)
    *   [Desktop](#desktop)
    *   [Browser Under Macroquad](#browser-under-macroquad)
    *   [In-Process](#in-process)
*   [Running the Pump](#running-the-pump)
    *   [What It Handles for You](#what-it-handles-for-you)
    *   [Reading the Link](#reading-the-link)
    *   [Splitting Poll Around a Resume](#splitting-poll-around-a-resume)
    *   [Answering a Protocol Mismatch](#answering-a-protocol-mismatch)
*   [Wiring the Browser Page](#wiring-the-browser-page)
    *   [Loading Order](#loading-order)
    *   [Checking the Imports](#checking-the-imports)
*   [Testing Without a Network](#testing-without-a-network)
    *   [A Scripted Socket](#a-scripted-socket)
    *   [A Real Round Trip](#a-real-round-trip)
*   [Design Notes](#design-notes)
*   [Error Handling](#error-handling)

## Core Concepts

*   **`Socket`**: the one trait every backend implements. Non-blocking, polled, no futures.
*   **`Event`**: what arrived: `Open`, `Message(bytes)`, `Text(string)`, `Closed(reason)`.
*   **Backend**: where the socket actually lives. `native` off wasm, `miniquad` on it, `loopback` anywhere.
*   **`FramePump`**: the loop every client of a `plaza_session` server would otherwise write: probes, kind-byte dispatch, the `Hello` check, the clock estimators.
*   **`Arrival`**: what the pump hands back after keeping the protocol traffic: `Opened`, `Ops`, `Mismatch`, `Closed`.
*   **Credential**: bytes the pump sends straight after its `Hello`, for a server that admits connections by what the client presents.
*   **`Closed`**: how a pumped connection ended: the server's goodbye code, its undecoded detail and a reason worded for a person. `refused()` is true for a 4xxx code.
*   **`Timeline`**: the pump's own round trip, clock fit and newest-stamp floor, from `plaza_client_utils`.
*   **`ScriptedSocket`**: the test double. Feed events in, read sent bytes out.

## Quick Start

### Connecting and Polling

```rust,ignore
use plaza_ws::{connect, Event};

let mut socket = connect("ws://127.0.0.1:9001")?;
let mut events = Vec::new();

// Once per frame.
socket.poll(&mut events);
for event in events.drain(..) {
  match event {
    Event::Open => socket.send_text("hello")?,
    Event::Message(bytes) => decode(&bytes),
    Event::Text(text) => println!("{text}"),
    Event::Closed(reason) => println!("closed: {reason:?}"),
  }
}
```

### The Pump

Against a `plaza_session` server, use this instead and never write the protocol loop.

```rust,ignore
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};

let mut pump = FramePump::connect(&url, MsgPackCodec, PROTOCOL)?.credential(token);
let mut arrivals = Vec::new();

// Once per frame.
pump.poll(now_ms, &mut arrivals);
for arrival in arrivals.drain(..) {
  match arrival {
    Arrival::Opened => {
      pump.send_ops(&[Op::Join { name }]);
    }
    Arrival::Ops(frame) => apply(codec.decode::<Vec<Op>>(frame.body())?),
    Arrival::Mismatch { ours, theirs } => banner(mismatch_message(ours, theirs)),
    Arrival::Closed(closed) if closed.refused() => sign_in_again(closed.code, closed.detail),
    Arrival::Closed(closed) => reconnect_later(closed.reason),
  }
}
```

`credential` is for a server that admits connections through a `ConnectionAdmitter`: the pump sends it straight after its `Hello`, before anything else. `Closed` carries the server's goodbye: its `code`, its undecoded `detail` and a `reason` worded for a person. `refused()` is true for a 4xxx code, which is the server deciding, so reconnecting with the same credential will be refused again.

## Holding a Socket

### Polling Once a Frame

```rust,ignore
socket.poll(&mut events);
```

Non-blocking and it drains into a buffer you own, so a per-frame call allocates nothing. Reuse the same `Vec` every frame and `drain(..)` it.

### Sending

```rust,ignore
socket.send(&bytes)?;
socket.send_text("a string")?;
```

With the `json` feature, `SendJson` adds one more by blanket impl over `Socket`:

```rust,ignore
use plaza_ws::SendJson;
socket.send_json(&my_op)?;
```

It is an **extension trait** rather than a trait method because a generic method cannot be called through a trait object and `Box<dyn Socket>` is exactly what an application holds when its transport is chosen by feature flag.

### Closing

```rust,ignore
socket.close();
```

Reconnection policy, backoff, heartbeats and your own message framing are all yours; the socket only moves bytes.

### Holding One Behind a Feature Flag

```rust,ignore
let mut socket: Box<dyn Socket> = plaza_ws::connect_boxed(&url)?;
```

`connect_boxed` exists in every build. With no backend compiled in it reports "this build has no socket backend compiled in" at runtime, because an offline teaching build still has to compile its connect path.

## Choosing a Backend

| feature | where | underneath | dependencies |
|---|---|---|---|
| `loopback` | anywhere | in-process channels | none |
| `native` | desktop | `tungstenite` on a worker thread | `tungstenite` |
| `miniquad` | browser, under macroquad | our own JS plugin | none |

`connect` picks whichever real backend this build has for its target and the choice is never ambiguous: `native` exists only off wasm and `miniquad` only on it, so enabling both, the normal shape for a crate shipping a desktop and a browser client, still leaves exactly one per target.

### Desktop

```toml
plaza_ws = { version = "0.6", features = ["native"] }
```

One thread owns the socket and talks to the frame loop over channels, because `tungstenite` is blocking, a frame loop cannot block and an async runtime would drag tokio into a program whose job is to render at 60 fps. It uses a non-blocking stream rather than a blocking read, since `tungstenite::WebSocket` has no split and a thread parked in `read()` would hold the socket for as long as the peer stayed quiet.

### Browser Under Macroquad

```toml
plaza_ws = { version = "0.6", features = ["miniquad"] }
```

The plugin's functions and miniquad's are provided by JavaScript when the page loads, so the module leaves them undefined at link time. From rustc 1.98, `wasm32-unknown-unknown` no longer turns undefined symbols into imports and the link fails with `undefined symbol: init_webgl` or `plaza_ws_connect`. Put the flag in the app's `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["-C", "link-arg=--import-undefined"]
```

See [Wiring the Browser Page](#wiring-the-browser-page): the plugin JS has to be loaded before the module is instantiated.

### In-Process

A host that plays has one player who is not on the network.

```rust,ignore
let (client_side, server_side) = plaza_ws::loopback::pair();
```

**Why the local player gets a socket.** Giving the local player a different code path is how the two drift apart: it skips serialization and the ordering the wire imposes, so it never shows the bugs a networked client would. `pair()` hands back a real `Socket`, so the host exercises the same client the joiners run. Bytes are copied exactly as they would be over a socket. It adds no latency on purpose, so impairment has to be set deliberately.

The backends can be combined: a listen-server that also plays enables `native` **and** `loopback` and talks to both through the same trait.

## Running the Pump

### What It Handles for You

Every client of a `plaza_session` server needs the same loop. `FramePump` implements it once, over any `WireCodec`:

*   schedules a `Kind::Ping` and answers the server's,
*   splits each message on its kind byte,
*   feeds pongs to the clock estimators,
*   checks the server's `Hello` against your own protocol version,
*   hands the application only what it owns.

### Reading the Link

```rust,ignore
let rtt = pump.timeline().rtt.rtt();
let server_now = pump.timeline().server_time_ms(now_ms);

let (up, down) = (pump.bytes_sent(), pump.bytes_received());
```

It counts every byte both ways, so a bandwidth panel diffs its counters instead of counting at every call site.

### Splitting Poll Around a Resume

`drain` and `digest` are the two halves of `poll`, split so a backlog trim can run between them.

```rust,ignore
pump.drain(now_ms, &mut events);
if plaza_ws::trim_backlog(&mut events, TRIGGER, KEEP).is_some() {
  pump.on_resume();   // a resumed tab's lump: what the estimators learned is stale
}
pump.digest(&mut events, now_ms, &mut arrivals);
```

### Answering a Protocol Mismatch

```rust,ignore
Arrival::Mismatch { ours, theirs } => {
  banner(mismatch_message(ours, theirs));   // "reload, this page is stale"
}
```

## Wiring the Browser Page

### Loading Order

Order matters. The bundle defines `miniquad_add_plugin`, the plugin registers against it and `load()` instantiates once both are in place.

```html
<script src="mq_js_bundle.js"></script>
<script src="plaza_ws.js"></script>
<script>load("your_app.wasm");</script>
```

### Checking the Imports

miniquad's bundle calls `add_missing_functions_stabs`, which replaces any import nothing provides with a stub, so a page missing the plugin **loads without any error and then does nothing**.

```sh
python3 check_js_imports.py target/wasm32-unknown-unknown/release/your_app.wasm js/plaza_ws.js
```

It parses the built wasm's import section and fails if the plugin does not satisfy it. `serve.sh` runs it on every build; run it on any bundle using this backend.

## Testing Without a Network

### A Scripted Socket

```toml
[dev-dependencies]
plaza_ws = { version = "0.6", features = ["scripted"] }
```

```rust,ignore
use plaza_ws::scripted::ScriptedSocket;

let mut socket = ScriptedSocket::new();
socket.feed(Event::Open);
socket.feed_message(server_frame);

let mut events = Vec::new();
socket.poll(&mut events);

assert_eq!(socket.sent(), vec![expected_bytes]);
```

The pump's own tests use it.

### A Real Round Trip

```sh
# desktop
cargo run -p plaza_ws --features native --example echo_server -- 9001
cargo run -p plaza_ws --features native --example echo -- ws://127.0.0.1:9001

# browser: builds, checks imports, serves the page and an echo server
cd ws_client && ./serve.sh          # then open http://localhost:8090
```

The browser spike is a macroquad app on purpose: miniquad's loader is what is under test and a plain wasm module would not exercise it. The result shows on screen: green means open, binary, text and close all arrived.

## Design Notes

**`poll` rather than `async fn recv()`.** A macroquad program is a synchronous `loop { ...; next_frame().await }` with nowhere to put a future, so an async API would be unusable there. Taking the buffer rather than returning one keeps a per-frame call allocation-free.

**No `web-sys`, so no `gloo-net` and no `tokio-tungstenite-wasm`.** All of them need `wasm-bindgen`, whose CLI rewrites the module and ships its own JS to instantiate it, while miniquad's bundle builds its own import object and instantiates the raw module itself. Both want to own instantiation, so under macroquad the wasm-bindgen route does not work. The two crates that do use miniquad's plugin mechanism are barely maintained and the mechanism is a handful of `extern "C"` declarations, so this uses the mechanism and skips the dependency.

An application **not** built on macroquad has the opposite problem and wants the wasm-bindgen route. It would be a fourth backend behind a `web` feature, left out until something needs it so it is not shipped untested.

## Error Handling

`connect` and `connect_boxed` return `Result`: a bad URL, a refused connection or no backend compiled in for this target.

```rust,ignore
let mut socket = match plaza_ws::connect(&url) {
  Ok(socket) => socket,
  Err(e) => return banner(format!("could not reach {url}: {e}")),
};
```

Everything after that is an `Event` rather than a `Result`. A socket that fails while open reports `Event::Closed(reason)` on the next `poll`, so a frame loop handles a drop in the same place it handles a clean close and there is no error path that can be forgotten between frames.

`send` on a closed socket returns `Err(WsError::Closed)` rather than panicking, so a frame loop that discovers the close one frame later can ignore that error on the frame in between.

The pump reports a stale build as `Arrival::Mismatch` rather than an error, since the connection works and the peer is only a different build. What to do about it is the application's and `mismatch_message` writes the sentence if you want it.
