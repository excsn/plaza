//! What the server owns: one overworld, and a battle for every pair in one.
//!
//! A seat is in exactly one of the two at a time, and that is the whole of the
//! switch. It is not a flag on a player, it is which collection holds them: a
//! trainer in a battle is not walked, not sent the overworld, and not visible
//! to anyone still in it. Anything else leaves a body standing in the grass
//! while its owner is elsewhere.

use std::collections::HashMap;

use plaza_server_utils::Roster;

use crate::battle::{Battle, Creature};
use crate::grid::{Facing, Tile};
use crate::protocol::PlayerId;
use crate::terrain;
use crate::world::{World, MAX_TRAINERS, PLAYER_SEATS};

/// How often a step into tall grass starts something, as one in this many.
///
/// The rate is two rules rather than one: this, and the fact that only tall
/// grass counts at all. Walking anywhere at one in twelve made an encounter a
/// tax on moving; walking into a patch you can see, at one in thirty, makes it
/// something you decided to do. A slider moves it from here at runtime.
pub const ENCOUNTER_ODDS: u64 = crate::protocol::DEFAULT_ENCOUNTER_ODDS as u64;

pub struct PoketoState {
  pub world: World,
  pub tick: u64,
  pub roster: Roster<PlayerId>,
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  /// What each seat is holding in the overworld.
  pub held: Vec<Option<Facing>>,
  /// The creature each seat owns, which is the only thing here that
  /// accumulates. Indexed by seat, like `held`.
  pub party: Vec<Creature>,
  /// Scratch for the tick, so the walk does not rebuild a direction list.
  pub held_now: Vec<Option<Facing>>,
  /// A seat is in here **or** in the world, never both.
  pub battles: HashMap<u16, Battle>,
  /// The knobs the town runs on, which a client may ask to move.
  pub tuning: crate::protocol::Tuning,
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
  /// Kept for the same reason the battle is: experience does not decay either,
  /// and coming back to a level-one creature is losing the only thing here
  /// worth having.
  pub party: Creature,
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
    let mut world = World::new();
    world.populate(crate::world::TOWN_CENTRE, 30);
    Self {
      world,
      tick: 0,
      roster: Roster::new(PLAYER_SEATS),
      agents: HashMap::new(),
      held: vec![None; MAX_TRAINERS],
      party: vec![Creature::of_kind(0); MAX_TRAINERS],
      held_now: Vec::new(),
      battles: HashMap::new(),
      tuning: crate::protocol::Tuning::new(),
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
        party: self.party[seat as usize],
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
    let view = self.tuning.view_tiles;
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
  /// Gated on the ground first: nothing starts outside the tall grass, which
  /// is what makes an encounter somewhere you chose to walk.
  pub fn encounter_at(&self, at: Tile, seat: usize) -> bool {
    if !terrain::wild(at) {
      return false;
    }
    let mut seed = (at.x as u64) << 32 | at.y as u64;
    seed ^= (self.tick / 7).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    seed ^= seat as u64;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    seed.is_multiple_of(self.tuning.encounter_odds.max(1) as u64)
  }

  /// What lives in the grass here, and how grown it is.
  ///
  /// Level rises with the zone, so walking further is walking into harder
  /// fights and no difficulty setting has to exist.
  pub fn wild_at(&self, at: Tile, zone: u8) -> Creature {
    let kind = (at.x ^ at.y) as u8;
    let spread = ((at.x.wrapping_mul(31) ^ at.y.wrapping_mul(17)) % 3) as u8;
    Creature::of_kind_at(kind, 1 + zone * 4 + spread)
  }

  /// Moves a seat out of the overworld and into a battle against `wild`.
  ///
  /// The wild side takes a seat number no player can hold, so a battle is
  /// always between two sides and the rules never need a case for "against
  /// nobody".
  pub fn begin_battle(&mut self, seat: u16, wild: Creature) {
    let seed = (self.tick as u32).wrapping_mul(0x9E37_79B9).wrapping_add(seat as u32);
    let mine = self.party[seat as usize];
    self
      .battles
      .insert(seat, Battle::between_at(mine, wild, (seat, WILD_SEAT), seed));
    self.held[seat as usize] = None;
  }

  /// Puts a seat back in the overworld, keeping whatever its creature did.
  ///
  /// **Losing sends you back to the start, whole.** A creature walked out on
  /// the single point it had left could only lose again, and the nearest spring
  /// is a region's walk away through the grass that just beat it, so the one
  /// thing a player could do is the one thing that cannot work. Winning leaves
  /// the damage on, because that is what a spring is for.
  pub fn end_battle(&mut self, seat: u16) {
    let Some(battle) = self.battles.remove(&seat) else {
      return;
    };
    let Some(side) = battle.sides.iter().find(|s| s.seat == seat) else {
      return;
    };

    let mut mine = side.creature;
    let won = battle.winner == Some(seat);
    if won {
      if let Some(beaten) = battle.sides.iter().find(|s| s.seat != seat).map(|s| s.creature) {
        mine.absorb(Creature::xp_for_win(&beaten));
      }
      mine.health = mine.health.max(1);
    } else {
      mine.health = mine.full_health();
      // Sent back rather than left where it fell. The trainer's tile simply
      // changes by more than a step, which is all a client needs to know that
      // what happened was not a walk.
      let spot = crate::world::spawn_spot();
      if let Some(walker) = self.world.walkers.get_mut(seat as usize) {
        walker.trainer.at = spot;
        walker.trainer.phase = 0;
        walker.stepping = 0;
        walker.arrived = false;
      }
    }
    self.party[seat as usize] = mine;
  }

  /// Mends a seat's creature if it is standing somewhere that does that.
  ///
  /// Returns whether anything changed, so a tick that heals nobody sends
  /// nothing: this is a change, and a change is what this example sends.
  pub fn mend(&mut self, seat: usize) -> bool {
    let Some(walker) = self.world.walkers.get(seat) else {
      return false;
    };
    if !terrain::mends(walker.trainer.at) {
      return false;
    }
    let creature = &mut self.party[seat];
    let full = creature.full_health();
    if creature.health >= full {
      return false;
    }
    creature.health = full;
    true
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

    state.begin_battle(1, Creature::of_kind(0));
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
    state.begin_battle(0, Creature::of_kind(1));
    assert_eq!(state.held[0], None);
  }

  #[test]
  fn an_encounter_is_a_function_of_where_and_when_rather_than_a_roll() {
    // So a replay of the same walk produces the same encounters, which is what
    // makes a test of this a measurement rather than an anecdote.
    let state = PoketoState::new();
    let at = Tile::new(40, 40);
    assert_eq!(state.encounter_at(at, 0), state.encounter_at(at, 0));

    // And it does happen, at roughly the odds it claims, on the ground that
    // has them.
    let grass: Vec<Tile> = (0..40_000u32)
      .map(|i| Tile::new(400 + i % 200, 400 + i / 200))
      .filter(|t| terrain::wild(*t))
      .collect();
    assert!(grass.len() > 1000, "a 200 by 200 patch of map should hold grass: {}", grass.len());

    let hits = grass.iter().filter(|t| state.encounter_at(**t, 0)).count();
    let rate = hits as f32 / grass.len() as f32;
    let claimed = 1.0 / ENCOUNTER_ODDS as f32;
    assert!(
      (claimed * 0.4..claimed * 2.5).contains(&rate),
      "one in {ENCOUNTER_ODDS} of grass tiles, roughly: {rate}"
    );
  }

  #[test]
  fn nothing_starts_outside_the_tall_grass() {
    // The half of the encounter rate that is not a number: walking a path is
    // walking, and an encounter is somewhere you chose to step.
    let state = PoketoState::new();
    for i in 0..40_000u32 {
      let at = Tile::new(400 + i % 200, 400 + i / 200);
      if !terrain::wild(at) {
        assert!(!state.encounter_at(at, 0), "something started on open ground at {at:?}");
      }
    }
  }

  #[test]
  fn the_wild_side_cannot_be_a_player() {
    assert!(WILD_SEAT as usize > MAX_TRAINERS, "or a battle could be against a real seat");
  }
}
