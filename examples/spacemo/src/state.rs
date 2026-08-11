//! What the server owns.
//!
//! One field per strategy is deliberately not what happens here: the field is
//! rebuilt each tick under whichever strategy the host has dialled, so the
//! panel can move between them and watch the bandwidth follow. That is the
//! same shape cube_yard's encoding dial arrived at, for the same reason: a
//! ratio you can only see by running twice is a ratio nobody sees.

use std::collections::HashMap;

use plaza_client_utils::math::Vec3;
use plaza_server_utils::Roster;

use crate::protocol::{Fly, PlayerId};
use crate::relevance::{Field, Strategy};
use crate::sim::{Space, MAX_PLAYERS};

/// How far a ship can see, at the widest the dial goes. Cells are sized against
/// this so a query never has to touch an absurd number of them.
pub const MAX_VIEW: f32 = crate::max_view();
/// Grid cell width, sized against the *current* radius rather than the widest,
/// or a small view would sweep cells that are mostly empty.
pub fn cell_for(view: f32) -> f32 {
  (view / 3.0).max(20.0)
}

pub struct SpaceState {
  pub space: Space,
  pub tick: u64,
  pub roster: Roster<PlayerId>,
  pub flying: [Fly; MAX_PLAYERS],
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  /// Rebuilt every tick. A ship moves every tick, so nothing is saved by
  /// keeping it, and an index that lags the world is worse than none.
  pub field: Field,
  pub strategy: Strategy,
  /// Scratch, so a tick that queries once per client allocates nothing.
  points: Vec<Vec3>,
  visible: Vec<u32>,
  /// A second index, over bolts. Separate from the ship index because the two
  /// churn at completely different rates: rebuilding one costs a handful of
  /// inserts and the other costs however many are in flight.
  bolt_field: Field,
  bolt_points: Vec<Vec3>,
  bolt_visible: Vec<u32>,
  /// What the last tick sent, per seat, for the panel.
  pub last_seen: [usize; MAX_PLAYERS],
  pub last_bytes: [usize; MAX_PLAYERS],
  pub last_bolt_bytes: [usize; MAX_PLAYERS],
  /// Whether frames go out bit-packed or at full serde width.
  pub packed: bool,
  pub relative: bool,
  pub bots: usize,
  /// How far a ship can see. The single number the whole example turns on.
  pub view: f32,
}

impl std::fmt::Debug for SpaceState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SpaceState").field("tick", &self.tick).finish_non_exhaustive()
  }
}

impl Default for SpaceState {
  fn default() -> Self {
    Self::new()
  }
}

impl SpaceState {
  pub fn new() -> Self {
    Self::with(Strategy::FlatBand)
  }

