//! A trainer on a tile map, and what discreteness is worth on the wire.
//!
//! Every bandwidth figure in this tree is measured against **continuous**
//! motion: horde's floats, cube_yard's quantised metres, spacemo's ships. A
//! world where movement is a step from one tile to the next is not a smaller
//! version of that, it is a different arithmetic. A position is an index rather
//! than a measurement, so it needs no bounds, no quantiser and no precision
//! argument: on a 1024 by 1024 map it is twenty bits, exactly, for ever.
//!
//! **The size is not the point, and measuring it said so.** Against a naive
//! wire of two floats and an angle a tile is 2.9x smaller; against what every
//! other example here actually sends, a quantised position, it is 1.4x. That is
//! a modest saving and an honest one.
//!
//! What discreteness buys instead is **exactness**. A tile is an index, so it
//! has no bounds to outgrow, no quantiser, no precision to argue about, and two
//! machines comparing positions can use `==`. cube_yard shipped a bug that
//! cannot exist here, by widening its world past the range its quantiser
//! covered and freezing everything that wandered out. The saving is a side
//! effect; not needing the apparatus is the result.

use serde::{Deserialize, Serialize};

/// Tiles across, and down. A power of two so a coordinate is a clean bit width
/// rather than a range needing a bound.
pub const MAP: u32 = 1024;
/// Bits one axis costs, derived rather than written down beside it.
pub const AXIS_BITS: u32 = MAP.trailing_zeros();

const _: () = assert!(MAP.is_power_of_two(), "a tile coordinate should be a bit width, not a bound");

/// Which way a trainer is facing, and the only directions one can move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facing {
  #[default]
  South,
  North,
  East,
  West,
}

impl Facing {
  pub const ALL: [Facing; 4] = [Facing::South, Facing::North, Facing::East, Facing::West];

  /// The tile one step this way, or `None` at the edge of the map.
  pub fn step(self, from: Tile) -> Option<Tile> {
    let (dx, dy): (i32, i32) = match self {
      Facing::South => (0, 1),
      Facing::North => (0, -1),
      Facing::East => (1, 0),
      Facing::West => (-1, 0),
    };
    let x = from.x as i32 + dx;
    let y = from.y as i32 + dy;
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
      return None;
    }
    Some(Tile {
      x: x as u32,
      y: y as u32,
    })
  }
}

/// A place on the map. Not a measurement: there is nothing between two tiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tile {
  pub x: u32,
  pub y: u32,
}

impl Tile {
  pub fn new(x: u32, y: u32) -> Self {
    Self { x, y }
  }

  /// Manhattan distance, which is also the number of steps between them.
  pub fn steps_to(self, other: Tile) -> u32 {
    self.x.abs_diff(other.x) + self.y.abs_diff(other.y)
  }

  /// Chebyshev distance, which is what a square view radius measures.
  pub fn within(self, other: Tile, radius: u32) -> bool {
    self.x.abs_diff(other.x) <= radius && self.y.abs_diff(other.y) <= radius
  }
}

/// How far through a step a trainer is, in sixteenths.
///
/// The whole of what a client needs to draw motion between two tiles, and the
/// reason a step is an animation rather than a position: the *rule* is that a
/// trainer occupies one tile or the next, and this says how far along that is.
/// Four bits, because nobody can see a sixteenth of a tile of error.
pub const PHASE_BITS: u32 = 4;
pub const PHASE_STEPS: u8 = 1 << PHASE_BITS;

/// One trainer, as the wire carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trainer {
  pub seat: u16,
  pub at: Tile,
  pub facing: Facing,
  /// Zero when standing still, otherwise how far into the step to the tile
  /// ahead.
  pub phase: u8,
}

impl Trainer {
  /// Where to draw it, in tiles, with the step played out.
  ///
  /// The client's whole interpolation problem, and it is arithmetic rather than
  /// a buffer of samples: a step has a known start, a known end and a known
  /// duration, so there is nothing to predict and nothing to smooth.
  pub fn drawn(&self) -> (f32, f32) {
    let t = self.phase as f32 / PHASE_STEPS as f32;
    let Some(next) = self.facing.step(self.at) else {
      return (self.at.x as f32, self.at.y as f32);
    };
    (
      self.at.x as f32 + (next.x as f32 - self.at.x as f32) * t,
      self.at.y as f32 + (next.y as f32 - self.at.y as f32) * t,
    )
  }
}

