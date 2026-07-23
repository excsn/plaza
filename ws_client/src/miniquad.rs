//! Browser, under a macroquad/miniquad page.
//!
//! The socket lives in JavaScript and this is the thin Rust side of it. See
//! `js/plaza_ws.js`, which must be included in the page after
//! `mq_js_bundle.js` and before `load()`.
//!
//! **No dependencies, by choice.** The obvious crates for this job are all
//! `wasm-bindgen` underneath and cannot work here (see the crate docs), and the
//! two crates that do use miniquad's plugin mechanism, `sapp-jsutils` and
//! `quad-net`, are barely maintained. The mechanism itself is a handful of
//! `extern "C"` declarations, so we use the mechanism and skip the dependency.
//!
//! The Rust side never allocates in JS and JS never calls back into wasm.
//! Events are queued in JavaScript and drained on demand: ask what kind is at
//! the front, ask how long it is, hand over a buffer, repeat. That is three
//! crossings per event and it removes every reentrancy question, which matters
//! because a callback into wasm during a frame could land in the middle of the
//! borrow the frame loop is already holding.

use crate::{CloseReason, Event, Socket, State, WsError};

const KIND_NONE: i32 = 0;
const KIND_OPEN: i32 = 1;
const KIND_BINARY: i32 = 2;
const KIND_TEXT: i32 = 3;
const KIND_CLOSED: i32 = 4;

unsafe extern "C" {
  fn plaza_ws_connect(url_ptr: *const u8, url_len: u32) -> i32;
  fn plaza_ws_send_binary(handle: i32, ptr: *const u8, len: u32) -> i32;
  fn plaza_ws_send_text(handle: i32, ptr: *const u8, len: u32) -> i32;
  fn plaza_ws_peek(handle: i32) -> i32;
  fn plaza_ws_peek_len(handle: i32) -> u32;
  fn plaza_ws_peek_code(handle: i32) -> i32;
  fn plaza_ws_take(handle: i32, ptr: *mut u8) -> u32;
  fn plaza_ws_state(handle: i32) -> i32;
  fn plaza_ws_close(handle: i32);
  fn plaza_ws_page_url(ptr: *mut u8) -> u32;
  fn plaza_ws_page_url_len() -> u32;
}

/// The version miniquad checks the JS plugin against.
///
/// The loader looks for `<plugin name>_crate_version` and, finding nothing,
/// logs that the plugin "is present in JS bundle, but is not used in the rust
/// code". Exporting it turns that into a real check: a page serving an older
/// `plaza_ws.js` than the wasm was built against now says so, instead of failing
/// somewhere later for no visible reason.
#[unsafe(no_mangle)]
pub extern "C" fn plaza_ws_crate_version() -> u32 {
  1
}

/// The WebSocket URL for the page this wasm was served from.
///
/// What a browser client should almost always connect to: the host that served
/// it. Hardcoding `127.0.0.1` works only on the machine doing the hosting, which
/// is the one case that did not need a network.
pub fn page_url() -> String {
  // Safety: the length is asked for first and JS writes exactly that many bytes
  // into a buffer with capacity for them.
  unsafe {
    let len = plaza_ws_page_url_len() as usize;
    let mut buffer = Vec::with_capacity(len);
    let written = plaza_ws_page_url(buffer.as_mut_ptr()) as usize;
    buffer.set_len(written.min(len));
    String::from_utf8_lossy(&buffer).into_owned()
  }
}

#[derive(Debug)]
pub struct MiniquadSocket {
  handle: i32,
  /// Reused across frames so a steady stream of messages allocates nothing.
  scratch: Vec<u8>,
}

/// Connects to `url` (`ws://` or `wss://`).
///
/// Returns immediately; the browser connects in the background and
/// [`Event::Open`] or [`Event::Closed`] arrives from a later
/// [`poll`](Socket::poll).
pub fn connect(url: &str) -> Result<MiniquadSocket, WsError> {
  if !(url.starts_with("ws://") || url.starts_with("wss://")) {
    return Err(WsError::BadUrl(url.to_owned()));
  }
  // Safety: the pointer and length describe `url`, which outlives the call, and
  // the JS side copies out of wasm memory before returning.
  let handle = unsafe { plaza_ws_connect(url.as_ptr(), url.len() as u32) };
  if handle < 0 {
    return Err(WsError::Connect("the plaza_ws JS plugin is not loaded".to_owned()));
  }
  Ok(MiniquadSocket { handle, scratch: Vec::new() })
}

impl Socket for MiniquadSocket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError> {
    let sent = unsafe { plaza_ws_send_binary(self.handle, bytes.as_ptr(), bytes.len() as u32) };
    if sent == 0 { Err(WsError::Closed) } else { Ok(()) }
  }

  fn send_text(&self, text: &str) -> Result<(), WsError> {
    let sent = unsafe { plaza_ws_send_text(self.handle, text.as_ptr(), text.len() as u32) };
    if sent == 0 { Err(WsError::Closed) } else { Ok(()) }
  }

  fn poll(&mut self, out: &mut Vec<Event>) {
    loop {
      let kind = unsafe { plaza_ws_peek(self.handle) };
      if kind == KIND_NONE {
        return;
      }
      let len = unsafe { plaza_ws_peek_len(self.handle) } as usize;
      let code = unsafe { plaza_ws_peek_code(self.handle) };

      self.scratch.clear();
      self.scratch.reserve(len);
      // Safety: the buffer has capacity for `len` bytes, JS writes exactly that
      // many (it told us the length a moment ago and the queue is not touched in
      // between, because JS only runs when we call it), and every byte in the
      // range is initialised by the copy before the length is set.
      let written = unsafe {
        let written = plaza_ws_take(self.handle, self.scratch.as_mut_ptr()) as usize;
        self.scratch.set_len(written.min(len));
        written
      };
      debug_assert_eq!(written, len, "js wrote a different length than it reported");

      match kind {
        KIND_OPEN => out.push(Event::Open),
        KIND_BINARY => out.push(Event::Message(self.scratch.clone())),
        KIND_TEXT => out.push(Event::Text(String::from_utf8_lossy(&self.scratch).into_owned())),
        KIND_CLOSED => {
          let reason = String::from_utf8_lossy(&self.scratch).into_owned();
          // JS marks an unclean close by negating the code, because the browser
          // distinguishes them through `wasClean` and an application's decision
          // to reconnect turns on it.
          out.push(Event::Closed(if code < 0 {
            CloseReason::Error(if reason.is_empty() {
              format!("connection failed ({})", -code)
            } else {
              reason
            })
          } else {
            CloseReason::Remote { code: code as u16, reason }
          }));
          return;
        }
        _ => return,
      }
    }
  }

  fn state(&self) -> State {
    match unsafe { plaza_ws_state(self.handle) } {
      0 => State::Connecting,
      1 => State::Open,
      _ => State::Closed,
    }
  }

  fn close(&mut self) {
    unsafe { plaza_ws_close(self.handle) };
  }
}
