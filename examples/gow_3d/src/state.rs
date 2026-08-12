//! What the server owns: one zone, and who is sitting in it.
//!
//! Thinner than the other examples' state, and that is the finding rather than
//! an omission. There is no predicted state to reconcile because the client's
//! position is the truth, and no simulation to step because the only thing with
//! a clock is a cast bar. What is left is a roster, a set of subscriptions, and
//! a timer per character.

use std::collections::HashMap;

use plaza_server_utils::Roster;

use crate::protocol::PlayerId;
use crate::relevance::Seat;
use crate::zone::Zone;

/// The most characters in one zone.
pub const MAX_CHARACTERS: usize = 64;

/// Where a character starts.
///
/// A spiral rather than a ring, because a ring of a fixed angular step wraps:
/// the first version stepped 0.9 radians and put seat 7 on top of seat 0. The
/// golden angle is the one step that never repeats.
pub fn spawn_at(seat: Seat) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = 8.0 + (seat as f32).sqrt() * 4.0;
  (angle.cos() * radius, 0.0, angle.sin() * radius)
}

pub struct GowState {
  pub zone: Zone,
  pub tick: u64,
  pub roster: Roster<PlayerId>,
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  /// Casts that landed on this tick, cleared when they have been sent.
  ///
  /// Held for exactly one tick because it is an event: keeping it longer would
  /// send it twice and clearing it earlier would lose it.
  pub landed: Vec<Seat>,
  /// Scratch, so a tick that queries once per client allocates nothing.
  scratch: Vec<Seat>,
}

impl std::fmt::Debug for GowState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GowState")
      .field("tick", &self.tick)
      .field("characters", &self.zone.characters.len())
      .finish_non_exhaustive()
  }
}

impl Default for GowState {
  fn default() -> Self {
    Self::new()
  }
}

impl GowState {
  pub fn new() -> Self {
    Self {
      zone: Zone::new(),
      tick: 0,
      roster: Roster::new(MAX_CHARACTERS),
      agents: HashMap::new(),
      landed: Vec::new(),
      scratch: Vec::new(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<Seat> {
    self.roster.seat_of(&player).map(|s| s as Seat)
  }

  /// Borrows the scratch buffer for one audience query.
  pub fn with_scratch<T>(&mut self, f: impl FnOnce(&mut Zone, &mut Vec<Seat>) -> T) -> T {
    let mut scratch = std::mem::take(&mut self.scratch);
    let out = f(&mut self.zone, &mut scratch);
    self.scratch = scratch;
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn spawns_are_spread_rather_than_stacked() {
    // A zone that starts as a pile is one where the first thing every client
    // does is a spatial query returning everybody, which is the case this
    // example is meant to be measuring away from.
    let mut closest = f32::MAX;
    for a in 0..MAX_CHARACTERS as Seat {
      for b in (a + 1)..MAX_CHARACTERS as Seat {
        closest = closest.min(crate::movement::distance(spawn_at(a), spawn_at(b)));
      }
    }
    // Every seat, not the first handful: the ring this replaced looked fine
    // for eight and put seat 7 on top of seat 0.
    assert!(closest > 2.0, "closest pair is {closest}");
  }
}
