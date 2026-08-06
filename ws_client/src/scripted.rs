//! A socket whose arrivals the test scripts.
//!
//! What a hidden tab's receive queue looks like from the Rust side, without a
//! browser: the test [`feed`](ScriptedSocket::feed)s events, the code under test
//! polls them out, and everything it sends is kept for the test to inspect.
//! Clones share the same queues, so the test keeps one handle while the client
//! owns another as its `Box<dyn Socket>`.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::{CloseReason, Event, Socket, State, WsError};

#[derive(Clone, Default)]
pub struct ScriptedSocket {
  shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
  inbox: Mutex<VecDeque<Event>>,
  sent: Mutex<Vec<Vec<u8>>>,
  closed: Mutex<bool>,
}

impl ScriptedSocket {
  /// A socket that is already open: [`loopback`](crate::loopback) semantics,
  /// no handshake. Feed an [`Event::Open`] yourself to exercise one.
  pub fn new() -> Self {
    Self::default()
  }

  /// Queues one event for the next [`Socket::poll`].
  pub fn feed(&self, event: Event) {
    self.shared.inbox.lock().push_back(event);
  }

  /// Queues one binary message.
  pub fn feed_message(&self, bytes: Vec<u8>) {
    self.feed(Event::Message(bytes));
  }

  /// Everything sent so far, in order. Text is kept as its bytes.
  pub fn sent(&self) -> Vec<Vec<u8>> {
    self.shared.sent.lock().clone()
  }

  /// The peer closes: the state flips and the [`Event::Closed`] is queued
  /// behind whatever is already waiting, exactly as a real socket delivers it.
  pub fn close_by_peer(&self, code: u16, reason: &str) {
    *self.shared.closed.lock() = true;
    self.feed(Event::Closed(CloseReason::Remote {
      code,
      reason: reason.to_owned(),
    }));
  }
}

impl Socket for ScriptedSocket {
  fn send(&self, bytes: &[u8]) -> Result<(), WsError> {
    if *self.shared.closed.lock() {
      return Err(WsError::Closed);
    }
    self.shared.sent.lock().push(bytes.to_vec());
    Ok(())
  }

  fn send_text(&self, text: &str) -> Result<(), WsError> {
    self.send(text.as_bytes())
  }

  fn poll(&mut self, out: &mut Vec<Event>) {
    out.extend(self.shared.inbox.lock().drain(..));
  }

  fn state(&self) -> State {
    if *self.shared.closed.lock() {
      State::Closed
    } else {
      State::Open
    }
  }

  fn close(&mut self) {
    let was = std::mem::replace(&mut *self.shared.closed.lock(), true);
    if !was {
      self.feed(Event::Closed(CloseReason::Local));
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fed_events_come_back_in_order_and_sends_are_kept() {
    let scripted = ScriptedSocket::new();
    let mut held: Box<dyn Socket> = Box::new(scripted.clone());

    scripted.feed(Event::Open);
    scripted.feed_message(vec![1, 2]);
    held.send(&[9]).unwrap();
    held.send_text("hi").unwrap();

    let mut events = Vec::new();
    held.poll(&mut events);
    assert_eq!(events, vec![Event::Open, Event::Message(vec![1, 2])]);
    assert_eq!(scripted.sent(), vec![vec![9], b"hi".to_vec()]);
  }

  #[test]
  fn a_close_is_terminal_and_the_last_words_still_arrive() {
    let scripted = ScriptedSocket::new();
    scripted.feed_message(vec![7]);
    scripted.close_by_peer(1000, "done");

    let mut held: Box<dyn Socket> = Box::new(scripted.clone());
    assert_eq!(held.state(), State::Closed);
    assert_eq!(held.send(&[1]), Err(WsError::Closed));

    let mut events = Vec::new();
    held.poll(&mut events);
    assert_eq!(
      events,
      vec![
        Event::Message(vec![7]),
        Event::Closed(CloseReason::Remote { code: 1000, reason: "done".to_owned() })
      ]
    );
  }
}