/// What one trainer costs on the wire as tiles, derived from the layout.
pub const fn trainer_bits() -> usize {
  (SEAT_BITS + AXIS_BITS * 2 + FACING_BITS + PHASE_BITS) as usize
}

/// The same trainer as a continuous position at full width: two floats and an
/// angle, which is what a naive wire sends.
pub const fn trainer_bits_continuous() -> usize {
  (SEAT_BITS + 32 * 2 + 32) as usize
}

/// And as a *quantised* continuous position, which is what every other example
/// in this tree actually sends.
///
/// The fair comparison, and the one that makes the point. cube_yard spends 16
/// bits an axis over a bounded world and 9 on an angle; against that a tile is
/// not dramatically smaller. What it is instead is **exact**, and free of the
/// apparatus: no bounds to outgrow, no quantiser, no precision argument, and no
/// clamp waiting to freeze something that wandered past the edge of a range.
pub const fn trainer_bits_quantised() -> usize {
  (SEAT_BITS + 16 * 2 + 9) as usize
}

pub const SEAT_BITS: u32 = 10;
pub const FACING_BITS: u32 = 2;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_step_stops_at_the_edge_rather_than_wrapping() {
    // A tile map has a real edge, unlike spacemo's volume: there is nowhere for
    // a trainer to be beyond it, so the step simply does not happen.
    let corner = Tile::new(0, 0);
    assert_eq!(Facing::North.step(corner), None);
    assert_eq!(Facing::West.step(corner), None);
    assert_eq!(Facing::South.step(corner), Some(Tile::new(0, 1)));

    let far = Tile::new(MAP - 1, MAP - 1);
    assert_eq!(Facing::South.step(far), None);
    assert_eq!(Facing::East.step(far), None);
  }

  #[test]
  fn a_drawn_step_runs_from_one_tile_to_the_next_and_no_further() {
    let mut trainer = Trainer {
      seat: 0,
      at: Tile::new(10, 10),
      facing: Facing::East,
      phase: 0,
    };
    assert_eq!(trainer.drawn(), (10.0, 10.0));

    trainer.phase = PHASE_STEPS / 2;
    let (x, y) = trainer.drawn();
    assert!((x - 10.5).abs() < 0.001, "half a step east is half a tile: {x}");
    assert_eq!(y, 10.0);

    // A phase never reaches the next tile: arriving is what moves `at`.
    trainer.phase = PHASE_STEPS - 1;
    let (x, _) = trainer.drawn();
    assert!(x < 11.0, "the far tile belongs to the next step, not this one: {x}");
  }

  #[test]
  fn a_trainer_facing_the_edge_is_drawn_standing_still() {
    // Nothing to interpolate toward, and the alternative is drawing it off the
    // map for the duration of a step it cannot take.
    let trainer = Trainer {
      seat: 0,
      at: Tile::new(0, 0),
      facing: Facing::North,
      phase: PHASE_STEPS / 2,
    };
    assert_eq!(trainer.drawn(), (0.0, 0.0));
  }

  #[test]
  fn what_a_tile_costs_against_a_measurement() {
    let tile = trainer_bits();
    let full = trainer_bits_continuous();
    let quantised = trainer_bits_quantised();
    println!("\n  one trainer, on the wire:\n");
    println!("    continuous, full width   {full} bits");
    println!("    continuous, quantised    {quantised} bits");
    println!("    a tile                   {tile} bits");
    println!(
      "\n  {:.1}x against the naive version and {:.1}x against the one every\n  other example here actually sends. The size is not the point.\n",
      full as f32 / tile as f32,
      quantised as f32 / tile as f32
    );

    assert!(tile * 2 < full, "{tile} against {full}");
    // Against a fair opponent the saving is modest, which is the finding
    // rather than a disappointment: discreteness buys **exactness**, not
    // bytes. A tile is an index, so it has no bounds to outgrow and no
    // rounding to argue about, and two machines comparing positions can use
    // `==`. cube_yard shipped a bug that a tile cannot have, by widening its
    // world past the range its quantiser covered.
    assert!(tile < quantised, "a tile should still be smaller: {tile} against {quantised}");
    assert!(
      quantised as f32 / tile as f32 > 1.2 && (quantised as f32 / tile as f32) < 2.0,
      "and only modestly so, which is the honest comparison"
    );
    // No bounds: the map is a power of two, so an axis is a bit width rather
    // than a range with a quantiser and a clamp behind it.
    assert_eq!(AXIS_BITS, 10);
  }
}
