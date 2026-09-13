//! The perspective query, and the audit that checks its work.
//!
//! Relevance here is about secrecy. horde culls to save bytes and can be
//! approximate. This example culls because a client must never hold what its
//! player cannot see, so sending even slightly too much is a cheat.
//!
//! This module holds the query that decides what a player may be sent and
//! [`leaks_in`], which checks whether a finished op names a place that player
//! could not see. Nothing calls `leaks_in` to decide what to send. It only
//! checks the query's output.

use std::collections::HashMap;

use crate::types::{FogOp, FogState, PlayerId, Relic, RelicId, CELL, VISION};

/// A uniform grid over the relics, which never move.
///
/// This example uses a spatial query on purpose. With 240 relics and a dozen
/// scouts a linear scan would also work. Bucketing makes the cost proportional
/// to what is near a scout, so it still works when the world is much larger.
#[derive(Clone, Debug, Default)]
pub struct RelicGrid {
  cell: f32,
  buckets: HashMap<(i32, i32), Vec<RelicId>>,
}

impl RelicGrid {
  pub fn build(relics: &[Relic], cell: f32) -> Self {
    let mut buckets: HashMap<(i32, i32), Vec<RelicId>> = HashMap::new();
    for relic in relics {
      buckets.entry(bucket_of(relic.x, relic.y, cell)).or_default().push(relic.id);
    }
    Self { cell, buckets }
  }

  /// Every relic in a cell the circle touches. A superset of the answer: the
  /// caller still has to measure, which is why [`visible_relics`] does.
  pub fn near(&self, x: f32, y: f32, radius: f32, out: &mut Vec<RelicId>) {
    let cell = if self.cell > 0.0 { self.cell } else { CELL };
    let (lo_x, lo_y) = bucket_of(x - radius, y - radius, cell);
    let (hi_x, hi_y) = bucket_of(x + radius, y + radius, cell);
    for bx in lo_x..=hi_x {
      for by in lo_y..=hi_y {
        if let Some(ids) = self.buckets.get(&(bx, by)) {
          out.extend_from_slice(ids);
        }
      }
    }
  }
}

fn bucket_of(x: f32, y: f32, cell: f32) -> (i32, i32) {
  ((x / cell).floor() as i32, (y / cell).floor() as i32)
}

fn within(ax: f32, ay: f32, bx: f32, by: f32, radius: f32) -> bool {
  let (dx, dy) = (ax - bx, ay - by);
  dx * dx + dy * dy <= radius * radius
}

/// Whether any of this player's scouts can see the point.
///
/// Every visibility decision in the example uses this. A player with no scouts
/// left sees nothing.
pub fn can_see(state: &FogState, player: PlayerId, x: f32, y: f32) -> bool {
  state.units_of(player).any(|u| within(u.x, u.y, x, y, VISION))
}

/// The relics this player may be sent, and how many the grid had to offer to
/// find them.
///
/// Compared with the total, the second number shows the work the index saved.
pub fn visible_relics(state: &FogState, player: PlayerId) -> (Vec<RelicId>, u64) {
  let mut candidates = Vec::new();
  for unit in state.units_of(player) {
    state.grid.near(unit.x, unit.y, VISION, &mut candidates);
  }
  let considered = candidates.len() as u64;

  candidates.sort_unstable();
  candidates.dedup();
  candidates.retain(|id| {
    let relic = &state.relics[*id as usize];
    can_see(state, player, relic.x, relic.y)
  });
  (candidates, considered)
}

/// Every place an op names, so the audit never has to guess.
///
/// There is no wildcard arm, so a new op variant does not compile until
/// someone has decided whether it reveals a position. pellet_maze leaked
/// through this gap: the frame was per-recipient and correct but the events
/// beside it named cells nobody had scouted.
pub fn positions_named(op: &FogOp) -> Vec<(f32, f32)> {
  match op {
    // Server to client.
    FogOp::Snapshot(view) => view
      .enemy_units
      .iter()
      .map(|u| (u.x, u.y))
      .chain(view.relics.iter().map(|r| (r.x, r.y)))
      .collect(),
    FogOp::Captured { x, y, .. } => vec![(*x, *y)],
    // Says who you are, and nothing about where anything is.
    FogOp::Welcome { .. } => Vec::new(),
    // Client to server: the player's own intent, never routed onward.
    FogOp::MoveTo { .. } | FogOp::SetLeakMode(_) => Vec::new(),
  }
}

/// How many places in this op the recipient could not see.
///
/// A player's own scouts are exempt because `positions_named` reads only
/// `enemy_units`, so a friendly position is never checked here.
pub fn leaks_in(state: &FogState, recipient: PlayerId, op: &FogOp) -> usize {
  positions_named(op)
    .into_iter()
    .filter(|(x, y)| !can_see(state, recipient, *x, *y))
    .count()
}
