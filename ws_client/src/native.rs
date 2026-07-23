//! Desktop, over `tungstenite` on a worker thread.
//!
//! The thread exists to keep [`Socket::poll`] honest. `tungstenite` is
//! blocking, a frame loop cannot block, and the alternative (an async runtime)
//! would drag tokio into a client whose whole job is to render at 60 fps. So one
//! thread owns the socket and talks to the frame loop through channels.
//!
//! Its inner loop uses a non-blocking stream rather than a blocking read,
//! because a blocking read cannot be interleaved with sends on the same socket:
//! `tungstenite::WebSocket` is a single object with no split, and a thread
//! parked in `read()` would hold it for as long as the peer stayed quiet. It
//! sleeps briefly when there is nothing to do, which costs a millisecond of
//! latency and avoids a spinning core.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::{CloseReason, Event, Socket, State, WsError};

/// How long the worker sleeps when neither side has anything.
const IDLE_NAP: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub struct NativeSocket {
  outbound: Sender<Command>,
  inbound: Receiver<Event>,
  /// Mirrors [`State`] so `state()` needs no lock and no channel round trip.
  state: Arc<AtomicU8>,
}

#[derive(Debug)]
enum Command {
  Binary(Vec<u8>),
  Text(String),
  Close,
}

const CONNECTING: u8 = 0;
const OPEN: u8 = 1;
const CLOSED: u8 = 2;

/// Connects to `url` (`ws://` or `wss://`).
///
/// Returns as soon as the worker is started, not when the handshake completes,
/// so a frame loop is never blocked by a slow or unreachable host. The socket
/// begins [`State::Connecting`]; sends before [`Event::Open`] are queued rather
/// than rejected, and a failure arrives as [`Event::Closed`].
pub fn connect(url: &str) -> Result<NativeSocket, WsError> {
  if !(url.starts_with("ws://") || url.starts_with("wss://")) {
    return Err(WsError::BadUrl(url.to_owned()));
  }
  let (cmd_tx, cmd_rx) = mpsc::channel();
  let (evt_tx, evt_rx) = mpsc::channel();
  let state = Arc::new(AtomicU8::new(CONNECTING));

  let worker_state = Arc::clone(&state);
  let target = url.to_owned();
  thread::Builder::new()
    .name("plaza_ws".to_owned())
    .spawn(move || run(target, cmd_rx, evt_tx, worker_state))
    .map_err(|e| WsError::Connect(e.to_string()))?;

  Ok(NativeSocket {
    outbound: cmd_tx,
    inbound: evt_rx,
    state,
  })
}

fn run(url: String, commands: Receiver<Command>, events: Sender<Event>, state: Arc<AtomicU8>) {
  let mut socket = match tungstenite::connect(&url) {
    Ok((socket, _response)) => socket,
    Err(e) => {
      state.store(CLOSED, Ordering::Release);
      let _ = events.send(Event::Closed(CloseReason::Error(e.to_string())));
      return;
    }
  };

  if let Err(e) = set_nonblocking(&mut socket) {
    state.store(CLOSED, Ordering::Release);
    let _ = events.send(Event::Closed(CloseReason::Error(e)));
    return;
  }

  state.store(OPEN, Ordering::Release);
  if events.send(Event::Open).is_err() {
    return;
  }

  loop {
    let mut worked = false;

    // Sends first: a caller that just queued something should not wait a nap for
    // it, and a close should go out before anything else is attempted.
    loop {
      match commands.try_recv() {
        Ok(Command::Binary(bytes)) => {
          worked = true;
          if socket.send(Message::Binary(bytes)).is_err() {
            return finish(&state, &events, CloseReason::Error("send failed".to_owned()));
          }
        }
        Ok(Command::Text(text)) => {
          worked = true;
          if socket.send(Message::Text(text)).is_err() {
            return finish(&state, &events, CloseReason::Error("send failed".to_owned()));
          }
        }
        Ok(Command::Close) => {
          let _ = socket.close(None);
          let _ = socket.flush();
          return finish(&state, &events, CloseReason::Local);
        }
        Err(TryRecvError::Empty) => break,
        // The handle was dropped. Close politely rather than abandoning the
        // socket, so the far end sees a clean goodbye instead of a reset.
        Err(TryRecvError::Disconnected) => {
          let _ = socket.close(None);
          let _ = socket.flush();
          return;
        }
      }
    }

    match socket.read() {
      Ok(Message::Binary(bytes)) => {
        worked = true;
        if events.send(Event::Message(bytes.to_vec())).is_err() {
          return;
        }
      }
      Ok(Message::Text(text)) => {
        worked = true;
        if events.send(Event::Text(text.to_string())).is_err() {
          return;
        }
      }
      // Ping/pong are answered by tungstenite itself on the next write; frames
      // an application did not ask for are not worth surfacing.
      Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => worked = true,
      Ok(Message::Close(frame)) => {
        let (code, reason) = frame.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new()));
        let _ = socket.flush();
        return finish(&state, &events, CloseReason::Remote { code, reason });
      }
      Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
      Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
        return finish(&state, &events, CloseReason::Remote { code: 1006, reason: String::new() });
      }
      Err(e) => return finish(&state, &events, CloseReason::Error(e.to_string())),
    }

    // Anything tungstenite buffered while the stream was busy.
    if let Err(tungstenite::Error::Io(e)) = socket.flush()
      && e.kind() != ErrorKind::WouldBlock
    {
      return finish(&state, &events, CloseReason::Error(e.to_string()));
    }

    if !worked {
      thread::sleep(IDLE_NAP);
    }
  }
}

