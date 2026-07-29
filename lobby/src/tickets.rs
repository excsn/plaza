//! One-use tickets, so a room learns who connected without asking them.
//!
//! Fills [`JoinRoomOutcomePayload::player_game_token`](crate::op_payloads::JoinRoomOutcomePayload),
//! which otherwise has nothing to put in it. The lobby already knows who it just
//! admitted; without carrying that across, a room's only source for the
//! connecting player's identity is the client, and a client that can name its
//! own id can name somebody else's.
//!
//! ```ignore
//! // In the lobby, after a successful admission:
//! let token = tickets.issue(player, room_id);
//! let endpoint = format!("{}?t={token}", handle.session_endpoint_info());
//!
//! // In the room's connect route:
//! let Some(ticket) = tickets.redeem(&token) else { return unauthorized() };
//! if ticket.room != room_id { return forbidden() }
//! session.handle_connection(&req, stream, Agent::new_human(ticket.player))
//! ```
//!
//! # This is placement, not authentication
//!
//! [`issue`](TicketRegistry::issue) mints a counter, which is guessable in one
//! try. It is enough to stop a client *naming* another player, which is the
//! failure this closes, and it is not a credential. Anything facing untrusted
//! clients should mint its own signed, expiring value and hand it to
//! [`issue_with`](TicketRegistry::issue_with); the registry does not care what
//! the string is.
//!
//! Plaza has no authentication story for this to be consistent with, which is
//! why the crate provides the bookkeeping and not the secret.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use plaza::agent::AgentId;

use crate::types::RoomId;

/// What one ticket entitles its bearer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket<ID: AgentId> {
  pub player: ID,
  pub room: RoomId,
}

/// Outstanding tickets. Shared between the lobby that issues and the route that
/// redeems, so it is internally synchronised rather than `&mut`.
#[derive(Debug, Default)]
pub struct TicketRegistry<ID: AgentId> {
  issued: Mutex<HashMap<String, Ticket<ID>>>,
  next: AtomicU64,
}

impl<ID: AgentId> TicketRegistry<ID> {
  pub fn new() -> Self {
    Self {
      issued: Mutex::new(HashMap::new()),
      next: AtomicU64::new(0),
    }
  }

  /// Mints a ticket with a generated token. See the module docs: the token is a
  /// counter, not a secret.
  pub fn issue(&self, player: ID, room: RoomId) -> String {
    let token = format!("t{}", self.next.fetch_add(1, Ordering::Relaxed));
    self.issue_with(token.clone(), player, room);
    token
  }

  /// Records a ticket under a token you minted, for anything that needs a real
  /// credential. Replaces any existing entry for that token.
  pub fn issue_with(&self, token: String, player: ID, room: RoomId) {
    self.issued.lock().insert(token, Ticket { player, room });
  }

  /// Spends a ticket, or `None` if it was never issued or has been used.
  ///
  /// One use, so a token that leaks cannot be replayed into a second connection
  /// alongside the one that already holds it.
  pub fn redeem(&self, token: &str) -> Option<Ticket<ID>> {
    self.issued.lock().remove(token)
  }

  /// Drops a ticket without spending it, for an admission the lobby has since
  /// cancelled.
  pub fn revoke(&self, token: &str) -> bool {
    self.issued.lock().remove(token).is_some()
  }

  /// Tickets handed out and not yet dialled. A number that climbs rather than
  /// hovering means placements are being issued and abandoned.
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
    let token = tickets.issue(7u32, room);
    let ticket = tickets.redeem(&token).expect("issued");
    assert_eq!(ticket.player, 7);
    assert_eq!(ticket.room, room);
  }

  #[test]
  fn a_ticket_spends_once() {
    let tickets = TicketRegistry::new();
    let token = tickets.issue(7u32, Uuid::new_v4());
    assert!(tickets.redeem(&token).is_some());
    assert!(tickets.redeem(&token).is_none(), "replay is refused");
    assert_eq!(tickets.outstanding(), 0);
  }

  #[test]
  fn an_unissued_token_resolves_to_nobody() {
    let tickets: TicketRegistry<u32> = TicketRegistry::new();
    assert!(tickets.redeem("t999").is_none());
  }

  #[test]
  fn tickets_are_distinct_per_issue() {
    let tickets = TicketRegistry::new();
    let room = Uuid::new_v4();
    assert_ne!(tickets.issue(1u32, room), tickets.issue(1u32, room));
  }

  #[test]
  fn a_caller_can_supply_its_own_token() {
    let tickets = TicketRegistry::new();
    let room = Uuid::new_v4();
    tickets.issue_with("signed.jwt.value".to_string(), 7u32, room);
    assert_eq!(tickets.redeem("signed.jwt.value").unwrap().player, 7);
  }

  #[test]
  fn a_revoked_ticket_cannot_be_redeemed() {
    let tickets = TicketRegistry::new();
    let token = tickets.issue(7u32, Uuid::new_v4());
    assert!(tickets.revoke(&token));
    assert!(tickets.redeem(&token).is_none());
  }

  #[test]
  fn outstanding_tracks_what_was_issued_and_not_dialled() {
    let tickets = TicketRegistry::new();
    let room = Uuid::new_v4();
    let a = tickets.issue(1u32, room);
    tickets.issue(2u32, room);
    tickets.redeem(&a);
    assert_eq!(tickets.outstanding(), 1);
  }
}
