# API Reference: `plaza_ws`

## 1. Introduction & Core Concepts

`plaza_ws` is one client-side WebSocket interface, whatever is underneath. `plaza_session` covers the server and is tokio/actix by construction, so it cannot help a client, least of all a browser one. This crate is the other half: the socket a *client* holds, with the same shape on a desktop, in a browser, and in-process.

**It is built for a frame loop, not for an async runtime.** [`Socket::poll`](#method-poll) is non-blocking and drains into a caller-owned buffer. That is the whole ergonomic decision, and it is made for macroquad-style applications, which have a synchronous `loop { ...; next_frame().await }` and nowhere to put a future. An `async fn recv()` would be the natural Rust API and would be unusable there. Reusing the buffer also keeps a per-frame call allocation-free, matching how `plaza_server_utils` hands back its results.

```rust
let mut events = Vec::new();
// once per frame
socket.poll(&mut events);
for event in events.drain(..) {
  match event {
    Event::Open => println!("connected"),
    Event::Message(bytes) => println!("{} bytes", bytes.len()),
    Event::Text(text) => println!("{text}"),
    Event::Closed(reason) => println!("gone: {reason:?}"),
  }
}
```

### Backends

Each is a feature, and they compose: a native host that also plays enables `native` *and* `loopback`, and talks to both over the same trait.

| feature | where | underneath |
|---|---|---|
| `loopback` | anywhere | in-process channels, no dependencies |
| `native` | desktop | `tungstenite` on a worker thread |
| `miniquad` | browser, under macroquad | our own JS, registered as a miniquad plugin |

### Why the browser backend is ours rather than a crate

Because the constraint is the *host page's loader*, not the platform. `web-sys` (and so `gloo-net`, and so `tokio-tungstenite-wasm`) needs `wasm-bindgen`, which rewrites the module with `wasm-bindgen-cli` and ships its own JS to instantiate it. miniquad's `mq_js_bundle.js` builds its own import object, lets plugins extend it, and instantiates the raw module itself. Both want to own instantiation, so under macroquad the wasm-bindgen route does not work, and it fails in the worst way available: miniquad stubs out imports nothing provides, so such a build loads happily and then silently does nothing.

So the [`miniquad`](#7-module-miniquad-feature-miniquad-wasm32-only) backend is a few `extern "C"` declarations against our own JS plugin (`js/plaza_ws.js`), which needs no crate at all. The two crates that *do* use miniquad's plugin mechanism, `sapp-jsutils` and `quad-net`, are barely maintained, and the mechanism is small enough not to need them.

An application that is **not** built on macroquad has the opposite problem and wants the wasm-bindgen route. That is a natural fourth backend (`tokio-tungstenite-wasm` behind a `web` feature) and is deliberately absent until something needs it, rather than shipped untested.

## 2. Error Handling

### Enum `WsError`

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsError {
  Closed,
  BadUrl(String),
  Connect(String),
  Send(String),
}
```

Implements `Display` and `std::error::Error`.

*   **`Closed`**: the socket is closed; nothing further can be sent.
*   **`BadUrl(String)`**: the URL could not be parsed or its scheme is not `ws`/`wss`.
*   **`Connect(String)`**: the connection could not be established.
*   **`Send(String)`**: a send failed. In practice this variant surfaces from [`SendJson`](#trait-sendjson-feature-json) when serialization fails; transport-level send failures on a live socket arrive as an [`Event::Closed`](#enum-event) instead.

Note the split between the two ways things go wrong. `Result` covers what a call can know immediately (a bad URL, a socket already closed); everything that happens later on the wire (a failed handshake, a lost connection) arrives as an [`Event::Closed`](#enum-event) from [`poll`](#method-poll), so a frame loop has one place to handle it.

## 3. Core API

### Trait `Socket`

```rust
pub trait Socket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError>;
  fn send_text(&self, text: &str) -> Result<(), WsError>;
  fn poll(&mut self, out: &mut Vec<Event>);
  fn state(&self) -> State;
  fn close(&mut self);
  fn is_open(&self) -> bool { self.state() == State::Open }
}
```

A client-side WebSocket. Deliberately small. Anything an application can do itself (reconnection policy, backoff, heartbeats, framing of its own messages) is left to it, because those are decisions and this is a pipe.

#### Method `send`

Sends a binary frame. Sends before [`Event::Open`](#enum-event) are queued, not errors, because a frame loop should not have to hold its own outbox. Returns `Err(WsError::Closed)` once the socket has ended.

#### Method `send_text`

Sends a text frame. Same queuing and error behavior as [`send`](#method-send).

#### Method `poll`

Drains everything that has arrived since the last call, appending to `out`. Never blocks and never awaits. Appends rather than replaces, and takes the buffer rather than returning one, so a per-frame call allocates nothing after the first. Call it once per frame and drain the buffer yourself.

#### Method `state`

The socket's current [`State`](#enum-state). Cheap on every backend; the native backend mirrors it in an atomic so no lock or channel round trip is involved.

#### Method `close`

Begins a close. An [`Event::Closed`](#enum-event) follows from [`poll`](#method-poll); calling this twice is harmless.

#### Method `is_open`

Provided. `true` when [`state`](#method-state) is `State::Open`.

### Enum `Event`

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
  Open,
  Message(Vec<u8>),
  Text(String),
  Closed(CloseReason),
}
```

Something that arrived, in order.

*   **`Open`**: the handshake finished. Sends before this are queued, not errors.
*   **`Message(Vec<u8>)`**: a binary frame.
*   **`Text(String)`**: a text frame.
*   **`Closed(CloseReason)`**: terminal. No further events follow.

### Enum `CloseReason`

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
  Remote { code: u16, reason: String },
  Local,
  Error(String),
}
```

Why a socket ended.

*   **`Remote`**: the peer closed cleanly, with the code and reason it gave.
*   **`Local`**: this side called [`Socket::close`](#method-close).
*   **`Error(String)`**: the connection failed or was lost. Distinguished from a clean close because an application usually wants to reconnect after one and not the other.

### Enum `State`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
  Connecting,
  Open,
  Closed,
}
```