  pub fn with(strategy: Strategy) -> Self {
    Self {
      space: Space::new(),
      tick: 0,
      roster: Roster::new(MAX_PLAYERS),
      flying: [Fly::default(); MAX_PLAYERS],
      agents: HashMap::new(),
      field: Field::new(cell_for(crate::default_view()), strategy),
      strategy,
      points: Vec::new(),
      visible: Vec::new(),
      bolt_field: Field::new(cell_for(crate::default_view()), strategy),
      bolt_points: Vec::new(),
      bolt_visible: Vec::new(),
      last_seen: [0; MAX_PLAYERS],
      last_bytes: [0; MAX_PLAYERS],
      last_bolt_bytes: [0; MAX_PLAYERS],
      packed: true,
      relative: true,
      bots: 0,
      view: crate::default_view(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.roster.seat_of(&player)
  }

  /// Rebuilds the spatial index for this tick, under the current strategy.
  pub fn reindex(&mut self) {
    let cell = cell_for(self.view);
    if self.field.strategy() != self.strategy || (self.field.cell() - cell).abs() > 0.01 {
      self.field = Field::new(cell, self.strategy);
    }
    self.space.positions(&mut self.points);
    self.field.rebuild(&self.points);

    if self.bolt_field.strategy() != self.strategy || (self.bolt_field.cell() - cell).abs() > 0.01 {
      self.bolt_field = Field::new(cell, self.strategy);
    }
    self.bolt_points.clear();
    self.bolt_points.extend(self.space.bolts.iter().map(|b| b.at));
    self.bolt_field.rebuild(&self.bolt_points);
  }

  /// The bolts a seat can see.
  ///
  /// Relevance applies to these exactly as it does to ships, and it matters
  /// more: a firefight on the far side of the volume is the traffic a viewer
  /// has no use for, and there is a great deal of it.
  pub fn bolts_visible_to(&mut self, seat: usize) -> &[u32] {
    let at = self.space.ships[seat].at;
    self.bolt_field.query(at, self.view, &mut self.bolt_visible, &[]);
    &self.bolt_visible
  }

  /// The seats a given seat can see, including itself.
  ///
  /// Itself unconditionally: a client that cannot see its own ship has nothing
  /// to fly, and a radius is no reason to omit the one entity it is guaranteed
  /// to be at the centre of.
  pub fn visible_to(&mut self, seat: usize) -> &[u32] {
    let at = self.space.ships[seat].at;
    // No truth set: this is the serving path, not the measuring one, and
    // scoring every query against a brute-force sweep would make relevance
    // cost more than it saves.
    self.field.query(at, self.view, &mut self.visible, &[]);
    self.visible.retain(|id| self.space.ships[*id as usize].alive);
    if !self.visible.contains(&(seat as u32)) {
      self.visible.push(seat as u32);
    }
    self.last_seen[seat] = self.visible.len();
    &self.visible
  }

  pub fn apply(&mut self, seat: usize, fly: Fly) {
    self.flying[seat] = fly;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_client_always_sees_its_own_ship() {
    // Even alone in an empty volume, where a radius query returns nothing.
    let mut state = SpaceState::new();
    state.space.spawn(0);
    state.reindex();
    let seen = state.visible_to(0).to_vec();
    assert!(seen.contains(&0), "it has to have something to fly: {seen:?}");
  }

  #[test]
  fn a_ship_out_of_range_is_not_sent() {
    let mut state = SpaceState::new();
    state.space.spawn(0);
    state.space.spawn(1);
    state.space.ships[0].at = Vec3::new(0.0, 0.0, 0.0);
    state.space.ships[1].at = Vec3::new(crate::default_view() * 3.0, 0.0, 0.0);
    state.reindex();
    let seen = state.visible_to(0).to_vec();
    assert_eq!(seen, vec![0], "only itself: {seen:?}");

    state.space.ships[1].at = Vec3::new(crate::default_view() * 0.5, 0.0, 0.0);
    state.reindex();
    let mut seen = state.visible_to(0).to_vec();
    seen.sort();
    assert_eq!(seen, vec![0, 1], "and now both: {seen:?}");
  }

  #[test]
  fn altitude_is_only_respected_by_the_strategies_that_look_at_it() {
    // The example's whole claim, at the smallest scale that shows it: two
    // ships at the same (x, z) and far apart in y.
    for (strategy, expect) in [
      (Strategy::Flat, 2),
      (Strategy::FlatBand, 1),
      (Strategy::Volume, 1),
    ] {
      let mut state = SpaceState::with(strategy);
      state.space.spawn(0);
      state.space.spawn(1);
      state.space.ships[0].at = Vec3::new(0.0, 0.0, 0.0);
      // Clear of the view and still inside the volume. A multiple of the radius
      // wrapped at the boundary once the radius grew, putting the ship back
      // *inside* the view from the other side and quietly inverting the test.
      state.space.ships[1].at = Vec3::new(0.0, crate::sim::VOLUME * 0.95, 0.0);
      state.reindex();
      let seen = state.visible_to(0).len();
      assert_eq!(seen, expect, "{} saw {seen}", strategy.name());
    }
  }

  #[test]
  fn a_dead_seat_is_never_sent() {
    let mut state = SpaceState::with(Strategy::Flat);
    state.space.spawn(0);
    // Seat 1 has never joined, so its ship sits at the origin, not alive.
    state.space.ships[0].at = Vec3::ZERO;
    state.reindex();
    let seen = state.visible_to(0).to_vec();
    assert_eq!(seen, vec![0], "an empty seat is not a ship: {seen:?}");
  }
}
