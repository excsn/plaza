//! The rules both sides run, as one piece of code.
//!
//! The same discipline as `bomb_grid`'s `rules.rs`, and for the same reason:
//! a rule written twice diverges, and on a lattice it diverges into different
//! cells rather than into a few pixels. What is new here is that the movement
//! rule **consumes a place-triggered input** ([`crate::sim::turn_queue`]), so
//! the queue has to be threaded through it rather than applied before it. A
//! turn is only ever resolved at a cell boundary, and the boundary is inside
//! this function.

use crate::sim::turn_queue::{Resolution, TurnQueue};
use crate::sim::types::{Cell, Dir, Maze, PlayerState, Step, SIM_STEP_MS};

/// Advances one player by `dt_ms`, resolving queued turns at every cell it
/// reaches.
///
/// Returns what happened to the queue, in order, so a caller can count taken
/// and expired turns and a client can compare *where* a turn was taken against
/// where the server took it.
///
/// **Leftover time carries into the next step.** Without the carry a run of N
/// cells loses up to N ticks, which is invisible over one cell and is a whole
/// cell over a corridor, and it is exactly the drift that puts a client and a
/// server at different junctions.
pub fn advance_player(
  player: &mut PlayerState,
  queue: &mut TurnQueue,
  maze: &Maze,
  tick: u64,
  buffer_ms: u64,
  dt_ms: u64,
) -> Vec<Resolution> {
  let mut outcomes = Vec::new();
  if !player.alive {
    player.step = None;
    return outcomes;
  }

  let mut remaining = dt_ms;
  loop {
    if player.step.is_none() {
      // At a cell, which is the only place a queued turn may be taken.
      let (heading, outcome) = queue.resolve(player.cell, player.heading, maze, tick, buffer_ms);
      player.heading = heading;
      outcomes.push(outcome);

      let Some(next) = player.cell.step(heading).filter(|c| maze.open(*c)) else {
        // Facing a wall: stand still and spend the rest of the tick doing
        // nothing. The next tick asks the queue again, so a turn pressed while
        // stuck still frees the player.
        return outcomes;
      };
      // The step this player is taking *now*: an eaten pursuer hurries home
      // rather than trudging back at hunting speed.
      let duration = player.current_step_ms(tick * SIM_STEP_MS).min(u16::MAX as u64) as u16;
      player.step = Some(Step {
        from: player.cell,
        to: next,
        progress_ms: 0,
        duration_ms: duration,
      });
    }

    let Some(step) = player.step.as_mut() else {
      return outcomes;
    };
    let left = step.duration_ms.saturating_sub(step.progress_ms) as u64;
    if remaining < left {
      step.progress_ms = step.progress_ms.saturating_add(remaining as u16);
      return outcomes;
    }
    remaining -= left;
    let arrived = step.to;
    player.step = None;
    player.cell = arrived;
    if remaining == 0 {
      return outcomes;
    }
  }
}

/// Which way a pursuer goes from `at`, heading `heading`, hunting `target`.
///
/// Deterministic and shared, so a client can run the pursuers itself and only
/// be corrected, rather than waiting to be told where four other things are.
/// The same trick horde uses for its enemies, on rails.
///
/// **Never reverses unless there is no choice.** A pursuer that may turn back
/// at any moment oscillates in a corridor and is both trivial to escape and
/// unpleasant to watch, so reversing is reserved for a dead end.
pub fn pursuit_dir(at: Cell, heading: Dir, target: Cell, maze: &Maze) -> Dir {
  let mut options: Vec<Dir> = maze.exits(at).into_iter().filter(|d| *d != heading.opposite()).collect();
  if options.is_empty() {
    // A dead end. Turning back is the only move there is.
    options = maze.exits(at);
  }
  options
    .into_iter()
    .min_by_key(|d| {
      let next = at.step(*d).unwrap_or(at);
      // Distance first, then a fixed direction order, so two pursuers in the
      // same cell make the same choice on both sides rather than whichever the
      // iterator happened to yield first.
      (next.distance(target), u8::from(*d))
    })
    .unwrap_or(heading)
}

