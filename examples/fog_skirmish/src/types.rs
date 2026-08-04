use std::collections::HashMap;

use plaza::agent::Agent;
use serde::{Deserialize, Serialize};

use crate::vision::RelicGrid;

pub type PlayerId = u32;
pub type UnitId = u32;
pub type RelicId = u32;

pub const FIELD: f32 = 200.0;
/// Enough that a per-recipient view is a small fraction of the world, which is
/// the whole reason the query has to be a query rather than a filter.
pub const RELICS: usize = 240;
pub const SCOUTS_PER_PLAYER: usize = 3;
pub const VISION: f32 = 26.0;
pub const SCOUT_SPEED: f32 = 16.0;
pub const CAPTURE_RADIUS: f32 = 4.5;
/// Ticks a scout must hold position on a relic, one second at 60Hz.
pub const CAPTURE_TICKS: u32 = 60;
/// Grid cell for the relevance query. Near the vision radius, so a query
/// touches a handful of cells rather than one huge one or hundreds of tiny.
pub const CELL: f32 = 25.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FogOp {
  // Client to server.
  /// Send my scouts to a point. The only input in the game.
  MoveTo { x: f32, y: f32 },
  /// Turn the deferral off, so events are told the moment they happen. The
  /// fault injection: it is what makes the leak counter move.
  SetLeakMode(bool),

  // Server to client.
  /// Which player you are, once, on joining.
  Welcome { you: PlayerId },
  /// Your view of the world, built for you and nobody else.
  Snapshot(Box<PlayerView>),
  /// A relic changed hands somewhere you can see.
  ///
  /// `late` marks one you are being told after the fact, because when it
  /// happened you could not see the place it happened in.
  Captured {
    relic: RelicId,
    x: f32,
    y: f32,
    by: PlayerId,
    tick: u64,
    late: bool,
  },
}

#[derive(Clone, Debug)]
pub struct Unit {
  pub id: UnitId,
  pub owner: PlayerId,
  pub x: f32,
  pub y: f32,
  /// Where this scout is heading, if anywhere.
  pub to: Option<(f32, f32)>,
}

#[derive(Clone, Debug)]
pub struct Relic {
  pub id: RelicId,
  pub x: f32,
  pub y: f32,
  pub owner: Option<PlayerId>,
  /// Ticks the current claimant has held it for.
  pub progress: u32,
  pub claimant: Option<PlayerId>,
}

/// An event a player has not been allowed to hear yet.
///
/// It is kept whole rather than summarised: when the place it happened in comes
/// into view, this is delivered as it was, with `late` set. That is what keeps
/// two boards telling the same story despite one of them hearing it minutes
/// after the other.
#[derive(Clone, Debug)]
pub struct Withheld {
  pub relic: RelicId,
  pub x: f32,
  pub y: f32,
  pub by: PlayerId,
  pub tick: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerStats {
  pub told: u64,
  pub told_late: u64,
  /// Positions that reached this player which they could not see at the time.
  /// Audited on the way out rather than asserted in a comment.
  pub leaks: u64,
  /// Relics the grid query offered, against the ones that survived the exact
  /// distance test. The difference is what the index saves.
  pub considered: u64,
  pub sent: u64,
}

#[derive(Clone, Debug)]
pub struct Player {
  pub agent: Agent<PlayerId>,
  pub bot: bool,
  pub score: u32,
  pub withheld: Vec<Withheld>,
  pub stats: PlayerStats,
}

#[derive(Clone, Debug, Default)]
pub struct FogState {
  pub players: HashMap<PlayerId, Player>,
  pub units: Vec<Unit>,
  pub relics: Vec<Relic>,
  pub grid: RelicGrid,
  pub tick: u64,
  /// When true, captures are told to everyone the instant they happen. The
  /// game plays identically and the secrecy is gone, which is the point.
  pub leak_mode: bool,
  pub next_unit: UnitId,
}

impl FogState {
  pub fn new() -> Self {
    let relics = scatter(RELICS);
    let grid = RelicGrid::build(&relics, CELL);
    Self {
      relics,
      grid,
      ..Default::default()
    }
  }

  pub fn units_of(&self, player: PlayerId) -> impl Iterator<Item = &Unit> {
    self.units.iter().filter(move |u| u.owner == player)
  }
}

/// A fixed scatter, so a run is reproducible and the example needs no `rand`.
///
/// Two coprime strides over the field, which spreads relics without clustering
/// on a lattice the way a plain grid would.
fn scatter(count: usize) -> Vec<Relic> {
  (0..count)
    .map(|i| {
      let i = i as f32;
      Relic {
        id: i as RelicId,
        x: (i * 47.0) % FIELD,
        y: (i * 29.0 + (i * 13.0) % 17.0) % FIELD,
        owner: None,
        progress: 0,
        claimant: None,
      }
    })
    .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UnitView {
  pub id: UnitId,
  pub owner: PlayerId,
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelicView {
  pub id: RelicId,
  pub x: f32,
  pub y: f32,
  pub owner: Option<PlayerId>,
  pub claimant: Option<PlayerId>,
  pub progress: u32,
}

/// What the panel reads. Every number here is about the outbound stream rather
/// than the frames carrying it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PanelView {
  pub relics_in_world: usize,
  pub relics_visible: usize,
  pub relics_considered: u64,
  pub told: u64,
  pub told_late: u64,
  pub withheld_now: usize,
  pub leaks: u64,
  pub leak_mode: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlayerView {
  pub you: PlayerId,
  pub tick: u64,
  pub my_units: Vec<UnitView>,
  /// Only enemies inside one of your scouts' vision. An enemy you cannot see
  /// is absent, not flagged: there is nothing in this payload to hide.
  pub enemy_units: Vec<UnitView>,
  /// Only relics you can see right now. Your client remembers the rest.
  pub relics: Vec<RelicView>,
  pub scores: Vec<(PlayerId, u32)>,
  pub panel: PanelView,
}
