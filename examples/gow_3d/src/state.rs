//! What the server owns: one zone, and who is sitting in it.
//!
//! Thinner than the other examples' state, and that is the finding rather than
//! an omission. There is no predicted state to reconcile because the client's
//! position is the truth, and no simulation to step because the only thing with
//! a clock is a cast bar. What is left is a roster, a set of subscriptions, and
//! a timer per character.

use std::collections::HashMap;

use plaza_server_utils::Roster;

use crate::bots::Bots;

use crate::protocol::PlayerId;
use crate::relevance::Seat;
use crate::zone::Zone;

/// How many characters a zone seats unless something says otherwise.
///
/// The number the example is *played* at, not a ceiling the design rests on:
/// [`GowState::with_capacity`] takes any, and `examples/zone_scale.rs` runs the
/// sweep that says what the shape costs past it. Nothing here is sized in a way
/// that cares, which is the claim that sweep exists to check rather than
/// assert: the spawn spiral's radius grows as `sqrt(seat)` so density is flat,
/// and the audience query is a grid lookup rather than a scan.
pub const MAX_CHARACTERS: usize = 64;

/// Where a character starts.
///
/// A spiral rather than a ring, because a ring of a fixed angular step wraps:
/// the first version stepped 0.9 radians and put seat 7 on top of seat 0. The
/// golden angle is the one step that never repeats. The ground then decides
/// the height and nudges the point onto footing, so nobody spawns in the sea
/// or inside a cliff.
pub fn spawn_at(seat: Seat) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = 10.0 + (seat as f32).sqrt() * 7.0;
  crate::terrain::footing_near(angle.cos() * radius, angle.sin() * radius)
}

/// Where the zone's beasts live, spread wider than the adventurers so there is
/// somewhere to walk to before the fighting starts.
pub fn den_at(index: usize) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = index as f32 * GOLDEN + 0.7;
  let radius = 26.0 + (index as f32).sqrt() * 11.0;
  crate::terrain::footing_near(angle.cos() * radius, angle.sin() * radius)
}

pub struct GowState {
  pub zone: Zone,
  pub tick: u64,
  /// Seats this zone holds. Read where the zone seats its own characters, so
  /// bots and beasts scale with it rather than with a constant.
  pub capacity: usize,
  pub roster: Roster<PlayerId>,
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  /// Casts that landed on this tick, cleared when they have been sent.
  ///
  /// Held for exactly one tick because it is an event: keeping it longer would
  /// send it twice and clearing it earlier would lose it.
  pub landed: Vec<crate::protocol::Landed>,
  /// The zone's own adventurers, so a lone player has a world around them.
  pub bots: Bots,
  /// Whether the zone has seated its own characters yet.
  pub populated: bool,
  /// How the spatial channel reaches clients this tick.
  pub delivery: crate::protocol::Delivery,
  /// How positions inside cell payloads are written this tick.
  pub precision: crate::protocol::Precision,
  /// Last tick's publication, kept so a per-tick rebuild reuses its cells
  /// rather than allocating one payload slot per cell per tick.
  pub published: Option<crate::zone::Publication>,
  /// One assembled body blob per occupied viewer-cell, shared by refcount
  /// between every viewer standing in it.
  pub assembled: plaza_server_utils::relevance::CellTable<Option<crate::protocol::Packed>>,
  /// Viewers bucketed by the cell they stand in. Every viewer in one cell has
  /// the same window and the same near/far reading of it, so this is the key
  /// the whole addressing layer wants.
  pub viewers: plaza_server_utils::relevance::CellTable<Vec<PlayerId>>,
  /// Who is listening to each cell at the coarse width, under
  /// [`Precision::Graded`](crate::protocol::Precision::Graded).
  pub audience_far: plaza_server_utils::relevance::CellTable<Vec<PlayerId>>,
  /// Who is listening to each cell, under [`Delivery::Cells`](crate::protocol::Delivery::Cells).
  /// The inverse of a view query, and the cost that scheme pays instead of
  /// assembling a buffer per client.
  pub audience: plaza_server_utils::relevance::CellTable<Vec<PlayerId>>,
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
    Self::with_capacity(MAX_CHARACTERS)
  }

  /// A zone seating `capacity` characters.
  pub fn with_capacity(capacity: usize) -> Self {
    Self::spanning(capacity, crate::terrain::EDGE)
  }

  /// A zone seating `capacity` characters over a world reaching `extent` units
  /// from the origin. See [`Zone::spanning`] for why a measurement that
  /// spreads a population past [`crate::terrain::EDGE`] must say so.
  pub fn spanning(capacity: usize, extent: f32) -> Self {
    let zone = Zone::spanning(extent);
    let audience = plaza_server_utils::relevance::CellTable::new(*zone.space());
    let assembled = plaza_server_utils::relevance::CellTable::new(*zone.space());
    let viewers = plaza_server_utils::relevance::CellTable::new(*zone.space());
    let audience_far = plaza_server_utils::relevance::CellTable::new(*zone.space());
    Self {
      assembled,
      viewers,
      audience_far,
      delivery: crate::protocol::Delivery::default(),
      precision: crate::protocol::Precision::default(),
      published: None,
      audience,
      zone,
      tick: 0,
      capacity,
      roster: Roster::new(capacity),
      agents: HashMap::new(),
      landed: Vec::new(),
      bots: Bots::default(),
      populated: false,
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
