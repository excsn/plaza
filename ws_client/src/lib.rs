//! One client-side WebSocket interface, whatever is underneath.
//!
//! [`plaza_session`] covers the server and is tokio/actix by construction, so it
//! cannot help a client, and least of all a browser one. This crate is the other
//! half: the socket a *client* holds, with the same shape on a desktop, in a
//! browser, and in-process.
//!
//! # It is built for a frame loop, not for an async runtime
//!
//! [`Socket::poll`] is non-blocking and drains into a caller-owned buffer. That
//! is the whole ergonomic decision, and it is made for macroquad-style
//! applications, which have a synchronous `loop { ...; next_frame().await }` and
//! nowhere to put a future. An `async fn recv()` would be the natural Rust API
//! and would be unusable there. Reusing the buffer also keeps a per-frame call
//! allocation-free, matching how [`plaza_server_utils`] hands back its results.
//!
//! ```no_run
//! # use plaza_ws::{Socket, Event};
//! # fn demo(socket: &mut impl Socket) {
//! let mut events = Vec::new();
//! // once per frame
//! socket.poll(&mut events);
//! for event in events.drain(..) {
//!   match event {
//!     Event::Open => println!("connected"),
//!     Event::Message(bytes) => println!("{} bytes", bytes.len()),
//!     Event::Text(text) => println!("{text}"),
//!     Event::Closed(reason) => println!("gone: {reason:?}"),
//!   }
//! }
//! # }
//! ```
//!
//! # Backends
//!
//! Each is a feature, and they compose: a native host that also plays enables
//! `native` *and* `loopback`, and talks to both over the same trait.
//!
//! | feature | where | underneath |
//! |---|---|---|
//! | `loopback` | anywhere | in-process channels, no dependencies |
//! | `native` | desktop | `tungstenite` on a worker thread |
//! | `miniquad` | browser, under macroquad | our own JS, registered as a miniquad plugin |
//!
//! ## Why the browser backend is ours rather than a crate
//!
//! Because the constraint is the *host page's loader*, not the platform.
//!
//! `web-sys` (and so `gloo-net`, and so `tokio-tungstenite-wasm`) needs
//! `wasm-bindgen`, which rewrites the module with `wasm-bindgen-cli` and ships
//! its own JS to instantiate it. miniquad's `mq_js_bundle.js` builds its own
//! import object, lets plugins extend it, and instantiates the raw module
//! itself. Both want to own instantiation, so under macroquad the wasm-bindgen
//! route does not work, and it fails in the worst way available: miniquad stubs
//! out imports nothing provides, so such a build loads happily and then silently
//! does nothing.
//!
//! So the `miniquad` backend is a few `extern "C"` declarations against our own
//! JS plugin (`js/plaza_ws.js`), which needs no crate at all. The two crates
//! that *do* use miniquad's plugin mechanism, `sapp-jsutils` and `quad-net`, are
//! barely maintained, and the mechanism is small enough not to need them.
//!
//! An application that is **not** built on macroquad has the opposite problem
//! and wants the wasm-bindgen route. That is a natural fourth backend
//! (`tokio-tungstenite-wasm` behind a `web` feature) and is deliberately absent
//! until something needs it, rather than shipped untested.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::fmt;

#[cfg(feature = "loopback")]
pub mod loopback;
#[cfg(all(feature = "miniquad", target_arch = "wasm32"))]
pub mod miniquad;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod native;

/// Something that arrived, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
  /// The handshake finished. Sends before this are queued, not errors, because
  /// a frame loop should not have to hold its own outbox.
  Open,
  Message(Vec<u8>),
  Text(String),
  /// Terminal. No further events follow.
  Closed(CloseReason),
}

/// Why a socket ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
  /// The peer closed cleanly, with the code and reason it gave.
  Remote { code: u16, reason: String },
  /// This side called [`Socket::close`].
  Local,
  /// The connection failed or was lost. Distinguished from a clean close
  /// because an application usually wants to reconnect after one and not the
  /// other.
  Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
  Connecting,
  Open,
  Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsError {
  /// The socket is closed; nothing further can be sent.
  Closed,
  /// The URL could not be parsed or its scheme is not `ws`/`wss`.
  BadUrl(String),
  /// The connection could not be established.
  Connect(String),
  Send(String),
}

impl fmt::Display for WsError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      WsError::Closed => write!(f, "socket is closed"),
      WsError::BadUrl(url) => write!(f, "not a usable websocket url: {url}"),
      WsError::Connect(e) => write!(f, "could not connect: {e}"),
      WsError::Send(e) => write!(f, "could not send: {e}"),
    }
  }
}

impl std::error::Error for WsError {}

/// A client-side WebSocket.
///
/// Deliberately small. Anything an application can do itself (reconnection
/// policy, backoff, heartbeats, framing of its own messages) is left to it,
/// because those are decisions and this is a pipe.
pub trait Socket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError>;

  fn send_text(&self, text: &str) -> Result<(), WsError>;

  /// Drains everything that has arrived since the last call, appending to `out`.
  ///
  /// Never blocks and never awaits. Appends rather than replaces, and takes the
  /// buffer rather than returning one, so a per-frame call allocates nothing
  /// after the first.
  fn poll(&mut self, out: &mut Vec<Event>);

  fn state(&self) -> State;

  /// Begins a close. A [`Event::Closed`] follows from [`poll`](Self::poll);
  /// calling this twice is harmless.
  fn close(&mut self);

  fn is_open(&self) -> bool {
    self.state() == State::Open
  }
}

/// Connects using whichever real transport this build has.
///
/// Present only when exactly one real backend is enabled, so that a build cannot
/// silently pick a transport the author did not intend. With several, name the
/// one you want: [`native::connect`] or [`miniquad::connect`]. [`loopback`] is
/// never chosen here because it connects to a peer rather than to a URL.
#[cfg(any(
  all(feature = "native", not(target_arch = "wasm32"), not(feature = "miniquad")),
  all(feature = "miniquad", target_arch = "wasm32", not(feature = "native")),
))]
pub fn connect(url: &str) -> Result<impl Socket, WsError> {
  #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
  return native::connect(url);
  #[cfg(all(feature = "miniquad", target_arch = "wasm32"))]
  return miniquad::connect(url);
}