### Trait `SendJson` (feature `json`)

```rust
pub trait SendJson {
  fn send_json<T: serde::Serialize>(&self, value: &T) -> Result<(), WsError>;
}

impl<S: Socket + ?Sized> SendJson for S { /* serde_json::to_string, then send_text */ }
```

Sending a value as JSON text, so call sites are not full of `serde_json::to_string`. A serialization failure returns `WsError::Send`.

An extension trait rather than a method on [`Socket`](#trait-socket), because a generic method cannot be called through a trait object, and holding the socket as `Box<dyn Socket>` is exactly what an application does when the transport is chosen by feature flag. The blanket impl covers every socket, sized or not.

Text rather than binary, deliberately. A WebSocket text frame arrives in a browser as a string that `JSON.parse` accepts directly, while a binary frame arrives as a `Blob` or `ArrayBuffer` that a JS client has to decode itself, having first remembered to set `binaryType`.

Send a **bare message, never an envelope**. A server attaches who a message came from, because identity is the server's fact and not the client's claim, and a client that could name itself could name somebody else. This is the same asymmetry `plaza_wire` documents on `SessionMessage`.

### Function `connect`

```rust
pub fn connect(url: &str) -> Result<impl Socket + use<>, WsError>
```

Connects using whichever real transport this build has for its target.

The choice is never ambiguous: `native` exists only off wasm and `miniquad` only on it, so a build that enables both features (the normal shape for an application shipping a desktop and a browser client from one crate) still has exactly one real backend per target. Present when the enabled backend exists for the target: (`native`, not wasm32) or (`miniquad`, wasm32). [`loopback`](#5-module-loopback-feature-loopback-on-by-default) is never chosen here because it connects to a peer rather than to a URL.

### Function `connect_boxed`

```rust
pub fn connect_boxed(url: &str) -> Result<Box<dyn Socket>, WsError>
```

[`connect`](#function-connect), boxed: the form an application holds when the backend is decided by the build rather than written at the call site.

Unlike `connect`, this exists in **every** build. A build with no real backend gets a runtime `WsError::Connect("this build has no socket backend compiled in")` instead of a compile error, because such a build is legitimate (an offline teaching build still compiles its connect path) and every application ends up writing this same fallback arm itself.

## 4. Module `backlog`

Discarding a resume backlog before any of it is parsed. Always compiled; `trim_backlog` and `DroppedBacklog` are re-exported at the crate root.

A hidden browser tab (or a machine that slept) stops running frames while its socket keeps receiving, so the first [`Socket::poll`](#method-poll) after it wakes can hand back minutes of traffic at once. None of it is playable: a client that renders in the past is about to restart its timeline, which discards whatever those messages would have built. Parsing them anyway is where a several-second freeze on refocus comes from, so the drop happens here, on message lengths alone, before any deserialisation.

**When to call it, and when not to.** What this cannot know is whether the burst is a *resume* or a *join*: a fresh connection's first poll legitimately carries a welcome and a warm world's whole baseline, and that must arrive intact. The caller knows (it has seen a frame before, or it has not), which is why this is a function the application calls rather than something `Socket::poll` does on its own. So: call it on the polls of an established session, skip it on the first poll of a new connection.

**The contract it depends on.** Dropping unread is safe only under the recovery contract the plaza blocks implement: the client restarts its timeline and drops its mirror, its next acknowledgement carries the digest of nothing, and the server answers with a full baseline. A transport used without that contract should not use this.

### Function `trim_backlog`

```rust
pub fn trim_backlog(events: &mut Vec<Event>, trigger: usize, keep: usize) -> Option<DroppedBacklog>
```

Trims a drained event list down to its newest `keep` payload messages, if it holds more than `trigger` of them.

`None` means the list was an ordinary poll and is untouched. `Some` means it was a backlog: everything but the newest `keep` messages is gone, the caller should treat its timeline as lost, and the return value says what was discarded. [`Event::Open`](#enum-event) and [`Event::Closed`](#enum-event) are never dropped, because they carry the connection's own state; they survive in place, in order.

Pick `trigger` several times past what a running frame loop can accumulate between two polls (a few seconds of the stream's message rate), and `keep` around what one send interval holds, so the restarted timeline has something current to anchor on.

### Struct `DroppedBacklog`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DroppedBacklog {
  pub messages: u64,
  pub bytes: u64,
}
```

What a trim discarded, for the application's meters and panel. The bytes still crossed the wire: count them as received, because the meters measure the link, not what the client chose to read. `bytes` is counted from payload lengths alone; nothing was parsed.

## 5. Module `loopback` (feature `loopback`, on by default)

An in-process pair, for a host that also plays.

A listen-server has one player who is not on the network. Giving that player a different code path is how the two drift apart: the local one skips serialization, skips the ordering the wire imposes, and quietly becomes the only client that is never wrong. Handing it a [`Socket`](#trait-socket) like everyone else's means the host is testing the same client the joiners run.

It is a real pipe, not a shortcut. Bytes are serialized and copied exactly as they would be over a socket, so a bug in encoding shows up locally instead of only after someone joins. What it does not have is latency, which is the point: impairment is a separate, deliberate choice rather than an accident of being local.

```rust
use plaza_ws::{loopback, Event, Socket};

let (mut client, mut host) = loopback::pair();
client.send(b"hello").unwrap();

let mut events = Vec::new();
host.poll(&mut events);
assert_eq!(events, vec![Event::Open, Event::Message(b"hello".to_vec())]);
```

### Function `loopback::pair`

```rust
pub fn pair() -> (LoopbackSocket, LoopbackSocket)
```

Creates a connected pair. Conventionally the first is the client and the second is the host's end, but they are symmetric.

### Struct `LoopbackSocket`

One end of an in-process pair, backed by `std::sync::mpsc` channels. Behavioral notes, all chosen to match a real socket so code written against one transport works on the other:

*   [`Event::Open`](#enum-event) is delivered on the first `poll` rather than at construction, so a caller written against a real socket, which cannot be open before it has connected, sees the same sequence here.
*   Closing one end closes both immediately (the flag is shared), and a subsequent `send` on either end returns `WsError::Closed`. The far end sees `Event::Closed(CloseReason::Remote { code: 1000, reason: "" })`.
*   A peer that was *dropped* without closing reads as `Event::Closed(CloseReason::Error(..))`, not a clean close, because an application's reconnect decision turns on the difference.
*   `state()` is `Open` until closed; there is no `Connecting` phase.

## 6. Module `native` (feature `native`, non-wasm32 only)

Desktop, over `tungstenite` on a worker thread.

The thread exists to keep [`Socket::poll`](#method-poll) honest. `tungstenite` is blocking, a frame loop cannot block, and the alternative (an async runtime) would drag tokio into a client whose whole job is to render at 60 fps. So one thread (named `plaza_ws`) owns the socket and talks to the frame loop through channels. Its inner loop uses a non-blocking stream rather than a blocking read, because a blocking read cannot be interleaved with sends on the same socket; it sleeps 1 ms when there is nothing to do, which costs a millisecond of latency and avoids a spinning core.

### Function `native::connect`

```rust
pub fn connect(url: &str) -> Result<NativeSocket, WsError>
```

Connects to `url` (`ws://` or `wss://`; anything else is `WsError::BadUrl`, checked before any thread is spawned).

Returns as soon as the worker is started, not when the handshake completes, so a frame loop is never blocked by a slow or unreachable host. The socket begins in [`State::Connecting`](#enum-state); sends before [`Event::Open`](#enum-event) are queued rather than rejected, and a failure arrives as [`Event::Closed`](#enum-event).

### Struct `NativeSocket`

The frame-loop end of the worker. Further behavior, from the worker loop:

*   Queued sends are flushed before reads on each worker iteration, and a `close` goes out before anything else is attempted.
*   Ping/pong and raw frames are handled inside `tungstenite` and never surfaced as events.
*   Dropping the `NativeSocket` closes the connection politely, so the far end sees a clean goodbye instead of a reset.
*   A remote close carries the peer's code and reason (1005 when no close frame body was given, 1006 when the connection was found already closed); if the worker vanishes without reporting, `poll` reports `Event::Closed(CloseReason::Error("worker stopped"))` exactly once.

TLS (`wss://`) is available through tungstenite's `rustls-tls-webpki-roots` feature, which this crate's `native` feature enables.

## 7. Module `miniquad` (feature `miniquad`, wasm32 only)

Browser, under a macroquad/miniquad page. The socket lives in JavaScript and this is the thin Rust side of it. See `js/plaza_ws.js`, which must be included in the page after `mq_js_bundle.js` and before `load()`.

**No dependencies, by choice.** The obvious crates for this job are all `wasm-bindgen` underneath and cannot work here (see [section 1](#why-the-browser-backend-is-ours-rather-than-a-crate)), and the two crates that do use miniquad's plugin mechanism are barely maintained. The mechanism itself is a handful of `extern "C"` declarations, so the module uses the mechanism and skips the dependency.

The Rust side never allocates in JS and JS never calls back into wasm. Events are queued in JavaScript and drained on demand: ask what kind is at the front, ask how long it is, hand over a buffer, repeat. That is three crossings per event and it removes every reentrancy question, which matters because a callback into wasm during a frame could land in the middle of the borrow the frame loop is already holding.

### Function `miniquad::connect`

```rust
pub fn connect(url: &str) -> Result<MiniquadSocket, WsError>
```

Connects to `url` (`ws://` or `wss://`; anything else is `WsError::BadUrl`). Returns immediately; the browser connects in the background and [`Event::Open`](#enum-event) or [`Event::Closed`](#enum-event) arrives from a later [`poll`](#method-poll). Returns `WsError::Connect` if the `plaza_ws` JS plugin is not loaded in the page.

### Function `miniquad::page_url`

```rust
pub fn page_url() -> String
```

The WebSocket URL for the page this wasm was served from. What a browser client should almost always connect to: the host that served it. Hardcoding `127.0.0.1` works only on the machine doing the hosting, which is the one case that did not need a network.

### Struct `MiniquadSocket`

The Rust handle to a JS-side socket. A scratch buffer is reused across frames so a steady stream of messages allocates nothing. An unclean browser close (`wasClean` false) surfaces as `CloseReason::Error`, a clean one as `CloseReason::Remote` with the code and reason the browser reported.

### Function `plaza_ws_crate_version`

```rust
#[unsafe(no_mangle)]
pub extern "C" fn plaza_ws_crate_version() -> u32
```

Not for calling from Rust; it is the version export miniquad's loader checks the JS plugin against. Without it the loader logs that the plugin "is present in JS bundle, but is not used in the rust code". Exporting it turns that into a real check: a page serving an older `plaza_ws.js` than the wasm was built against now says so, instead of failing somewhere later for no visible reason.

## 8. Module `pump` (feature `pump`)

The client side of plaza's framed protocol, pumped once per frame. Owns the [`Socket`](#trait-socket), a `plaza_client_utils::Timeline`, and the kind dispatch: it schedules pings, answers the server's probes, feeds pongs to the clock estimators, sends and checks the `Hello`, and hands the application only what it owns. Pulls in `plaza_wire` (with `serde`) and `plaza_client_utils`.

### Struct `FramePump<C: WireCodec>`

```rust
impl<C: WireCodec> FramePump<C> {
  pub fn new(socket: Box<dyn Socket>, wire: C, protocol: u32) -> Self;
  pub fn connect(url: &str, wire: C, protocol: u32) -> Result<Self, WsError>;
  pub fn ping_interval_ms(self, ms: u64) -> Self;               // default PING_INTERVAL_MS = 1000

  pub fn poll(&mut self, now_ms: u64, out: &mut Vec<Arrival>);  // drain + digest in one call
  pub fn drain(&mut self, now_ms: u64, events: &mut Vec<Event>);
  pub fn digest(&mut self, events: &mut Vec<Event>, now_ms: u64, out: &mut Vec<Arrival>);

  pub fn send_ops<T: Serialize>(&mut self, ops: &[T]) -> Option<usize>;
  pub fn send_op<T: Serialize>(&mut self, op: &T) -> Option<usize>;

  pub fn timeline(&self) -> &Timeline;
  pub fn timeline_mut(&mut self) -> &mut Timeline;
  pub fn rtt_ms(&self) -> Option<f32>;
  pub fn pong_rtts(&self) -> (u64, u64);                        // last raw, worst since resume
  pub fn server_time_ms(&self, now_ms: u64) -> u64;
  pub fn on_resume(&mut self);

  pub fn bytes_sent(&self) -> u64;                              // cumulative, probes included
  pub fn bytes_received(&self) -> u64;
  pub fn messages_received(&self) -> u64;
  pub fn is_open(&self) -> bool;
  pub fn state(&self) -> State;
  pub fn close(&mut self);
}
```

`poll` is `drain` plus `digest` glued together. A client that trims a resume backlog needs its hands between the socket and the dispatch, so the two halves are also public: `drain` into a caller-owned event buffer, [`trim_backlog`](#4-module-backlog), call `on_resume` if anything was dropped, then `digest` the survivors.

`protocol` is the build's wire format number (from `plaza_wire::build`); it goes out as the `Hello` when the socket opens and is compared against the server's. `send_ops` returns the frame's wire length, or `None` if the value would not serialise. The byte counters are cumulative so a windowed meter diffs them; they count everything, probes and answers included.

### Enum `Arrival`

```rust
pub enum Arrival {
  Opened,
  Ops(OpsFrame),
  Mismatch { ours: u32, theirs: u32 },
  Closed(String),
}
```

Something the application has to act on; everything the session could finish by itself already has been. `Ops` carries the frame undecoded ([`OpsFrame::body`] feeds your codec's `decode::<Vec<Op>>`, [`OpsFrame::wire_len`] is what it cost tag byte included), because the pump cannot know your `Op` type and the decode is work worth timing where it happens. `Closed` carries the reason worded for a person.

### Function `mismatch_message`

```rust
pub fn mismatch_message(ours: u32, theirs: u32) -> String
```

The standard wording for a protocol mismatch, for the common client whose build is a cached browser bundle ("...reload to get the current client"). Word your own if yours is not.

## 9. Module `scripted` (feature `scripted`)

### Struct `ScriptedSocket`

A socket whose arrivals the test scripts: what a hidden tab's receive queue looks like from the Rust side, without a browser. `feed(Event)` / `feed_message(Vec<u8>)` queue arrivals for the next `poll`; `sent()` returns everything sent so far as raw bytes; `close_by_peer(code, reason)` flips the state and queues the `Closed` event behind whatever is already waiting, exactly as a real socket delivers it. Clones share the same queues, so the test keeps one handle while the code under test owns another as its `Box<dyn Socket>`. Pulls in `parking_lot`.

## 10. Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `loopback` | yes | Compiles [`loopback`](#5-module-loopback-feature-loopback-on-by-default). No dependencies. |
| `native` | no | Compiles [`native`](#6-module-native-feature-native-non-wasm32-only) (non-wasm32 targets only) and pulls in `tungstenite` with rustls TLS. |
| `miniquad` | no | Compiles [`miniquad`](#7-module-miniquad-feature-miniquad-wasm32-only) (wasm32 targets only). No dependencies; requires `js/plaza_ws.js` in the page. |
| `json` | no | Compiles [`SendJson`](#trait-sendjson-feature-json) and pulls in `serde` and `serde_json`. Off by default because the transport itself has no opinion about what rides on it. |
| `pump` | no | Compiles [`pump`](#8-module-pump-feature-pump) and pulls in `plaza_wire` (with `serde`) and `plaza_client_utils`. |
| `scripted` | no | Compiles [`scripted`](#9-module-scripted-feature-scripted) and pulls in `parking_lot`. Meant for `dev-dependencies`. |

The module `cfg`s combine feature and target: `native` code exists only when the target is not wasm32, and `miniquad` code only when it is, so enabling both features is safe and each build gets the one that applies, including through the free [`connect`](#function-connect) function.
