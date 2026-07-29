//! Seats promised to players who have not arrived yet.
//!
//! A lobby admits a player, and some time later that player opens a socket to
//! the room. In between, the room has to be able to tell an admitted player from
//! anyone else who happens to connect, or its capacity is decided by whoever
//! dials fastest and the lobby's accounting is decorative.
//!
//! Keep one of these in the room's state and drive it:
//!
//! ```ignore
//! match input {
//!   LogicInput::AgentJoined { agent } => {
//!     let id = agent.id_cloned().unwrap();
//!     // Admitted *and* still room: the two checks are not one, because the
//!     // lobby's capacity check and this connect are not atomic.
//!     let seat = if state.reserved.consume(&id) && state.seated() < state.max {
//!       Seat::Player
//!     } else {
//!       Seat::Spectator
//!     };
//!   }
//!   LogicInput::AgentLeft { .. } => {
//!     // Deliberately nothing. See below.
//!   }
//! }
//! ```
//!
//! # A closing socket must not cancel a reservation
//!
//! This is the whole reason the type exists rather than a bare `HashSet`, and it
//! was found the expensive way. When a player moves from one room to another,
//! the lobby reserves the new seat and *then* the client closes its old socket,
//! so the room sees `AgentLeft` after the reservation was made. A room that
//! withdraws on disconnect throws away a seat the lobby has already promised, and
//! the player silently lands as a spectator while the lobby reports them seated.
//! Neither half complains.
//!
//! So [`withdraw`](SeatReservations::withdraw) is the lobby's word, never the
//! transport's. This is [`ReconnectTracker`](plaza::common::reconnect)'s lesson
//! from the other side: plaza reports a dropped connection and deliberately does
//! not say what it means, because the transport does not know. Only the lobby
//! can tell "gone" from "the same player, one second later".
//!
//! Distinct from [`SeatTable`](https://docs.rs/plaza_server_utils) in
//! `plaza_server_utils`, which allocates seat *indices* to agents already
//! connected. This is about the window before that.

use std::collections::HashSet;

use plaza::agent::AgentId;

/// Seats promised but not yet taken.
#[derive(Debug, Clone, Default)]
pub struct SeatReservations<ID: AgentId> {
  held: HashSet<ID>,
}

impl<ID: AgentId> SeatReservations<ID> {
  pub fn new() -> Self {
    Self { held: HashSet::new() }
  }

  /// Promises a seat. Returns `false` if this player already held one, so a
  /// duplicate admission is visible rather than silent.
  pub fn reserve(&mut self, player: ID) -> bool {
    self.held.insert(player)
  }

  /// Takes the reservation if there is one. Call this on `AgentJoined`.
  ///
  /// Returns whether the arriving player was admitted. A reservation is spent
  /// once, so a second connection on the same id is not a second seat.
  pub fn consume(&mut self, player: &ID) -> bool {
    self.held.remove(player)
  }

  /// Cancels a reservation that will never be used.
  ///
  /// **Call this from the lobby, not from a disconnect.** The lobby knows when a
  /// player was placed elsewhere or left; a closed socket knows neither.
  pub fn withdraw(&mut self, player: &ID) -> bool {
    self.held.remove(player)
  }

  pub fn holds(&self, player: &ID) -> bool {
    self.held.contains(player)
  }

  /// Outstanding reservations. A count that only grows is the signal that
  /// admissions are being handed out and never dialled.
  pub fn count(&self) -> usize {
    self.held.len()
  }

  pub fn is_empty(&self) -> bool {
    self.held.is_empty()
  }

  pub fn iter(&self) -> impl Iterator<Item = &ID> {
    self.held.iter()
  }

  pub fn clear(&mut self) {
    self.held.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_reservation_is_spent_once() {
    let mut seats = SeatReservations::new();
    assert!(seats.reserve(1));
    assert!(seats.consume(&1));
    assert!(!seats.consume(&1), "a second connection is not a second seat");
  }

  #[test]
  fn an_unreserved_arrival_is_not_admitted() {
    let mut seats: SeatReservations<u32> = SeatReservations::new();
    assert!(!seats.consume(&1));
  }

  #[test]
  fn reserving_twice_reports_the_duplicate() {
    let mut seats = SeatReservations::new();
    assert!(seats.reserve(1));
    assert!(!seats.reserve(1));
    assert_eq!(seats.count(), 1);
  }

  #[test]
  fn the_lobby_can_withdraw() {
    let mut seats = SeatReservations::new();
    seats.reserve(1);
    assert!(seats.withdraw(&1));
    assert!(!seats.consume(&1));
  }

  /// The bug this type exists to prevent: a room hop closes the old socket
  /// after the new seat is reserved, so a disconnect that withdrew would demote
  /// a player the lobby had already told they were seated.
  #[test]
  fn a_reservation_survives_everything_except_a_withdrawal() {
    let mut seats = SeatReservations::new();
    seats.reserve(1);
    // However many disconnects the transport reports, nothing here reacts to
    // them: only `withdraw` and `consume` remove a seat.
    assert!(seats.holds(&1));
    assert!(seats.consume(&1));
  }

  #[test]
  fn outstanding_counts_what_was_never_dialled() {
    let mut seats = SeatReservations::new();
    seats.reserve(1);
    seats.reserve(2);
    seats.consume(&1);
    assert_eq!(seats.count(), 1);
    assert_eq!(seats.iter().collect::<Vec<_>>(), vec![&2]);
  }
}