/// Which way to go from `at` to reach the nearest pellet.
///
/// A breadth-first sweep of the maze rather than "walk toward the closest one
/// as the crow flies": a pellet two cells away through a wall is further than
/// one six cells away down a corridor, and a bot steering by straight-line
/// distance walks into walls and looks broken. The maze is a few hundred cells,
/// so the sweep is cheap enough to run at every junction.
///
/// `avoid` is a pursuer to route around when there is a choice. It is a
/// tie-break rather than a hard constraint: refusing every route that passes
/// near a pursuer leaves a cornered runner with no route at all.
///
/// Deterministic in its tie-breaks, like every other shared rule here, so two
/// sides running it reach the same answer rather than whichever the iterator
/// happened to yield first.
pub fn pellet_dir(at: Cell, heading: Dir, pellets: &[Cell], maze: &Maze, avoid: Option<Cell>) -> Option<Dir> {
  if pellets.is_empty() {
    return None;
  }
  // The first move of the shortest path to each reachable cell.
  let mut first: std::collections::HashMap<Cell, Dir> = std::collections::HashMap::new();
  let mut queue: std::collections::VecDeque<Cell> = std::collections::VecDeque::new();

  // Seeded in a fixed direction order, so ties between equal-length paths
  // resolve the same way every time.
  //
  // **Reversing is excluded unless it is the only way out**, exactly as the
  // pursuit rule excludes it, and for a sharper reason. A runner eats the cell
  // it stands on, so the nearest remaining pellet is very often the one just
  // behind it: seeded naively, the rule turns the runner round, it eats the
  // next cell back, and the nearest pellet is behind it again. That is a bot
  // that paces one corridor for a whole round, covering three hundred cells to
  // eat fifty.
  let mut exits: Vec<Dir> = maze.exits(at).into_iter().filter(|d| *d != heading.opposite()).collect();
  if exits.is_empty() {
    exits = maze.exits(at);
  }
  exits.sort_by_key(|d| (*d != heading, u8::from(*d)));
  for dir in exits {
    if let Some(next) = at.step(dir).filter(|c| maze.open(*c)) {
      first.entry(next).or_insert(dir);
      queue.push_back(next);
    }
  }

  let mut best: Option<(u16, Dir)> = None;
  let mut depth = 0u16;
  while let Some(cell) = queue.pop_front() {
    depth += 1;
    if pellets.contains(&cell) {
      let dir = first[&cell];
      // Among equally close pellets, prefer the route that does not run at the
      // pursuer.
      let penalty = avoid.map_or(0, |threat| u16::from(cell.distance(threat) <= 2));
      let score = depth + penalty * 4;
      if best.is_none_or(|(b, _)| score < b) {
        best = Some((score, dir));
      }
      if penalty == 0 {
        return Some(dir);
      }
    }
    let mut onward = maze.exits(cell);
    onward.sort_by_key(|d| u8::from(*d));
    for dir in onward {
      if let Some(next) = cell.step(dir).filter(|c| maze.open(*c))
        && !first.contains_key(&next)
      {
        first.insert(next, first[&cell]);
        queue.push_back(next);
      }
    }
  }
  best.map(|(_, dir)| dir)
}

