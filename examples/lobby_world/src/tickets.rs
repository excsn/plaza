//! One-use tickets filling `JoinRoomOutcomePayload::player_game_token`: a client
//! that can name its own id in the arena URL can name someone else's.
//!
//! The value is a counter. This shows where the check goes, not how to build a
//! credential.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::types::{PlayerId, RoomId};

#[derive(Debug, Clone, Copy)]
pub struct Ticket {
  pub player: PlayerId,
  pub room: RoomId,
}

#[derive(Default)]
pub struct TicketRegistry {
  issued: Mutex<HashMap<String, Ticket>>,
  next: AtomicU64,
}

impl TicketRegistry {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn issue(&self, player: PlayerId, room: RoomId) -> String {
    let token = format!("t{}", self.next.fetch_add(1, Ordering::Relaxed));
    self.issued.lock().insert(token.clone(), Ticket { player, room });
    token
  }

  /// Spends the ticket, so a leaked token cannot be replayed alongside the
  /// connection that already used it.
  pub fn redeem(&self, token: &str) -> Option<Ticket> {
    self.issued.lock().remove(token)
  }

  pub fn outstanding(&self) -> usize {
    self.issued.lock().len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use uuid::Uuid;

  #[test]
  fn a_ticket_resolves_to_who_it_was_issued_for() {
    let tickets = TicketRegistry::new();
    let room = Uuid::new_v4();
    let token = tickets.issue(7, room);
    let ticket = tickets.redeem(&token).expect("issued");
    assert_eq!(ticket.player, 7);
    assert_eq!(ticket.room, room);
  }

  #[test]
  fn a_ticket_spends_once() {
    let tickets = TicketRegistry::new();
    let token = tickets.issue(7, Uuid::new_v4());
    assert!(tickets.redeem(&token).is_some());
    assert!(tickets.redeem(&token).is_none());
    assert_eq!(tickets.outstanding(), 0);
  }

  #[test]
  fn an_unissued_token_resolves_to_nobody() {
    let tickets = TicketRegistry::new();
    assert!(tickets.redeem("t999").is_none());
  }

  #[test]
  fn tickets_are_distinct_per_issue() {
    let tickets = TicketRegistry::new();
    let room = Uuid::new_v4();
    assert_ne!(tickets.issue(1, room), tickets.issue(1, room));
  }
}
