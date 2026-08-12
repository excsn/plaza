//! Turning a destination into a route, the same way on both ends.
//!
//! This is the rule that makes a click cheap. The client sends one square and
//! then expands it here, immediately, and the server expands the same square
//! with the same code over the same derived map and gets the same answer. There
//! is no path on the wire, no correction coming back, and nothing to
//! reconcile.
//!
//! **Which puts the entire determinism surface in the tie-break.** Two routes
//! of equal length are equally correct and only one of them is the one the
//! server picked, so anything that leaves the choice between them to an
//! implementation detail is a divergence waiting for the first symmetric
//! stretch of grass. Three things make that impossible here rather than
//! unlikely:
//!
//! - The open set is ordered on `(f, h, seq)` where `seq` counts pushes, so
//!   ties fall to whichever was reached first and never to heap internals.
//! - Every table is a dense array indexed by square. There is no hash map in
//!   the search, so there is no iteration order to depend on.
//! - Neighbours are visited in one fixed order, cardinals before diagonals,
//!   which is also what makes a route look like something a person would walk.
//!
//! The budget matters for the same reason. A search that gives up returns the
//! best partial route rather than nothing, and *which* partial is decided by
//! the same total order, so giving up is as reproducible as succeeding.

use std::collections::BinaryHeap;

use crate::protocol::Tile;
use crate::world::{self, SIZE};

/// Squares a search may settle before it gives up and walks as far as it got.
///
/// A click across the whole map is a request to search thirty thousand squares
/// inside a tick that also has a world in it. Partial routes are what a player
/// experiences as walking toward somewhere far away, which is what they asked
/// for anyway.
pub const MAX_VISITED: usize = 4500;

/// Squares in one of the eight directions, cardinals first.
///
/// The order is part of the protocol in everything but name: it decides which
/// of several equal routes both ends pick, so changing it changes the answer on
/// whichever end changed first.
const STEPS: [(i16, i16); 8] = [
  (0, -1),
  (1, 0),
  (0, 1),
  (-1, 0),
  (1, -1),
  (1, 1),
  (-1, 1),
  (-1, -1),
];

const CELLS: usize = SIZE as usize * SIZE as usize;

fn index_of(tile: Tile) -> usize {
  tile.y as usize * SIZE as usize + tile.x as usize
}

fn tile_of(index: usize) -> Tile {
  Tile::new((index % SIZE as usize) as i16, (index / SIZE as usize) as i16)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Node {
  f: u32,
  h: u32,
  seq: u32,
  index: u32,
}

impl Ord for Node {
  /// Reversed, so `BinaryHeap` pops the smallest. Total on `(f, h, seq)` with
  /// `seq` unique, so two nodes are never equal and the heap has nothing left
  /// to decide for itself.
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    other
      .f
      .cmp(&self.f)
      .then(other.h.cmp(&self.h))
      .then(other.seq.cmp(&self.seq))
  }
}

impl PartialOrd for Node {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(other))
  }
}

/// What a caller is aiming at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
  /// Stand on that square.
  On(Tile),
  /// Stand next to it, which is what clicking a tree means.
  Beside(Tile),
}

impl Goal {
  pub fn tile(self) -> Tile {
    match self {
      Goal::On(tile) | Goal::Beside(tile) => tile,
    }
  }

  pub fn reached(self, at: Tile) -> bool {
    match self {
      Goal::On(tile) => at == tile,
      Goal::Beside(tile) => at.is_beside(tile),
    }
  }

  pub fn estimate(self, from: Tile) -> u32 {
    let steps = from.steps_to(self.tile());
    match self {
      Goal::On(_) => steps as u32,
      Goal::Beside(_) => (steps - 1).max(0) as u32,
    }
  }
}

/// Reusable scratch for the search.
///
/// Kept rather than allocated per call because both ends run one of these on
/// every click, and half a megabyte of dense tables is cheaper to hold than to
/// rebuild. The generation stamp is what removes the clear: a cell is stale
/// unless it was written this search.
pub struct Pathfinder {
  came: Vec<u32>,
  cost: Vec<u32>,
  stamp: Vec<u32>,
  generation: u32,
  open: BinaryHeap<Node>,
  /// Squares settled by the last search, which is the number worth reporting
  /// when a tick runs long.
  pub visited: usize,
}