/// How many ticks a duration is, for turning the panel's millisecond sliders
/// into the unit the queue actually measures in.
pub const fn ticks_of(ms: u64) -> u64 {
  ms / SIM_STEP_MS
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{Role, Tile, MAZE_SEED};

  fn corridor_with_branch() -> Maze {
    let mut maze = Maze::default();
    for x in 1..10u8 {
      maze.set(Cell::new(x, 1), Tile::Corridor);
    }
    maze.set(Cell::new(5, 2), Tile::Corridor);
    maze.set(Cell::new(5, 3), Tile::Corridor);
    maze
  }

  fn runner(cell: Cell, heading: Dir) -> PlayerState {
    PlayerState::new(0, Role::Runner, cell, heading)
  }

  #[test]
  fn a_player_runs_without_being_told_to() {
    let maze = corridor_with_branch();
    let mut player = runner(Cell::new(1, 1), Dir::Right);
    let mut queue = TurnQueue::new();
    // Thirty ticks is 480 ms, which at 150 ms a cell is three whole cells and
    // a bit: the committed cell is three along, with a fourth step in flight.
    for _ in 0..30 {
      advance_player(&mut player, &mut queue, &maze, 0, 200, SIM_STEP_MS);
    }
    assert_eq!(player.cell, Cell::new(4, 1));
    assert!(player.step.is_some(), "and still running, because there is no stopping");
  }

  #[test]
  fn a_queued_turn_is_taken_at_the_branch_and_nowhere_else() {
    let maze = corridor_with_branch();
    let mut player = runner(Cell::new(1, 1), Dir::Right);
    let mut queue = TurnQueue::new();
    queue.request(Dir::Down, 0);

    let mut taken_at = None;
    for tick in 0..200u64 {
      for outcome in advance_player(&mut player, &mut queue, &maze, tick, 10_000, SIM_STEP_MS) {
        if let Resolution::Taken { at, .. } = outcome {
          taken_at = Some(at);
        }
      }
      if taken_at.is_some() {
        break;
      }
    }
    assert_eq!(taken_at, Some(Cell::new(5, 1)), "the one cell that offers it");
    assert_eq!(player.heading, Dir::Down);
  }

  #[test]
  fn facing_a_wall_stands_still_and_a_turn_frees_it() {
    let maze = corridor_with_branch();
    let mut player = runner(Cell::new(9, 1), Dir::Right);
    let mut queue = TurnQueue::new();

    for tick in 0..20u64 {
      advance_player(&mut player, &mut queue, &maze, tick, 200, SIM_STEP_MS);
    }
    assert_eq!(player.cell, Cell::new(9, 1), "stopped at the wall");
    assert!(player.step.is_none());

    queue.request(Dir::Left, 20);
    advance_player(&mut player, &mut queue, &maze, 20, 200, SIM_STEP_MS);
    assert_eq!(player.heading, Dir::Left, "and freed by a press");
    assert!(player.step.is_some());
  }

  #[test]
  fn leftover_time_carries_into_the_next_cell() {
    // The drift that puts a client and a server at different junctions. Half a
    // tick lost per cell is invisible once and a whole cell over a corridor.
    let maze = corridor_with_branch();
    let mut fine = runner(Cell::new(1, 1), Dir::Right);
    let mut coarse = runner(Cell::new(1, 1), Dir::Right);
    let (mut a, mut b) = (TurnQueue::new(), TurnQueue::new());

    for _ in 0..30 {
      advance_player(&mut fine, &mut a, &maze, 0, 200, 16);
    }
    for _ in 0..10 {
      advance_player(&mut coarse, &mut b, &maze, 0, 200, 48);
    }
    assert_eq!(fine.cell, coarse.cell, "the same total time is the same cell");
  }

  #[test]
  fn a_pursuer_closes_on_its_target() {
    let maze = Maze::generate(MAZE_SEED);
    let corridors = maze.corridors();
    let target = corridors[corridors.len() - 1];
    let mut at = corridors[0];
    let mut heading = maze.exits(at)[0];

    let before = at.distance(target);
    for _ in 0..200 {
      heading = pursuit_dir(at, heading, target, &maze);
      at = at.step(heading).unwrap_or(at);
      if at == target {
        break;
      }
    }
    assert!(at.distance(target) < before, "it got closer: {} then {}", before, at.distance(target));
  }

  #[test]
  fn a_pursuer_does_not_reverse_in_a_corridor() {
    // A pursuer that may turn back at any moment oscillates, which is both
    // trivial to escape and unpleasant to watch.
    let maze = corridor_with_branch();
    // Running right, with the target behind: the tempting move is a reversal.
    let dir = pursuit_dir(Cell::new(7, 1), Dir::Right, Cell::new(1, 1), &maze);
    assert_ne!(dir, Dir::Left, "it commits to the corridor rather than flip-flopping");
  }

  #[test]
  fn a_pursuer_reverses_at_a_dead_end() {
    let maze = corridor_with_branch();
    let dir = pursuit_dir(Cell::new(5, 3), Dir::Down, Cell::new(1, 1), &maze);
    assert_eq!(dir, Dir::Up, "there is nothing else to do");
  }

  #[test]
  fn two_pursuers_in_one_cell_choose_identically() {
    // Determinism, which is what lets a client run them locally. An iterator's
    // order is not a tie-break anyone can reproduce.
    let maze = Maze::generate(MAZE_SEED);
    let junction = maze.corridors().into_iter().find(|c| maze.is_junction(*c)).expect("a junction");
    let target = Cell::new(1, 1);
    let first = pursuit_dir(junction, Dir::Right, target, &maze);
    let second = pursuit_dir(junction, Dir::Right, target, &maze);
    assert_eq!(first, second);
  }

  #[test]
  fn a_pellet_route_follows_corridors_rather_than_straight_lines() {
    // A pellet two cells away through a wall is further than one six cells away
    // down a corridor. A bot steering by straight-line distance walks into
    // walls and looks broken.
    let mut maze = Maze::default();
    for x in 1..9u8 {
      maze.set(Cell::new(x, 1), Tile::Corridor);
    }
    // A pellet just above the start, behind a wall: closest as the crow flies,
    // unreachable in one move.
    let dir = pellet_dir(Cell::new(4, 1), Dir::Right, &[Cell::new(4, 0), Cell::new(7, 1)], &maze, None);
    assert!(matches!(dir, Some(Dir::Right)), "it goes the way the corridor goes: {dir:?}");
  }

  #[test]
  fn with_no_pellets_there_is_no_route() {
    let maze = Maze::generate(MAZE_SEED);
    assert_eq!(pellet_dir(Cell::new(1, 1), Dir::Right, &[], &maze, None), None);
  }

  #[test]
  fn a_route_prefers_a_pellet_away_from_the_pursuer() {
    // A tie-break rather than a hard constraint: refusing every route that
    // passes near a pursuer leaves a cornered runner with nowhere to go.
    let mut maze = Maze::default();
    for x in 1..10u8 {
      maze.set(Cell::new(x, 1), Tile::Corridor);
    }
    // Arriving from above, so **neither** candidate is a reversal: the rule
    // refuses to turn back, and a test offering only forward and backward would
    // be asserting about that rule instead of about this one.
    maze.set(Cell::new(5, 0), Tile::Corridor);
    let at = Cell::new(5, 1);
    let dir = pellet_dir(at, Dir::Down, &[Cell::new(3, 1), Cell::new(7, 1)], &maze, Some(Cell::new(7, 1)));
    assert_eq!(dir, Some(Dir::Left), "it eats away from the threat when the choice is free");
  }
}
