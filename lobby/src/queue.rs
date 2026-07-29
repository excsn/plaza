//! Waiting for opponents, and giving up on them.
//!
//! [`InMemoryLobbyManager`](crate::InMemoryLobbyManager) answers "which room",
//! which assumes the player is choosing. A queue answers "who with", for the
//! games where they are not: press play, wait, get dropped into a match.
//!
//! Holds no timers and spawns nothing, like
//! [`ReconnectTracker`](plaza::common::reconnect). Keep one and drive it:
//!
//! ```ignore
//! LogicInput::AgentOps { .. } => { queue.enqueue(player, state.now); }
//! LogicInput::TimeStep { .. } => {
//!   for formed in queue.drain_ready(state.now) {
//!     let room = spawn_room().await;
//!     for player in &formed.players { admit(room, player) }
//!     for seat in 0..formed.bots { seat_bot(room, seat) }   // your rule
//!   }
//! }
//! ```
//!
//! # Why patience produces bots rather than a refusal
//!
//! A queue that only ever pairs humans stops working at exactly the moment a
//! game needs it most: launch, off-peak, and small regions. The interesting
//! decision is not "how long do we wait" but "what do we do when the wait is
//! over", and the answer that keeps a game playable is to start anyway with the
//! seats filled. So [`Formed::bots`] is a *count of seats to fill*, and what
//! fills them is yours: a bot, a lower player count, a merged lobby.
//!
//! Zero patience forms a match on the next [`drain_ready`](MatchQueue::drain_ready)
//! with whoever is present, which is a reasonable way to say "never wait".

use std::collections::VecDeque;

use plaza::agent::AgentId;
use plaza::common::scheduler::SchedulerInstant;

/// A match the queue has decided to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formed<ID: AgentId> {
  /// The humans, in the order they queued.
  pub players: Vec<ID>,
  /// Seats nobody is coming to fill. Zero for a full human match.
  pub bots: usize,
  /// Whether patience ran out rather than the match filling.
  ///
  /// The two cases are worth telling apart in a readout: a queue that only ever
  /// forms this way is not matchmaking, it is a single-player game with a delay.
  pub timed_out: bool,
}

impl<ID: AgentId> Formed<ID> {
  pub fn size(&self) -> usize {
    self.players.len() + self.bots
  }
}

#[derive(Debug, Clone)]
struct Waiting<ID: AgentId, T: SchedulerInstant> {
  player: ID,
  since: T,
  deadline: T,
}

/// Players waiting to be matched.
#[derive(Debug, Clone)]
pub struct MatchQueue<ID: AgentId, T: SchedulerInstant> {
  waiting: VecDeque<Waiting<ID, T>>,
  size: usize,
  patience: T,
}

impl<ID: AgentId, T: SchedulerInstant> MatchQueue<ID, T> {
  /// A queue forming matches of `size`, waiting `patience` before filling the
  /// remaining seats.
  ///
  /// # Panics
  ///
  /// If `size` is zero, which would form empty matches for ever.
  pub fn new(size: usize, patience: T) -> Self {
    assert!(size > 0, "MatchQueue size must be greater than 0");
    Self {
      waiting: VecDeque::new(),
      size,
      patience,
    }
  }

  /// Adds a player. Returns `false` if they were already queued, so a
  /// double-press does not take two places.
  pub fn enqueue(&mut self, player: ID, now: T) -> bool {
    if self.waiting.iter().any(|w| w.player == player) {
      return false;
    }
    self.waiting.push_back(Waiting {
      player: player.clone(),
      since: now,
      deadline: now.add_interval(self.patience),
    });
    true
  }

  /// Removes a player who cancelled or disconnected.
  pub fn remove(&mut self, player: &ID) -> bool {
    let before = self.waiting.len();
    self.waiting.retain(|w| &w.player != player);
    self.waiting.len() != before
  }

  /// Every match that can start now, in the order they formed.
  ///
  /// Drive from `TimeStep`. Full matches come out first and for as long as there
  /// are enough people; then, if whoever is left has waited past their patience,
  /// one more comes out with the empty seats counted in [`Formed::bots`].
  pub fn drain_ready(&mut self, now: T) -> Vec<Formed<ID>> {
    let mut formed = Vec::new();

    while self.waiting.len() >= self.size {
      let players: Vec<ID> = self.waiting.drain(..self.size).map(|w| w.player).collect();
      formed.push(Formed {
        players,
        bots: 0,
        timed_out: false,
      });
    }

    // The head is the longest-waiting, so if anybody is out of patience it is
    // them, and everyone still queued goes into the match with them rather than
    // being left behind for a round they would only wait through again.
    if self.waiting.front().is_some_and(|w| w.deadline <= now) {
      let players: Vec<ID> = self.waiting.drain(..).map(|w| w.player).collect();
      let bots = self.size.saturating_sub(players.len());
      formed.push(Formed {
        players,
        bots,
        timed_out: true,
      });
    }

    formed
  }

  /// This player's place in line, zero-based.
  pub fn position(&self, player: &ID) -> Option<usize> {
    self.waiting.iter().position(|w| &w.player == player)
  }

  /// When this player joined the queue, for a "waiting 12s" readout.
  pub fn waiting_since(&self, player: &ID) -> Option<T> {
    self.waiting.iter().find(|w| &w.player == player).map(|w| w.since)
  }

  pub fn contains(&self, player: &ID) -> bool {
    self.waiting.iter().any(|w| &w.player == player)
  }