impl Default for Pathfinder {
  fn default() -> Self {
    Self::new()
  }
}

impl Pathfinder {
  pub fn new() -> Self {
    Self {
      came: vec![0; CELLS],
      cost: vec![0; CELLS],
      stamp: vec![0; CELLS],
      generation: 0,
      open: BinaryHeap::with_capacity(1024),
      visited: 0,
    }
  }

  /// The route from one square to a goal, not including the square started on.
  ///
  /// Empty means already there. A route that does not end on the goal is a
  /// partial one: the search ran out of budget or the goal is walled off, and
  /// walking toward it is the honest answer to a click nobody can honour.
  pub fn route(&mut self, from: Tile, goal: Goal) -> Vec<Tile> {
    self.route_with(from, goal, &world::walkable)
  }

  pub fn route_with(&mut self, from: Tile, goal: Goal, walkable: &dyn Fn(Tile) -> bool) -> Vec<Tile> {
    self.visited = 0;
    if !world::in_bounds(from) || !world::in_bounds(goal.tile()) {
      return Vec::new();
    }
    if goal.reached(from) {
      return Vec::new();
    }

    self.generation = self.generation.wrapping_add(1);
    if self.generation == 0 {
      // Wrapping past zero would make every stale cell look fresh, so the one
      // time it happens the tables are cleared for real.
      self.stamp.iter_mut().for_each(|s| *s = 0);
      self.generation = 1;
    }
    let generation = self.generation;
    self.open.clear();

    let start = index_of(from);
    self.stamp[start] = generation;
    self.came[start] = start as u32;
    self.cost[start] = 0;

    let mut seq = 0u32;
    let first_h = goal.estimate(from);
    self.open.push(Node {
      f: first_h,
      h: first_h,
      seq,
      index: start as u32,
    });

    let mut closest = (first_h, start);
    let mut found: Option<usize> = None;

    while let Some(node) = self.open.pop() {
      let index = node.index as usize;
      // A square can be pushed more than once, so the stale copies are dropped
      // on the way out rather than removed on the way in.
      if node.f != self.cost[index] + node.h {
        continue;
      }
      let at = tile_of(index);
      if goal.reached(at) {
        found = Some(index);
        break;
      }
      self.visited += 1;
      if self.visited >= MAX_VISITED {
        break;
      }

      let here = self.cost[index];
      for (dx, dy) in STEPS {
        let next = Tile::new(at.x + dx, at.y + dy);
        if !world::in_bounds(next) || !walkable(next) {
          continue;
        }
        // A diagonal needs both of its sides open, or a body cuts the corner
        // of a cliff and walks through a wall a square thick.
        if dx != 0
          && dy != 0
          && (!walkable(Tile::new(at.x + dx, at.y)) || !walkable(Tile::new(at.x, at.y + dy)))
        {
          continue;
        }
        let next_index = index_of(next);
        let step_cost = here + 1;
        let fresh = self.stamp[next_index] != generation;
        if !fresh && self.cost[next_index] <= step_cost {
          continue;
        }
        self.stamp[next_index] = generation;
        self.cost[next_index] = step_cost;
        self.came[next_index] = index as u32;
        let h = goal.estimate(next);
        if h < closest.0 {
          closest = (h, next_index);
        }
        seq += 1;
        self.open.push(Node {
          f: step_cost + h,
          h,
          seq,
          index: next_index as u32,
        });
      }
    }

    let end = found.unwrap_or(closest.1);
    if end == start {
      return Vec::new();
    }
    self.walk_back(start, end, generation)
  }

