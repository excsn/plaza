//! One client-side WebSocket interface, whatever is underneath.
//!
//! `plaza_session` covers the server and is tokio/actix by construction, so it
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
//! allocation-free, matching how `plaza_server_utils` hands back its results.
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

pub use backlog::{trim_backlog, DroppedBacklog};

pub mod backlog;
#[cfg(feature = "loopback")]
pub mod loopback;
#[cfg(all(feature = "miniquad", target_arch = "wasm32"))]
pub mod miniquad;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod native;
#[cfg(feature = "pump")]
pub mod pump;
#[cfg(feature = "scripted")]
pub mod scripted;

/// Something that arrived, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
  /// The handshake finished, and [`Socket::state`] is now [`State::Open`].
  ///
  /// Sending before this arrives is **backend-dependent**, so a portable
  /// application waits for it: see [`Socket::send`].
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

/// Where a socket is in its lifecycle. Monotonic: a socket never returns to an
/// earlier state, so [`Closed`](State::Closed) is terminal and reconnecting
/// means a new socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
  /// The handshake is in flight. Not every backend has this phase:
  /// [`loopback`] is connected the moment it exists and so is never
  /// `Connecting`.
  Connecting,
  /// The handshake finished and messages can be sent.
  Open,
  /// Terminal, whether the peer closed, this side called
  /// [`Socket::close`], or the connection failed. Which of those it was is on
  /// the [`Event::Closed`] that [`Socket::poll`] delivers.
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
  /// A value could not be serialised for sending. Raised by
  /// [`SendJson::send_json`] alone: a transport that cannot deliver bytes
  /// reports [`Closed`](WsError::Closed), so this names a fault in the value
  /// rather than in the socket.
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
  /// Sends one binary message.
  ///
  /// Takes `&self` rather than `&mut self`, so a send site does not need
  /// exclusive access to a socket the frame loop is also polling.
  ///
  /// Fails with [`WsError::Closed`] once the connection is gone. **Before the
  /// handshake completes, the backends differ**, and this is the one place
  /// they do: [`native`] queues the message and delivers it on connect, while
  /// the `miniquad` backend refuses it with [`WsError::Closed`] because a browser
  /// `WebSocket` will not accept data before it opens. Portable code sends
  /// only after [`Event::Open`] (or checks [`is_open`](Self::is_open)), which
  /// is what an application wanting to know its message actually left should
  /// do regardless.
  fn send(&self, bytes: &[u8]) -> Result<(), WsError>;

  /// Sends one text message. Same rules as [`send`](Self::send).
  ///
  /// Prefer text for a JSON protocol: a text frame arrives in a browser as a
  /// string that `JSON.parse` accepts directly, where a binary frame arrives
  /// as a `Blob` or `ArrayBuffer` the client has to decode itself.
  fn send_text(&self, text: &str) -> Result<(), WsError>;

  /// Drains everything that has arrived since the last call, appending to `out`.
  ///
  /// Never blocks and never awaits. Appends rather than replaces, and takes the
  /// buffer rather than returning one, so a per-frame call allocates nothing
  /// after the first.
  fn poll(&mut self, out: &mut Vec<Event>);

  /// Where the connection is in its lifecycle, readable at any time without
  /// polling.
  ///
  /// This is the connection's own state, not a report of what [`poll`](Self::poll)
  /// has handed over: a socket can read [`State::Closed`] while its
  /// [`Event::Closed`], and any messages that arrived before it, are still
  /// queued. Drain [`poll`](Self::poll) after seeing a close rather than
  /// discarding the socket, or the last thing the peer said is lost.
  fn state(&self) -> State;

  /// Begins a close. A [`Event::Closed`] follows from [`poll`](Self::poll);
  /// calling this twice is harmless.
  fn close(&mut self);

  /// Whether messages can be sent right now. See [`send`](Self::send) for why
  /// this matters before the handshake completes.
  fn is_open(&self) -> bool {
    self.state() == State::Open
  }
}

/// Sending a value as JSON text, so call sites are not full of
/// `serde_json::to_string`.
///
/// An extension trait rather than a method on [`Socket`], because a generic
/// method cannot be called through a trait object, and holding the socket as
/// `Box<dyn Socket>` is exactly what an application does when the transport is
/// chosen by feature flag. Implemented for every socket, sized or not.
///
/// Text rather than binary, deliberately. A WebSocket text frame arrives in a
/// browser as a string that `JSON.parse` accepts directly, while a binary frame
/// arrives as a `Blob` or `ArrayBuffer` that a JS client has to decode itself,
/// having first remembered to set `binaryType`.
///
/// Send a **bare message, never an envelope**. A server attaches who a message
/// came from, because identity is the server's fact and not the client's claim,
/// and a client that could name itself could name somebody else.
#[cfg(feature = "json")]
pub trait SendJson {
  fn send_json<T: serde::Serialize>(&self, value: &T) -> Result<(), WsError>;
}

#[cfg(feature = "json")]
impl<S: Socket + ?Sized> SendJson for S {
  fn send_json<T: serde::Serialize>(&self, value: &T) -> Result<(), WsError> {
    match serde_json::to_string(value) {
      Ok(text) => self.send_text(&text),
      Err(e) => Err(WsError::Send(e.to_string())),
    }
  }
}

/// Connects using whichever real transport this build has for its target.
///
/// The choice is never ambiguous: [`native`] exists only off wasm and
/// [`miniquad`] only on it, so a build that enables both features (the normal
/// shape for an application shipping a desktop and a browser client from one
/// crate) still has exactly one real backend per target. [`loopback`] is never
/// chosen here because it connects to a peer rather than to a URL.
#[cfg(any(
  all(feature = "native", not(target_arch = "wasm32")),
  all(feature = "miniquad", target_arch = "wasm32"),
))]
pub fn connect(url: &str) -> Result<impl Socket + use<>, WsError> {
  #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
  return native::connect(url);
  #[cfg(all(feature = "miniquad", target_arch = "wasm32"))]
  return miniquad::connect(url);
}

/// [`connect`], boxed: the form an application holds when the backend is
/// decided by the build rather than written at the call site.
///
/// Unlike [`connect`], this exists in every build. A build with no real backend
/// gets a runtime [`WsError::Connect`] instead of a compile error, because such
/// a build is legitimate (an offline teaching build still compiles its connect
/// path) and every application ends up writing this same fallback arm itself.
pub fn connect_boxed(url: &str) -> Result<Box<dyn Socket>, WsError> {
  #[cfg(any(
    all(feature = "native", not(target_arch = "wasm32")),
    all(feature = "miniquad", target_arch = "wasm32"),
  ))]
  return connect(url).map(|s| Box::new(s) as Box<dyn Socket>);
  #[cfg(not(any(
    all(feature = "native", not(target_arch = "wasm32")),
    all(feature = "miniquad", target_arch = "wasm32"),
  )))]
  {
    let _ = url;
    Err(WsError::Connect("this build has no socket backend compiled in".to_owned()))
  }
}
