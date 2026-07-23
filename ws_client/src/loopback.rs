//! An in-process pair, for a host that also plays.
//!
//! A listen-server has one player who is not on the network. Giving that player
//! a different code path is how the two drift apart: the local one skips
//! serialization, skips the ordering the wire imposes, and quietly becomes the
//! only client that is never wrong. Handing it a [`Socket`] like everyone
//! else's means the host is testing the same client the joiners run.
//!
//! It is a real pipe, not a shortcut. Bytes are serialized and copied exactly as
//! they would be over a socket, so a bug in encoding shows up locally instead of
//! only after someone joins. What it does not have is latency, which is the
//! point: impairment is a separate, deliberate choice rather than an accident of
//! being local.
//!
//! ```
//! use plaza_ws::{loopback, Event, Socket};
//!
//! let (mut client, mut host) = loopback::pair();
//! client.send(b"hello").unwrap();
//!
//! let mut events = Vec::new();
//! host.poll(&mut events);
//! assert_eq!(events, vec![Event::Open, Event::Message(b"hello".to_vec())]);
//! ```

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use crate::{CloseReason, Event, Socket, State, WsError};

/// One end of an in-process pair.
pub struct LoopbackSocket {
  outbound: Sender<Wire>,
  inbound: Receiver<Wire>,
  /// Shared so both ends agree the moment either closes, without a round trip.
  closed: Arc<Mutex<bool>>,
  /// [`Event::Open`] is delivered on the first poll rather than at construction,
  /// so a caller written against a real socket, which cannot be open before it
  /// has connected, sees the same sequence here.
  announced_open: bool,
}

enum Wire {
  Binary(Vec<u8>),
  Text(String),
  Close,
}

/// Creates a connected pair. Conventionally the first is the client and the
/// second is the host's end, but they are symmetric.
pub fn pair() -> (LoopbackSocket, LoopbackSocket) {
  let (a_tx, b_rx) = mpsc::channel();
  let (b_tx, a_rx) = mpsc::channel();
  let closed = Arc::new(Mutex::new(false));
  (
    LoopbackSocket {
      outbound: a_tx,
      inbound: a_rx,
      closed: Arc::clone(&closed),
      announced_open: false,
    },
    LoopbackSocket {
      outbound: b_tx,
      inbound: b_rx,
      closed,
      announced_open: false,
    },
  )
}

impl LoopbackSocket {
  fn is_closed(&self) -> bool {
    *self.closed.lock().expect("loopback close flag poisoned")
  }

  fn mark_closed(&self) {
    *self.closed.lock().expect("loopback close flag poisoned") = true;
  }
}

impl Socket for LoopbackSocket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError> {
    if self.is_closed() {
      return Err(WsError::Closed);
    }
    self.outbound.send(Wire::Binary(bytes.to_vec())).map_err(|_| WsError::Closed)
  }

  fn send_text(&self, text: &str) -> Result<(), WsError> {
    if self.is_closed() {
      return Err(WsError::Closed);
    }
    self.outbound.send(Wire::Text(text.to_owned())).map_err(|_| WsError::Closed)
  }

  fn poll(&mut self, out: &mut Vec<Event>) {
    if !self.announced_open {
      self.announced_open = true;
      out.push(Event::Open);
    }
    loop {
      match self.inbound.try_recv() {
        Ok(Wire::Binary(bytes)) => out.push(Event::Message(bytes)),
        Ok(Wire::Text(text)) => out.push(Event::Text(text)),
        Ok(Wire::Close) => {
          self.mark_closed();
          out.push(Event::Closed(CloseReason::Remote {
            code: 1000,
            reason: String::new(),
          }));
          return;
        }
        Err(TryRecvError::Empty) => return,
        // The far end was dropped without closing. A real socket reports that as
        // a failure rather than a clean close, and so does this, because an
        // application's reconnect decision turns on the difference.
        Err(TryRecvError::Disconnected) => {
          if !self.is_closed() {
            self.mark_closed();
            out.push(Event::Closed(CloseReason::Error("loopback peer dropped".to_owned())));
          }
          return;
        }
      }
    }
  }

  fn state(&self) -> State {
    if self.is_closed() { State::Closed } else { State::Open }
  }

  fn close(&mut self) {
    if self.is_closed() {
      return;
    }
    let _ = self.outbound.send(Wire::Close);
    self.mark_closed();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn drain(socket: &mut LoopbackSocket) -> Vec<Event> {
    let mut out = Vec::new();
    socket.poll(&mut out);
    out
  }

  #[test]
  fn open_is_announced_once_on_the_first_poll() {
    // A caller written against a real socket waits for `Open` before sending. If
    // the loopback were open at construction, that caller would work over the
    // network and hang locally, or the reverse. Same sequence, both transports.
    let (mut client, _host) = pair();
    assert_eq!(drain(&mut client), vec![Event::Open]);
    assert_eq!(drain(&mut client), vec![]);
  }

  #[test]
  fn messages_cross_in_both_directions_and_in_order() {
    let (mut client, mut host) = pair();
    client.send(b"one").unwrap();
    client.send_text("two").unwrap();
    host.send(b"three").unwrap();

    assert_eq!(
      drain(&mut host),
      vec![Event::Open, Event::Message(b"one".to_vec()), Event::Text("two".to_owned())]
    );
    assert_eq!(drain(&mut client), vec![Event::Open, Event::Message(b"three".to_vec())]);
  }

  #[test]
  fn poll_appends_rather_than_replacing() {
    // The buffer belongs to the caller, so a frame loop can keep one and never
    // allocate again. Replacing its contents would quietly drop anything the
    // caller had not consumed yet.
    let (mut client, host) = pair();
    host.send(b"x").unwrap();
    let mut out = vec![Event::Text("kept".to_owned())];
    client.poll(&mut out);
    assert_eq!(out[0], Event::Text("kept".to_owned()));
    assert_eq!(out.len(), 3);
  }

  #[test]
  fn closing_one_end_closes_the_other_and_sending_then_fails() {
    let (mut client, mut host) = pair();
    drain(&mut host);
    client.close();

    assert_eq!(client.state(), State::Closed);
    assert_eq!(client.send(b"late"), Err(WsError::Closed));
    assert_eq!(
      drain(&mut host),
      vec![Event::Closed(CloseReason::Remote {
        code: 1000,
        reason: String::new()
      })]
    );
    assert_eq!(host.state(), State::Closed);
  }

  #[test]
  fn closing_twice_is_harmless() {
    let (mut client, _host) = pair();
    client.close();
    client.close();
    assert_eq!(client.state(), State::Closed);
  }

  #[test]
  fn a_dropped_peer_reads_as_an_error_not_a_clean_close() {
    // The distinction an application reconnects on. A peer that vanished is a
    // failure; a peer that said goodbye is not.
    let (mut client, host) = pair();
    drop(host);
    let events = drain(&mut client);
    assert_eq!(events[0], Event::Open);
    assert!(
      matches!(events[1], Event::Closed(CloseReason::Error(_))),
      "expected an error close, got {:?}",
      events[1]
    );
  }
}
