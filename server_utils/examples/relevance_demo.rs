//! Relevance demo: why a multiplayer world with many entities needs interest
//! management, and how the `relevance` building blocks provide it.
//!
//! A world larger than one screen holds more entities than fit on the wire, and
//! its players stand in different places. Sending every entity to every player is
//! `players x entities` per tick and does not scale. This runs a field of static
//! entities with several moving players and, each tick, sends each player only
//! what is near it, streaming the churn (what entered or left its view). Headless
//! and deterministic:
//!
//! ```sh
//! cargo run --example relevance_demo -p plaza_server_utils
//! ```
//!
//! It reports the bandwidth the relevance filter saves over the naive broadcast,
//! and the per-player spawn/despawn stream as players move, exactly what a horde
//! game (thousands of short-lived entities) leans on.

use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid, VisibilitySet};

const WORLD: f32 = 2000.0;
const ENTITY_COUNT: u32 = 2000;
const VIEW_RADIUS: f32 = 250.0;
const CELL_SIZE: f32 = 128.0;
const PLAYERS: usize = 4;
const TICKS: usize = 90;

/// A deterministic scatter of entities across the field.
fn entity_pos(i: u32) -> (f32, f32) {
  let x = ((i.wrapping_mul(2_654_435_761) >> 8) % 20000) as f32 / 20000.0 * WORLD;
  let y = ((i.wrapping_mul(40_503) >> 4) % 20000) as f32 / 20000.0 * WORLD;
  (x, y)
}

/// A player drifting on its own deterministic path across the field.
fn player_pos(p: usize, tick: usize) -> (f32, f32) {
  let phase = p as f32 * 1.7;
  let t = tick as f32 * 0.06;
  let cx = WORLD * 0.5;
  let cy = WORLD * 0.5;
  let r = WORLD * (0.15 + 0.08 * p as f32);
  (cx + r * (t + phase).cos(), cy + r * (t * 0.8 + phase).sin())
}

fn main() {
  let quantizer = GridQuantizer::new((0.0, 0.0), CELL_SIZE);
  let mut grid: SpatialGrid<u32> = SpatialGrid::new(quantizer);

  // Per-player: the previous tick's visible set, so we can diff for entered/left.
  let mut prev: Vec<VisibilitySet> = (0..PLAYERS).map(|_| VisibilitySet::with_capacity(ENTITY_COUNT)).collect();
  let mut cur: Vec<VisibilitySet> = (0..PLAYERS).map(|_| VisibilitySet::with_capacity(ENTITY_COUNT)).collect();

  // Reusable scratch, so the per-tick loop does not allocate.
  let mut candidates: Vec<u32> = Vec::new();
  let mut entered: Vec<u32> = Vec::new();
  let mut left: Vec<u32> = Vec::new();

  let mut total_relevant = 0u64;
  let mut total_entered = 0u64;
  let mut total_left = 0u64;

  println!("relevance demo: {ENTITY_COUNT} entities, {PLAYERS} players, view radius {VIEW_RADIUS:.0} in a {WORLD:.0} world\n");
  println!("{:>5}  {:>18}  {:>16}  {:>16}", "tick", "relevant/player", "entered (spawn)", "left (despawn)");

  for tick in 0..TICKS {
    // Rebuild the spatial index for this tick (cheap: buckets keep capacity).
    grid.clear();
    for i in 0..ENTITY_COUNT {
      let (x, y) = entity_pos(i);
      grid.insert(i, x, y);
    }

    let mut tick_relevant = 0usize;
    let mut tick_entered = 0usize;
    let mut tick_left = 0usize;

    for p in 0..PLAYERS {
      let (px, py) = player_pos(p, tick);

      // Gather the cell-granular candidates, then narrow to the exact radius.
      candidates.clear();
      grid.query_radius(px, py, VIEW_RADIUS, &mut candidates);
      cur[p].clear();
      for &id in &candidates {
        let (ex, ey) = entity_pos(id);
        if ((ex - px).powi(2) + (ey - py).powi(2)).sqrt() <= VIEW_RADIUS {
          cur[p].insert(id);
        }
      }

      // The spawn/despawn stream since last tick.
      entered.clear();
      left.clear();
      cur[p].diff(&prev[p], &mut entered, &mut left);

      tick_relevant += cur[p].count();
      tick_entered += entered.len();
      tick_left += left.len();

      std::mem::swap(&mut prev[p], &mut cur[p]);
    }

    total_relevant += tick_relevant as u64;
    total_entered += tick_entered as u64;
    total_left += tick_left as u64;

    if tick % 15 == 0 {
      println!(
        "{:>5}  {:>18.1}  {:>16}  {:>16}",
        tick,
        tick_relevant as f32 / PLAYERS as f32,
        tick_entered,
        tick_left,
      );
    }
  }

  let ticks = TICKS as u64;
  let naive_per_tick = PLAYERS as u64 * ENTITY_COUNT as u64;
  let relevant_per_tick = total_relevant / ticks;
  println!("\naverage relevant per player per tick: {:.1}", total_relevant as f32 / (ticks * PLAYERS as u64) as f32);
  println!("full-state broadcast: {naive_per_tick} entity-sends/tick   relevance: {relevant_per_tick}/tick   ({:.0}% culled)", (1.0 - relevant_per_tick as f32 / naive_per_tick as f32) * 100.0);
  println!("and after the first tick you send deltas, not the set: {} spawns + {} despawns/tick on average", total_entered / ticks, total_left / ticks);
  println!("that gap is the difference between a multiplayer horde that scales and one that saturates the link.");
}
