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
//! # A promise with a duration is still the lobby's word
//!
//! [`with_expiry`](SeatReservations::with_expiry) does not contradict the rule
//! above. The lobby sets the window when it reserves, so a lapse is the lobby
//! having said "for this long" rather than the transport having said anything.
//! Drive it from your `TimeStep` arm with [`tick`](SeatReservations::tick),
//! which hands back whoever lapsed so the lobby's own records can follow.
//!
//! **The window has to be longer than the placement ticket's.** Redemption is
//! two steps in two places: the route spends the ticket, the session comes up,
//! and only then does the room's logic consume the reservation. Equal windows
//! strand a client that dialled at the edge, holding a spent ticket and seated
//! as a spectator.
//!
//! Distinct from [`SeatTable`](https://docs.rs/plaza_server_utils) in
//! `plaza_server_utils`, which allocates seat *indices* to agents already
//! connected. This is about the window before that.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

use plaza::agent::AgentId;

/// Seats promised but not yet taken.
#[derive(Debug, Clone, Default)]
pub struct SeatReservations<ID: AgentId> {
  held: HashMap<ID, Duration>,
  elapsed: Duration,
  window: Option<Duration>,
}

impl<ID: AgentId> SeatReservations<ID> {
  /// Reservations that are held until consumed or withdrawn, however long that
  /// takes.
  pub fn new() -> Self {
    Self {
      held: HashMap::new(),
      elapsed: Duration::ZERO,
      window: None,
    }
  }

  /// Reservations that lapse `window` after they were made, once [`tick`] has
  /// been told that much time passed.
  ///
  /// [`tick`]: SeatReservations::tick
  pub fn with_expiry(window: Duration) -> Self {
    Self {
      window: Some(window),
      ..Self::new()
    }
  }

  /// Advances this type's own clock and drops whatever lapsed, returning those
  /// players so the lobby can clear its records too.
  ///
  /// Call it from `LogicInput::TimeStep` with the same `delta_time`. Nothing
  /// here reads a clock, exactly as nothing here reacts to a disconnect.
  pub fn tick(&mut self, delta: Duration) -> Vec<ID> {
    self.elapsed = self.elapsed.saturating_add(delta);
    let Some(window) = self.window else {
      return Vec::new();
    };
    // Ages, not a cutoff: `elapsed - window` saturates at zero, which would make
    // every reservation made before the first window elapsed look overdue.
    let lapsed: Vec<ID> = self
      .held
      .iter()
      .filter(|(_, made)| self.elapsed.saturating_sub(**made) >= window)
      .map(|(player, _)| player.clone())
      .collect();
    for player in &lapsed {
      self.held.remove(player);
    }
    lapsed
  }

  /// The window a reservation stays live for, or `None` if it never lapses.
  pub fn expiry(&self) -> Option<Duration> {
    self.window
  }

  /// Promises a seat. Returns `false` if this player already held one, so a
  /// duplicate admission is visible rather than silent.
  ///
  /// Re-reserving does not restart the window: the first promise is the one
  /// being kept, and a second `reserve` reporting `false` is the caller's signal
  /// that it already had one.
  pub fn reserve(&mut self, player: ID) -> bool {
    match self.held.entry(player) {
      Entry::Occupied(_) => false,
      Entry::Vacant(slot) => {
        slot.insert(self.elapsed);
        true
      }
    }
  }

  /// Takes the reservation if there is one. Call this on `AgentJoined`.
  ///
  /// Returns whether the arriving player was admitted. A reservation is spent
  /// once, so a second connection on the same id is not a second seat.
  pub fn consume(&mut self, player: &ID) -> bool {
    self.held.remove(player).is_some()
  }

  /// Cancels a reservation that will never be used.
  ///
  /// **Call this from the lobby, not from a disconnect.** The lobby knows when a
  /// player was placed elsewhere or left; a closed socket knows neither.
  pub fn withdraw(&mut self, player: &ID) -> bool {
    self.held.remove(player).is_some()
  }

  pub fn holds(&self, player: &ID) -> bool {
    self.held.contains_key(player)
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
    self.held.keys()
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
  fn without_a_window_nothing_ever_lapses() {
    let mut seats = SeatReservations::new();
    seats.reserve(1);
    assert!(seats.tick(Duration::from_secs(86_400)).is_empty());
    assert!(seats.holds(&1), "today's behaviour");
  }

  #[test]
  fn a_reservation_lapses_once_its_window_has_passed() {
    let mut seats = SeatReservations::with_expiry(Duration::from_secs(45));
    seats.reserve(1);

    assert!(seats.tick(Duration::from_secs(44)).is_empty(), "not yet");
    assert!(seats.holds(&1));

    assert_eq!(seats.tick(Duration::from_secs(1)), vec![1], "the lapsed are handed back");
    assert!(!seats.holds(&1));
    assert!(!seats.consume(&1), "and the seat is gone, not merely unreported");
  }

  #[test]
  fn the_window_runs_from_each_reservation_rather_than_from_the_clock() {
    let mut seats = SeatReservations::with_expiry(Duration::from_secs(45));
    seats.reserve(1);
    seats.tick(Duration::from_secs(30));
    seats.reserve(2);

    assert_eq!(seats.tick(Duration::from_secs(15)), vec![1], "only the older one is due");
    assert!(seats.holds(&2));
  }

  #[test]
  fn re_reserving_does_not_buy_a_fresh_window() {
    // Otherwise a lobby that re-issues on every quick-match press keeps a seat
    // alive indefinitely without the player ever dialling.
    let mut seats = SeatReservations::with_expiry(Duration::from_secs(45));
    seats.reserve(1);
    seats.tick(Duration::from_secs(40));
    assert!(!seats.reserve(1), "already held");

    assert_eq!(seats.tick(Duration::from_secs(5)), vec![1]);
  }

  #[test]
  fn a_player_who_arrived_never_lapses() {
    // The property that keeps expiry from racing an arrival: consuming removes
    // the reservation, so a seated player is already out of reach of the sweep.
    let mut seats = SeatReservations::with_expiry(Duration::from_secs(45));
    seats.reserve(1);
    assert!(seats.consume(&1));
    assert!(seats.tick(Duration::from_secs(90)).is_empty());
  }

  #[test]
  fn a_lapse_reports_every_player_it_dropped() {
    let mut seats = SeatReservations::with_expiry(Duration::from_secs(45));
    seats.reserve(1);
    seats.reserve(2);
    seats.reserve(3);
    seats.consume(&2);

    let mut lapsed = seats.tick(Duration::from_secs(46));
    lapsed.sort();
    assert_eq!(lapsed, vec![1, 3]);
    assert!(seats.is_empty());
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