  /// How many are needed to form a match without waiting.
  pub fn match_size(&self) -> usize {
    self.size
  }

  pub fn len(&self) -> usize {
    self.waiting.len()
  }

  pub fn is_empty(&self) -> bool {
    self.waiting.is_empty()
  }

  pub fn clear(&mut self) {
    self.waiting.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn queue(size: usize, patience: u64) -> MatchQueue<u32, u64> {
    MatchQueue::new(size, patience)
  }

  #[test]
  fn a_full_house_forms_at_once() {
    let mut q = queue(2, 100);
    q.enqueue(1, 0);
    assert!(q.drain_ready(0).is_empty(), "one player is not a match");
    q.enqueue(2, 0);
    let formed = q.drain_ready(0);
    assert_eq!(formed.len(), 1);
    assert_eq!(formed[0].players, vec![1, 2]);
    assert_eq!(formed[0].bots, 0);
    assert!(!formed[0].timed_out);
  }

  #[test]
  fn players_are_matched_in_the_order_they_queued() {
    let mut q = queue(2, 100);
    for id in 1..=4 {
      q.enqueue(id, 0);
    }
    let formed = q.drain_ready(0);
    assert_eq!(formed.len(), 2);
    assert_eq!(formed[0].players, vec![1, 2]);
    assert_eq!(formed[1].players, vec![3, 4]);
    assert!(q.is_empty());
  }

  #[test]
  fn patience_fills_the_empty_seats() {
    let mut q = queue(4, 100);
    q.enqueue(1, 0);
    assert!(q.drain_ready(99).is_empty(), "still waiting");

    let formed = q.drain_ready(100);
    assert_eq!(formed.len(), 1);
    assert_eq!(formed[0].players, vec![1]);
    assert_eq!(formed[0].bots, 3);
    assert!(formed[0].timed_out);
    assert_eq!(formed[0].size(), 4);
  }

  /// Everyone still queued joins the timed-out match, rather than being left to
  /// wait out a second patience they have already partly served.
  #[test]
  fn a_timeout_takes_everyone_still_waiting() {
    let mut q = queue(4, 100);
    q.enqueue(1, 0);
    q.enqueue(2, 50);
    let formed = q.drain_ready(100);
    assert_eq!(formed[0].players, vec![1, 2]);
    assert_eq!(formed[0].bots, 2);
    assert!(q.is_empty());
  }

  /// Each player's deadline runs from their own arrival, so the next person to
  /// queue does not inherit the wait of the one who just timed out.
  #[test]
  fn patience_is_measured_from_each_players_own_arrival() {
    let mut q = queue(2, 100);
    q.enqueue(1, 0);
    let formed = q.drain_ready(100);
    assert!(formed[0].timed_out);
    assert_eq!(formed[0].bots, 1);

    q.enqueue(2, 200);
    assert!(q.drain_ready(250).is_empty(), "their own clock started at 200");
    assert!(!q.drain_ready(300).is_empty());
  }

  /// A full house forms as a pairing even when someone in it is out of
  /// patience: there is nothing left to wait for.
  #[test]
  fn filling_up_beats_the_clock() {
    let mut q = queue(2, 100);
    q.enqueue(1, 0);
    q.enqueue(2, 500);
    let formed = q.drain_ready(500);
    assert_eq!(formed.len(), 1);
    assert_eq!(formed[0].players, vec![1, 2]);
    assert_eq!(formed[0].bots, 0);
    assert!(!formed[0].timed_out, "nobody was given a bot, so nobody timed out");
  }

  #[test]
  fn zero_patience_never_waits() {
    let mut q = queue(4, 0);
    q.enqueue(1, 0);
    let formed = q.drain_ready(0);
    assert_eq!(formed[0].bots, 3);
  }

  #[test]
  fn a_double_press_does_not_take_two_places() {
    let mut q = queue(2, 100);
    assert!(q.enqueue(1, 0));
    assert!(!q.enqueue(1, 0));
    assert_eq!(q.len(), 1);
  }

  #[test]
  fn cancelling_leaves_the_queue() {
    let mut q = queue(2, 100);
    q.enqueue(1, 0);
    q.enqueue(2, 0);
    assert!(q.remove(&1));
    assert!(!q.remove(&1));
    assert!(q.drain_ready(0).is_empty(), "one left is not a match");
    assert_eq!(q.position(&2), Some(0));
  }

  #[test]
  fn position_and_since_report_the_wait() {
    let mut q = queue(3, 100);
    q.enqueue(1, 10);
    q.enqueue(2, 20);
    assert_eq!(q.position(&1), Some(0));
    assert_eq!(q.position(&2), Some(1));
    assert_eq!(q.waiting_since(&2), Some(20));
    assert_eq!(q.position(&9), None);
  }

  #[test]
  fn draining_an_empty_queue_forms_nothing() {
    let mut q = queue(2, 0);
    assert!(q.drain_ready(1000).is_empty());
  }

  #[test]
  #[should_panic(expected = "MatchQueue size must be greater than 0")]
  fn a_zero_size_queue_is_refused() {
    let _: MatchQueue<u32, u64> = MatchQueue::new(0, 10);
  }

  #[test]
  fn solo_matches_form_immediately() {
    let mut q = queue(1, 1000);
    q.enqueue(1, 0);
    let formed = q.drain_ready(0);
    assert_eq!(formed[0].players, vec![1]);
    assert_eq!(formed[0].bots, 0);
  }
}