  fn walk_back(&self, start: usize, end: usize, generation: u32) -> Vec<Tile> {
    let mut route = Vec::new();
    let mut index = end;
    while index != start {
      route.push(tile_of(index));
      if self.stamp[index] != generation {
        return Vec::new();
      }
      let parent = self.came[index] as usize;
      if parent == index {
        break;
      }
      index = parent;
    }
    route.reverse();
    route
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn open_field(_: Tile) -> bool {
    true
  }

  #[test]
  fn a_route_over_open_ground_is_as_long_as_the_distance() {
    // Eight-way movement at one cost a step means the shortest route is the
    // Chebyshev distance, and anything longer is the search wandering.
    let mut finder = Pathfinder::new();
    for (from, to) in [
      (Tile::new(10, 10), Tile::new(20, 10)),
      (Tile::new(10, 10), Tile::new(20, 20)),
      (Tile::new(40, 12), Tile::new(18, 31)),
    ] {
      let route = finder.route_with(from, Goal::On(to), &open_field);
      assert_eq!(route.len() as i32, from.steps_to(to), "from {from:?} to {to:?}");
      assert_eq!(route.last().copied(), Some(to));
    }
  }

  #[test]
  fn the_same_click_is_the_same_route_every_time() {
    // The property the whole design rests on: the client draws this before the
    // server has heard the question, so the two had better agree.
    let mut a = Pathfinder::new();
    let mut b = Pathfinder::new();
    for i in 0..400i16 {
      let from = world::footing_near(Tile::new((i * 7) % SIZE, (i * 11) % SIZE));
      let to = world::footing_near(Tile::new((i * 23) % SIZE, (i * 5) % SIZE));
      let first = a.route(from, Goal::On(to));
      let second = b.route(from, Goal::On(to));
      assert_eq!(first, second, "two searches disagreed from {from:?} to {to:?}");
    }
  }

  #[test]
  fn a_reused_pathfinder_answers_the_same_as_a_fresh_one() {
    // The generation stamp is an optimisation, and an optimisation that leaks
    // state between searches would diverge the two ends after the first click.
    let mut reused = Pathfinder::new();
    let pairs: Vec<(Tile, Tile)> = (0..60i16)
      .map(|i| {
        (
          world::footing_near(Tile::new((i * 13) % SIZE, (i * 3) % SIZE)),
          world::footing_near(Tile::new((i * 29) % SIZE, (i * 17) % SIZE)),
        )
      })
      .collect();
    for _ in 0..3 {
      for (from, to) in &pairs {
        let mut fresh = Pathfinder::new();
        assert_eq!(
          reused.route(*from, Goal::On(*to)),
          fresh.route(*from, Goal::On(*to)),
          "a reused search drifted from {from:?} to {to:?}"
        );
      }
    }
  }

  #[test]
  fn the_tie_break_is_pinned_rather_than_incidental() {
    // On open ground every route of the same length is equally correct, so
    // this asserts *which* one, on purpose. If the neighbour order or the open
    // set's ordering changes, this fails, which is the point: both are part of
    // the protocol in everything but name.
    let mut finder = Pathfinder::new();
    let route = finder.route_with(Tile::new(5, 5), Goal::On(Tile::new(9, 7)), &open_field);
    assert_eq!(
      route,
      vec![
        Tile::new(6, 5),
        Tile::new(7, 5),
        Tile::new(8, 6),
        Tile::new(9, 7),
      ],
      "the tie-break moved"
    );
  }

  #[test]
  fn a_diagonal_does_not_cut_a_corner() {
    // Without the rule a body walks through the join of two walls, which is
    // the one pathfinding bug a player notices immediately and cannot unsee.
    let wall = |tile: Tile| !(tile.x == 6 && tile.y <= 5) && !(tile.y == 6 && tile.x <= 5);
    let mut finder = Pathfinder::new();
    let route = finder.route_with(Tile::new(5, 5), Goal::On(Tile::new(7, 7)), &wall);
    assert!(
      !route.contains(&Tile::new(6, 6)) || route.is_empty(),
      "the route cut the corner: {route:?}"
    );
    for pair in route.windows(2) {
      let (a, b) = (pair[0], pair[1]);
      let (dx, dy) = (b.x - a.x, b.y - a.y);
      if dx != 0 && dy != 0 {
        assert!(
          wall(Tile::new(a.x + dx, a.y)) && wall(Tile::new(a.x, a.y + dy)),
          "a diagonal from {a:?} to {b:?} passed through a wall"
        );
      }
    }
  }

  #[test]
  fn clicking_a_tree_walks_you_next_to_it_rather_than_into_it() {
    let mut finder = Pathfinder::new();
    let tree = (0..SIZE)
      .flat_map(|y| (0..SIZE).map(move |x| Tile::new(x, y)))
      .find(|t| world::prop_at(*t) == Some(crate::world::Prop::Tree))
      .expect("a world with no trees in it");
    let from = world::footing_near(Tile::new(tree.x + 12, tree.y + 9));
    let route = finder.route(from, Goal::Beside(tree));
    assert!(!route.is_empty(), "no route to a tree twelve squares away");
    let arrived = *route.last().unwrap();
    assert!(arrived.is_beside(tree), "stopped at {arrived:?} rather than beside {tree:?}");
    assert!(world::walkable(arrived));
  }

  #[test]
  fn every_square_of_a_route_can_be_stood_on() {
    let mut finder = Pathfinder::new();
    for i in 0..80i16 {
      let from = world::footing_near(Tile::new((i * 19) % SIZE, (i * 7) % SIZE));
      let to = world::footing_near(Tile::new((i * 31) % SIZE, (i * 23) % SIZE));
      for square in finder.route(from, Goal::On(to)) {
        assert!(world::walkable(square), "the route went through {square:?}");
      }
    }
  }

  #[test]
  fn a_walled_off_goal_walks_as_close_as_it_can_rather_than_refusing() {
    // A click nobody can honour still means something, and standing still is a
    // worse answer than setting off toward it.
    let island = |tile: Tile| tile.x < 20 || tile.x > 24;
    let mut finder = Pathfinder::new();
    let route = finder.route_with(Tile::new(5, 5), Goal::On(Tile::new(60, 5)), &island);
    assert!(!route.is_empty(), "gave up entirely");
    assert!(route.last().unwrap().x < 20, "walked through the wall: {:?}", route.last());
    assert!(route.len() > 10, "barely moved: {}", route.len());
  }

  #[test]
  fn a_search_across_the_whole_map_stays_inside_its_budget() {
    // A tick with a world in it cannot also hold an unbounded search, and the
    // partial route this produces is what a player reads as setting off.
    let mut finder = Pathfinder::new();
    let blocked = |tile: Tile| tile.x != 90;
    let route = finder.route_with(Tile::new(2, 2), Goal::On(Tile::new(189, 189)), &blocked);
    assert!(finder.visited <= MAX_VISITED, "settled {} squares", finder.visited);
    assert!(!route.is_empty());
  }

  #[test]
  fn standing_on_the_goal_is_no_route_at_all() {
    let mut finder = Pathfinder::new();
    assert!(finder.route(Tile::new(30, 30), Goal::On(Tile::new(30, 30))).is_empty());
    assert!(finder
      .route_with(Tile::new(30, 30), Goal::Beside(Tile::new(31, 30)), &open_field)
      .is_empty());
  }

  #[test]
  fn what_a_click_costs_against_what_a_held_key_costs() {
    // The headline arithmetic, stated where it can go stale loudly. gow_3d
    // sends a direction thirty times a second; this sends one square per
    // journey, and a journey is however long the route is.
    let mut finder = Pathfinder::new();
    let mut squares = 0usize;
    let journeys = 200;
    for i in 0..journeys as i16 {
      let from = world::footing_near(Tile::new((i * 7) % SIZE, (i * 13) % SIZE));
      let to = world::footing_near(Tile::new((i * 17) % SIZE, (i * 29) % SIZE));
      squares += finder.route(from, Goal::On(to)).len();
    }
    let per_journey = squares as f32 / journeys as f32;
    let seconds = per_journey * crate::protocol::TICK_MS as f32 / 1000.0;
    println!("\n  {per_journey:.0} squares a journey, {seconds:.1}s of walking for one op");
    println!("  a held direction at 30Hz would have sent {:.0} in the same time\n", seconds * 30.0);
    assert!(per_journey > 20.0, "journeys are too short to make the point: {per_journey}");
  }
}
