//! The authoritative state. Only [`crate::logic::YardLogic`] mutates it.

use std::collections::HashMap;

use plaza::agent::Agent;
use plaza_server_utils::{Roster, SeatState};

use crate::protocol::{Drive, Encoding, PlayerId};
use crate::sim::{Yard, MAX_PLAYERS};

pub struct YardState {
  pub yard: Yard,
  pub tick: u64,
  pub encoding: Encoding,
  /// Whether the server snaps its own state onto the wire's grid each tick.
  pub snap: bool,
  pub roster: Roster<PlayerId>,
  /// The level each seat currently holds; a tick with nothing new repeats it.
  pub driving: [Drive; MAX_PLAYERS],
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
}

impl std::fmt::Debug for YardState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("YardState").field("tick", &self.tick).finish_non_exhaustive()
  }
}

impl Default for YardState {
  fn default() -> Self {
    Self::new()
  }
}

impl YardState {
  pub fn new() -> Self {
    Self::with(Encoding::default(), false)
  }

  pub fn with(encoding: Encoding, snap: bool) -> Self {
    Self {
      yard: Yard::new(),
      tick: 0,
      encoding,
      snap,
      roster: Roster::new(MAX_PLAYERS),
      driving: [Drive::default(); MAX_PLAYERS],
      agents: HashMap::new(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.roster.seat_of(&player)
  }

  pub fn is_human(&self, seat: usize) -> bool {
    matches!(self.roster.seat_state(seat), SeatState::Human(_))
  }
}
