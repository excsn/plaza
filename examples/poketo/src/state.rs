//! What the server owns: one overworld, and a battle for every pair in one.
//!
//! A seat is in exactly one of the two at a time, and that is the whole of the
//! switch. It is not a flag on a player, it is which collection holds them: a
//! trainer in a battle is not walked, not sent the overworld, and not visible
//! to anyone still in it. Anything else leaves a body standing in the grass
//! while its owner is elsewhere.

use std::collections::HashMap;

use plaza_server_utils::Roster;

use crate::battle::Battle;
use crate::grid::{Facing, Tile};
use crate::protocol::PlayerId;
use crate::world::{World, MAX_TRAINERS, VIEW_TILES};

/// How often a step onto a wild tile starts something, as one in this many.
///
/// Rare enough that walking is walking, common enough that a test which walks
/// for a few seconds sees one.
pub const ENCOUNTER_ODDS: u64 = 12;

pub struct PoketoState {
  pub world: World,
  pub tick: u64,
  pub roster: Roster<PlayerId>,
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  /// What each seat is holding in the overworld.
  pub held: Vec<Option<Facing>>,
  /// A seat is in here **or** in the world, never both.
  pub battles: HashMap<u16, Battle>,
  /// How far a client is told about, in tiles.
  pub view: u32,
  /// What a dropped connection left behind, by the token it was given.
  ///
  /// Kept rather than destroyed, because a turn-based game is the one place
  /// where a disconnection costs nothing if you wait: nothing decays, so a
  /// battle is exactly as valid a minute later. The window is what stops it
  /// being a leak.
  pub parked: HashMap<u64, Parked>,
  /// Handed out on seating, so a client has something to come back with.
  next_token: u64,
  /// Which token each connected player holds, for parking on departure.
  pub tokens: HashMap<PlayerId, u64>,
  /// Scratch, so a tick that queries once per client allocates nothing.
  seen: Vec<u16>,
}

/// What a departed player can come back to.
#[derive(Clone, Debug)]
pub struct Parked {
  pub seat: u16,
  pub at: Tile,
  pub battle: Option<Battle>,
  /// The tick it was parked on, for aging it out.
  pub since: u64,
}

/// Ticks a parked seat is kept. A minute at 60Hz, which is a long time to be
/// reconnecting and no time at all to hold a seat in a town.
pub const PARK_TICKS: u64 = 3600;

impl std::fmt::Debug for PoketoState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PoketoState").field("tick", &self.tick).finish_non_exhaustive()
  }
}

impl Default for PoketoState {
  fn default() -> Self {
    Self::new()
  }
}

