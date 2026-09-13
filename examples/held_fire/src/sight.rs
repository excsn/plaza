//! The ground rules both ends share: what blocks, what sees and the one
//! canonical path a march walks.
//!
//! Shared verbatim so the client can show reachable cells and lines of sight
//! without asking. The walk is deterministic to the cell, which lets a test
//! say exactly where an overwatch trigger fires.

use crate::protocol::{Cell, UnitId, MAP_H, MAP_W, MOVE_RANGE, ROCKS, SIGHT, WATCH_REACH};

pub fn rock(cell: Cell) -> bool {
  ROCKS.contains(&cell)
}

pub fn in_bounds(cell: Cell) -> bool {
  cell.0 < MAP_W && cell.1 < MAP_H
}

fn manhattan(a: Cell, b: Cell) -> u8 {
  (a.0.abs_diff(b.0) + a.1.abs_diff(b.1)) as u8
}

/// Neighbours in a fixed order, which is what makes the path canonical.
fn neighbours(cell: Cell) -> [Option<Cell>; 4] {
  let (x, y) = cell;
  [
    y.checked_sub(1).map(|y| (x, y)),
    Some((x + 1, y)).filter(|c| in_bounds(*c)),
    Some((x, y + 1)).filter(|c| in_bounds(*c)),
    x.checked_sub(1).map(|x| (x, y)),
  ]
}

/// Walking eyes: within [`SIGHT`] on a clear line. A unit can shoot at
/// anything it sees.
pub fn sees(from: Cell, to: Cell) -> bool {
  clear_within(from, to, SIGHT)
}

/// A watcher's lane: within [`WATCH_REACH`] on a clear line. Longer than
/// [`sees`] on purpose; ambushes happen in the band between the two.
pub fn watches(from: Cell, to: Cell) -> bool {
  clear_within(from, to, WATCH_REACH)
}

/// Line of sight within `range` and no rock on the Bresenham line between the
/// endpoints. Endpoints do not block themselves and units never block sight;
/// only rocks do.
fn clear_within(from: Cell, to: Cell, range: u8) -> bool {
  if manhattan(from, to) > range {
    return false;
  }
  let (mut x0, mut y0) = (from.0 as i32, from.1 as i32);
  let (x1, y1) = (to.0 as i32, to.1 as i32);
  let dx = (x1 - x0).abs();
  let dy = -(y1 - y0).abs();
  let sx = if x0 < x1 { 1 } else { -1 };
  let sy = if y0 < y1 { 1 } else { -1 };
  let mut err = dx + dy;
  loop {
    if (x0, y0) != (from.0 as i32, from.1 as i32) && (x0, y0) != (x1, y1) && rock((x0 as u8, y0 as u8)) {
      return false;
    }
    if (x0, y0) == (x1, y1) {
      return true;
    }
    let e2 = 2 * err;
    if e2 >= dy {
      err += dy;
      x0 += sx;
    }
    if e2 <= dx {
      err += dx;
      y0 += sy;
    }
  }
}

/// The canonical path from `from` to `to`, `from` excluded, or `None` when
/// `to` is out of range or unreachable. Breadth-first over the fixed
/// neighbour order, rocks and standing units block, the destination must be
/// empty.
pub fn path(from: Cell, to: Cell, occupied: &[Cell]) -> Option<Vec<Cell>> {
  if from == to || !in_bounds(to) || rock(to) || occupied.contains(&to) {
    return None;
  }
  let blocked = |cell: Cell| rock(cell) || occupied.contains(&cell);
  let mut parent: std::collections::HashMap<Cell, Cell> = std::collections::HashMap::new();
  let mut frontier = std::collections::VecDeque::from([(from, 0u8)]);
  while let Some((cell, cost)) = frontier.pop_front() {
    if cost == MOVE_RANGE {
      continue;
    }
    for next in neighbours(cell).into_iter().flatten() {
      if next != to && blocked(next) {
        continue;
      }
      if next == from || parent.contains_key(&next) {
        continue;
      }
      parent.insert(next, cell);
      if next == to {
        let mut walk = vec![to];
        let mut back = to;
        while let Some(prev) = parent.get(&back).copied().filter(|p| *p != from) {
          walk.push(prev);
          back = prev;
        }
        walk.reverse();
        return Some(walk);
      }
      frontier.push_back((next, cost + 1));
    }
  }
  None
}

/// Every cell a unit at `from` may march to.
pub fn reachable(from: Cell, occupied: &[Cell]) -> Vec<Cell> {
  let mut out = Vec::new();
  for x in 0..MAP_W {
    for y in 0..MAP_H {
      if path(from, (x, y), occupied).is_some() {
        out.push((x, y));
      }
    }
  }
  out
}

/// Which enemy units a side sees: any of the side's living units has line of
/// sight to it.
pub fn side_sees(eyes: &[(UnitId, Cell)], target: Cell) -> bool {
  eyes.iter().any(|(_, at)| sees(*at, target))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_rock_blocks_the_line_and_the_edge_does_not() {
    assert!(sees((0, 0), (5, 0)), "an open lane at exactly walking sight");
    assert!(!sees((5, 4), (7, 4)), "the rock at (6,4) blocks straight through");
    assert!(!sees((0, 0), (6, 0)), "distance alone refuses past SIGHT");
    assert!(watches((0, 0), (7, 0)), "a watcher's lane reaches further");
    assert!(!watches((0, 0), (8, 0)), "and stops at its own edge");
  }

  #[test]
  fn the_path_is_canonical_and_avoids_rocks_and_bodies() {
    let walk = path((5, 4), (7, 4), &[]).expect("a way around the rock exists");
    assert_eq!(walk.last(), Some(&(7, 4)));
    assert!(!walk.iter().any(|c| rock(*c)), "no step lands on a rock");
    assert_eq!(walk, path((5, 4), (7, 4), &[]).unwrap(), "the same ground walks the same way");

    let blocked = path((0, 0), (0, 2), &[(0, 1), (1, 0), (1, 1), (1, 2)]);
    assert!(blocked.is_none(), "bodies wall the corridor shut");
  }

  #[test]
  fn range_bounds_the_march() {
    assert!(path((0, 0), (0, 4), &[]).is_some(), "four is the range");
    assert!(path((0, 0), (0, 5), &[]).is_none(), "five is past it");
  }
}