fn finish(state: &Arc<AtomicU8>, events: &Sender<Event>, reason: CloseReason) {
  state.store(CLOSED, Ordering::Release);
  let _ = events.send(Event::Closed(reason));
}

/// Reaches through whatever TLS wrapper is in play to the `TcpStream`.
fn set_nonblocking(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<(), String> {
  let stream = match socket.get_mut() {
    MaybeTlsStream::Plain(s) => s,
    #[cfg(feature = "native")]
    MaybeTlsStream::Rustls(s) => s.get_mut(),
    other => return Err(format!("unsupported stream kind: {other:?}")),
  };
  stream.set_nonblocking(true).map_err(|e| e.to_string())
}

impl Socket for NativeSocket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError> {
    self
      .outbound
      .send(Command::Binary(bytes.to_vec()))
      .map_err(|_| WsError::Closed)
  }

  fn send_text(&self, text: &str) -> Result<(), WsError> {
    self.outbound.send(Command::Text(text.to_owned())).map_err(|_| WsError::Closed)
  }

  fn poll(&mut self, out: &mut Vec<Event>) {
    loop {
      match self.inbound.try_recv() {
        Ok(event) => {
          if matches!(event, Event::Closed(_)) {
            self.state.store(CLOSED, Ordering::Release);
          }
          out.push(event);
        }
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
          // The worker is gone. Report it once, and only if nobody has already
          // been told the socket ended.
          if self.state.swap(CLOSED, Ordering::AcqRel) != CLOSED {
            out.push(Event::Closed(CloseReason::Error("worker stopped".to_owned())));
          }
          return;
        }
      }
    }
  }

  fn state(&self) -> State {
    match self.state.load(Ordering::Acquire) {
      CONNECTING => State::Connecting,
      OPEN => State::Open,
      _ => State::Closed,
    }
  }

  fn close(&mut self) {
    let _ = self.outbound.send(Command::Close);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_url_that_is_not_a_websocket_url_is_rejected_before_a_thread_is_spawned() {
    assert_eq!(connect("http://example.com").unwrap_err(), WsError::BadUrl("http://example.com".to_owned()));
    assert_eq!(connect("example.com").unwrap_err(), WsError::BadUrl("example.com".to_owned()));
  }

  #[test]
  fn connecting_to_nothing_reports_a_close_rather_than_hanging() {
    // The frame loop must never be blocked by an unreachable host, so `connect`
    // returns immediately and the failure arrives as an event like any other.
    // Port 1 on loopback refuses fast and needs no network.
    let mut socket = connect("ws://127.0.0.1:1").expect("returns without connecting");
    let mut out = Vec::new();
    for _ in 0..200 {
      socket.poll(&mut out);
      if !out.is_empty() {
        break;
      }
      thread::sleep(Duration::from_millis(10));
    }
    assert!(
      matches!(out.first(), Some(Event::Closed(CloseReason::Error(_)))),
      "expected a failed connect, got {out:?}"
    );
    assert_eq!(socket.state(), State::Closed);
  }
}