impl PoketoState {
  pub fn new() -> Self {
    Self {
      world: World::new(),
      tick: 0,
      roster: Roster::new(MAX_TRAINERS),
      agents: HashMap::new(),
      held: vec![None; MAX_TRAINERS],
      battles: HashMap::new(),
      view: VIEW_TILES,
      parked: HashMap::new(),
      next_token: 1,
      tokens: HashMap::new(),
      seen: Vec::new(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.roster.seat_of(&player)
  }

  /// A token for a newly seated player.
  pub fn issue_token(&mut self) -> u64 {
    let token = self.next_token;
    self.next_token += 1;
    token
  }

  /// Parks everything a departing seat was doing, against its token.
  pub fn park(&mut self, token: u64, seat: u16) {
    let at = self
      .world
      .walkers
      .get(seat as usize)
      .map(|w| w.trainer.at)
      .unwrap_or_default();
    self.parked.insert(
      token,
      Parked {
        seat,
        at,
        battle: self.battles.remove(&seat),
        since: self.tick,
      },
    );
  }

  /// Takes back what a token was holding, if it is still there.
  pub fn claim(&mut self, token: u64) -> Option<Parked> {
    let parked = self.parked.get(&token)?;
    if self.tick.saturating_sub(parked.since) > PARK_TICKS {
      // Aged out. Removed here rather than swept, so nothing has to run on a
      // tick to keep this honest.
      self.parked.remove(&token);
      return None;
    }
    self.parked.remove(&token)
  }

  /// Drops parked seats nobody came back for.
  pub fn expire_parked(&mut self) {
    let now = self.tick;
    self.parked.retain(|_, p| now.saturating_sub(p.since) <= PARK_TICKS);
  }

  /// Whether this seat is in a battle rather than the overworld.
  pub fn battling(&self, seat: u16) -> bool {
    self.battles.contains_key(&seat)
  }

  /// The trainers a seat can see, itself included, and nobody who is away in a
  /// battle.
  pub fn visible_to(&mut self, seat: usize) -> &[u16] {
    let view = self.view;
    self.world.visible_to(seat, view, &mut self.seen);
    // A trainer in a battle is not standing in the overworld, so it is not
    // drawn there either. The alternative is a body everyone can see and
    // nobody can interact with.
    self.seen.retain(|s| !self.battles.contains_key(s));
    &self.seen
  }

  /// Whether a step onto this tile begins something.
  ///
  /// Hashed from the tile and the tick rather than rolled, so a replay of the
  /// same walk produces the same encounters and a test is a measurement.
  pub fn encounter_at(&self, at: Tile, seat: usize) -> bool {
    let mut seed = (at.x as u64) << 32 | at.y as u64;
    seed ^= (self.tick / 7).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    seed ^= seat as u64;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    seed.is_multiple_of(ENCOUNTER_ODDS)
  }

  /// Moves a seat out of the overworld and into a battle.
  ///
  /// The wild side takes a seat number no player can hold, so a battle is
  /// always between two sides and the rules never need a case for "against
  /// nobody".
  pub fn begin_battle(&mut self, seat: u16, kind: u8) {
    let wild = WILD_SEAT;
    self.battles.insert(seat, Battle::between(seat, wild, (kind, kind.wrapping_add(1))));
    self.held[seat as usize] = None;
  }

  /// Puts a seat back in the overworld where it was standing.
  pub fn end_battle(&mut self, seat: u16) {
    self.battles.remove(&seat);
  }
}

/// The seat a wild creature sits in: past every real one, so it can never
/// collide with a player and never needs to be excluded from a roster.
pub const WILD_SEAT: u16 = u16::MAX;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_seat_in_a_battle_is_not_in_the_overworld() {
    // The switch is which collection holds them, not a flag: a trainer in a
    // battle must not be walked, sent, or visible, or there is a body standing
    // in the grass while its owner is elsewhere.
    let mut state = PoketoState::new();
    state.world.seat(0, Tile::new(10, 10));
    state.world.seat(1, Tile::new(11, 10));

    let seen = state.visible_to(0).to_vec();
    assert!(seen.contains(&1), "neighbours see each other while walking");

    state.begin_battle(1, 0);
    let seen = state.visible_to(0).to_vec();
    assert!(!seen.contains(&1), "and not while one of them is away");
    assert!(seen.contains(&0), "which does not hide the watcher from itself");
  }

  #[test]
  fn a_battle_clears_whatever_direction_was_held() {
    // Or a trainer walks out of the grass the moment its battle ends, having
    // been holding a direction the whole time it was away.
    let mut state = PoketoState::new();
    state.world.seat(0, Tile::new(10, 10));
    state.held[0] = Some(Facing::East);
    state.begin_battle(0, 1);
    assert_eq!(state.held[0], None);
  }

  #[test]
  fn an_encounter_is_a_function_of_where_and_when_rather_than_a_roll() {
    // So a replay of the same walk produces the same encounters, which is what
    // makes a test of this a measurement rather than an anecdote.
    let state = PoketoState::new();
    let at = Tile::new(40, 40);
    assert_eq!(state.encounter_at(at, 0), state.encounter_at(at, 0));

    // And it does happen, at roughly the odds it claims.
    let hits = (0..600)
      .filter(|i| state.encounter_at(Tile::new(*i % 64, *i / 64), 0))
      .count();
    assert!(hits > 10, "walking should start something eventually: {hits} in 600 tiles");
    assert!(hits < 200, "and not constantly: {hits}");
  }

  #[test]
  fn the_wild_side_cannot_be_a_player() {
    assert!(WILD_SEAT as usize > MAX_TRAINERS, "or a battle could be against a real seat");
  }
}
