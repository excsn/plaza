//! The rule, written once and called by both sides.
//!
//! Nothing here reads a clock, a connection or a seat. Movement, sight lines
//! and what a ray hits are the same function on the server, in a client's
//! prediction, and in the offline harness, which is the only reason a client's
//! guess is ever worth anything.

use crate::sim::types::{ARENA_H, ARENA_W, PLAYER_R, PlayerId, PlayerSnap, V2, WALLS, Wall};

const SKIN: f32 = 0.01;

/// Keeps a circle inside the arena.
pub fn clamp_to_arena(p: V2, r: f32) -> V2 {
  V2::new(p.x.clamp(r, ARENA_W - r), p.y.clamp(r, ARENA_H - r))
}

fn push_out_x(mut p: V2, r: f32) -> V2 {
  for wall in WALLS {
    let e = wall.expanded(r);
    if e.contains(p) {
      let left = e.x;
      let right = e.x + e.w;
      p.x = if (p.x - left).abs() <= (right - p.x).abs() { left - SKIN } else { right + SKIN };
    }
  }
  p
}

fn push_out_y(mut p: V2, r: f32) -> V2 {
  for wall in WALLS {
    let e = wall.expanded(r);
    if e.contains(p) {
      let top = e.y;
      let bottom = e.y + e.h;
      p.y = if (p.y - top).abs() <= (bottom - p.y).abs() { top - SKIN } else { bottom + SKIN };
    }
  }
  p
}

/// Moves a circle by a delta, sliding along whatever it runs into.
///
/// The two axes are resolved separately and in a fixed order. A single combined
/// resolution has to choose a direction to push out of a corner, and the choice
/// depends on floating point noise, which makes the same input produce
/// different results on two machines that agree about everything else.
pub fn move_circle(from: V2, delta: V2, r: f32) -> V2 {
  let mut p = V2::new(from.x + delta.x, from.y);
  p = push_out_x(p, r);
  p = V2::new(p.x, p.y + delta.y);
  p = push_out_y(p, r);
  clamp_to_arena(p, r)
}

/// Distance along a unit ray to the near face of a box, if it is hit at all.
fn ray_wall(from: V2, dir: V2, wall: Wall) -> Option<f32> {
  let mut tmin = 0.0f32;
  let mut tmax = f32::INFINITY;
  let axes = [(from.x, dir.x, wall.x, wall.x + wall.w), (from.y, dir.y, wall.y, wall.y + wall.h)];
  for (origin, d, lo, hi) in axes {
    if d.abs() < 1e-6 {
      if origin < lo || origin > hi {
        return None;
      }
    } else {
      let a = (lo - origin) / d;
      let b = (hi - origin) / d;
      let (near, far) = if a > b { (b, a) } else { (a, b) };
      tmin = tmin.max(near);
      tmax = tmax.min(far);
      if tmin > tmax {
        return None;
      }
    }
  }
  Some(tmin)
}

/// Distance along a unit ray to the nearest wall.
pub fn ray_to_wall(from: V2, dir: V2) -> Option<f32> {
  WALLS.iter().filter_map(|w| ray_wall(from, dir, *w)).fold(None, |acc: Option<f32>, t| Some(acc.map_or(t, |best| best.min(t))))
}

/// Distance along a unit ray to the near surface of a circle.
pub fn ray_to_circle(from: V2, dir: V2, centre: V2, r: f32) -> Option<f32> {
  let m = from.sub(centre);
  let b = m.dot(dir);
  let c = m.dot(m) - r * r;
  // Origin outside the circle and the ray pointing away from it.
  if c > 0.0 && b > 0.0 {
    return None;
  }
  let disc = b * b - c;
  if disc < 0.0 {
    return None;
  }
  Some((-b - disc.sqrt()).max(0.0))
}

/// Whether two points can see each other.
pub fn line_of_sight(a: V2, b: V2) -> bool {
  let delta = b.sub(a);
  let dist = delta.len();
  if dist <= f32::EPSILON {
    return true;
  }
  match ray_to_wall(a, delta.scale(1.0 / dist)) {
    Some(t) => t >= dist,
    None => true,
  }
}

/// What a ray found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
  pub target: Option<PlayerId>,
  /// Where the ray stopped: a body, a wall, or the end of its range.
  pub point: V2,
  pub dist: f32,
}

