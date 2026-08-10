//! The authoritative state. Only [`crate::logic::YardLogic`] mutates it.

use std::collections::HashMap;

use plaza::agent::Agent;
use plaza_server_utils::{Roster, SeatState};

use crate::budget::Stream;
use crate::protocol::{Drive, Encoding, PlayerId};
use crate::sim::{Yard, MAX_PLAYERS};

pub struct YardState {
  pub yard: Yard,
  pub tick: u64,
  pub encoding: Encoding,
  /// Whether the server snaps its own state onto the wire's grid each tick.
  pub snap: bool,
  /// Frames per second on the wire. Below the tick rate, a client has gaps to
  /// fill and interpolation stops being a formality.
  pub send_hz: u64,
  pub roster: Roster<PlayerId>,
  /// The level each seat currently holds; a tick with nothing new repeats it.
  pub driving: [Drive; MAX_PLAYERS],
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  /// One share of the wire per client: a budget is per link, so the choosing
  /// is too, and two clients standing in different places get different cubes.
  pub streams: HashMap<PlayerId, Stream>,
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
    Self::at_rate(encoding, snap, crate::protocol::TICK_HZ)
  }

  pub fn at_rate(encoding: Encoding, snap: bool, send_hz: u64) -> Self {
    Self {
      yard: Yard::new(),
      tick: 0,
      encoding,
      snap,
      send_hz: send_hz.clamp(1, crate::protocol::TICK_HZ),
      roster: Roster::new(MAX_PLAYERS),
      driving: [Drive::default(); MAX_PLAYERS],
      agents: HashMap::new(),
      streams: HashMap::new(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.roster.seat_of(&player)
  }

  pub fn is_human(&self, seat: usize) -> bool {
    matches!(self.roster.seat_state(seat), SeatState::Human(_))
  }
}