/// Casts a shot through a set of bodies, stopped by cover.
///
/// The bodies are passed in rather than read from a world, because the whole
/// point of this example is that the server calls this twice with two different
/// sets: where the targets are now, and where the shooter last saw them.
pub fn cast(from: V2, aim: V2, range: f32, bodies: &[(PlayerId, PlayerSnap)]) -> RayHit {
  let dir = aim.normalized();
  if dir == V2::ZERO {
    return RayHit { target: None, point: from, dist: 0.0 };
  }
  let wall = ray_to_wall(from, dir).unwrap_or(f32::INFINITY);
  let stop = wall.min(range);

  let mut best: Option<(f32, PlayerId)> = None;
  for (id, snap) in bodies {
    if !snap.alive {
      continue;
    }
    let Some(t) = ray_to_circle(from, dir, snap.pos, PLAYER_R) else { continue };
    if t > stop {
      continue;
    }
    if best.is_none_or(|(bt, _)| t < bt) {
      best = Some((t, *id));
    }
  }

  match best {
    Some((t, id)) => RayHit { target: Some(id), point: from.add(dir.scale(t)), dist: t },
    None => RayHit { target: None, point: from.add(dir.scale(stop)), dist: stop },
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::ROCKET_BLAST_R;

  fn body(id: PlayerId, x: f32, y: f32) -> (PlayerId, PlayerSnap) {
    (id, PlayerSnap { pos: V2::new(x, y), alive: true })
  }

  const RIFLE_RANGE_T: f32 = 900.0;

  /// The one horizontal line that crosses the whole map with nothing on it.
  ///
  /// Named rather than written as a literal at each call site, because the
  /// first draft of these tests used y 100, which runs straight through both
  /// left pillars, and every one of them failed for a reason that had nothing
  /// to do with the code under test.
  const OPEN_LANE_Y: f32 = 162.0;

  #[test]
  fn a_wall_stops_a_shot_that_would_otherwise_land() {
    // The left pillar spans x 140..164, y 60..150.
    let blocked = cast(V2::new(20.0, 100.0), V2::new(1.0, 0.0), RIFLE_RANGE_T, &[body(1, 300.0, 100.0)]);
    assert_eq!(blocked.target, None, "the pillar is in the way");

    let open = cast(V2::new(20.0, OPEN_LANE_Y), V2::new(1.0, 0.0), RIFLE_RANGE_T, &[body(1, 300.0, OPEN_LANE_Y)]);
    assert_eq!(open.target, Some(1), "and below it the lane is open");
  }

  #[test]
  fn the_nearest_body_takes_the_shot() {
    let from = V2::new(20.0, OPEN_LANE_Y);
    let hit = cast(from, V2::new(1.0, 0.0), RIFLE_RANGE_T, &[body(2, 400.0, OPEN_LANE_Y), body(1, 200.0, OPEN_LANE_Y)]);
    assert_eq!(hit.target, Some(1), "order in the slice must not decide it");
  }

  #[test]
  fn a_dead_body_does_not_block_or_absorb_a_shot() {
    let from = V2::new(20.0, OPEN_LANE_Y);
    let corpse = (1u8, PlayerSnap { pos: V2::new(200.0, OPEN_LANE_Y), alive: false });
    let hit = cast(from, V2::new(1.0, 0.0), RIFLE_RANGE_T, &[corpse, body(2, 400.0, OPEN_LANE_Y)]);
    assert_eq!(hit.target, Some(2));
  }

  #[test]
  fn a_shot_past_its_range_reaches_nothing() {
    let hit = cast(V2::new(20.0, OPEN_LANE_Y), V2::new(1.0, 0.0), 100.0, &[body(1, 400.0, OPEN_LANE_Y)]);
    assert_eq!(hit.target, None);
    assert!((hit.dist - 100.0).abs() < 0.01);
  }

  #[test]
  fn sight_is_symmetric() {
    // Asymmetric line of sight would mean the panel's "behind cover" count
    // depended on which end the server asked from.
    let pairs = [
      (V2::new(200.0, 200.0), V2::new(500.0, 200.0)),
      (V2::new(60.0, 60.0), V2::new(580.0, 340.0)),
      (V2::new(150.0, 30.0), V2::new(150.0, 380.0)),
    ];
    for (a, b) in pairs {
      assert_eq!(line_of_sight(a, b), line_of_sight(b, a), "{a:?} {b:?}");
    }
  }

  #[test]
  fn walking_into_a_wall_slides_along_it_rather_than_stopping_dead() {
    // Into the left face of the central block, moving down and right.
    let start = V2::new(240.0, 150.0);
    let after = move_circle(start, V2::new(6.0, 6.0), PLAYER_R);
    assert!(after.y > start.y, "the free axis still moved");
    for wall in WALLS {
      assert!(!wall.expanded(PLAYER_R).contains(after), "and it did not end up inside {wall:?}");
    }
  }

  #[test]
  fn no_delta_can_push_a_player_inside_a_wall() {
    let mut p = V2::new(60.0, 60.0);
    let mut seed = 12345u64;
    for _ in 0..4000 {
      seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
      let dx = ((seed >> 33) % 21) as f32 - 10.0;
      let dy = ((seed >> 13) % 21) as f32 - 10.0;
      p = move_circle(p, V2::new(dx, dy), PLAYER_R);
      for wall in WALLS {
        assert!(!wall.expanded(PLAYER_R).contains(p), "{p:?} ended inside {wall:?}");
      }
      assert!(p.x >= PLAYER_R - 0.1 && p.x <= ARENA_W - PLAYER_R + 0.1);
      assert!(p.y >= PLAYER_R - 0.1 && p.y <= ARENA_H - PLAYER_R + 0.1);
    }
  }

  #[test]
  fn a_blast_radius_reaches_further_than_a_body() {
    assert!(ROCKET_BLAST_R > PLAYER_R * 2.0, "otherwise a rocket is a slower rifle");
  }
}
