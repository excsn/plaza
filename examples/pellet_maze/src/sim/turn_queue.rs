//! An input whose execution point is a **place**, not a time.
//!
//! This is the file the example exists for.
//!
//! # The problem tick-addressing does not solve
//!
//! Every other playground here keys an input to a tick: the client says which
//! tick the input is meant for, the server runs it on that tick or refuses it,
//! and both sides therefore run the same input at the same moment. That answers
//! *when*.
//!
//! A queued turn does not have a when. "Left" pressed halfway down a corridor
//! is not a request to turn left now, because there is no left to turn into. It
//! is a request to turn left **at the next place where that is possible**, and
//! which place that is depends on where the player is, which is exactly the
//! thing the two sides can disagree about.
//!
//! So the two sides can agree perfectly about the tick and still take the turn
//! at different intersections. And unlike a mispredicted cell, which is one cell
//! wrong and then corrected, a turn taken at the wrong junction sends the player
//! down a **different corridor**: the error compounds instead of settling, and a
//! correction arriving a moment later has to undo a route rather than a step.
//!
//! # Why a place-trigger still needs a time bound
//!
//! The obvious implementation, "hold the turn until it becomes legal", is
//! wrong, and wrong in a way players feel rather than see. A turn held
//! indefinitely fires at the next junction *however far away it is*, so a press
//! from two seconds and four corners ago takes a corner nobody meant. The
//! buffer exists so that pressing slightly **early** into a corner works; it is
//! not a promise to remember forever.
//!
//! So a queued turn carries the tick it was asked for and expires. That makes it
//! a hybrid: a *place* decides where it fires and a *time* decides whether it
//! still may. Both bounds are needed and they fail differently, which is why
//! [`TurnQueue`] counts the two outcomes separately.

use serde::{Deserialize, Serialize};

use crate::sim::types::{Cell, Dir, Maze, SIM_STEP_MS};

/// A turn asked for and not yet taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTurn {
  pub dir: Dir,
  /// The tick it was asked for, which is what the expiry is measured from.
  pub asked_tick: u64,
}

/// What happened to a queued turn when a player arrived somewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
  /// Nothing was queued.
  Idle,
  /// Still waiting: the turn is live but this cell does not offer it.
  Held,
  /// Taken here.
  Taken { dir: Dir, at: Cell },
  /// Dropped: it was still waiting when its time ran out.
  Expired { dir: Dir },
}

/// One player's pending turn, and the counters that judge the buffer.
///
/// Deliberately not a queue of many: a player can only mean one thing at a
/// time, and holding a backlog of turns produces exactly the "took a corner I
/// pressed four corners ago" behaviour the expiry exists to prevent. A new
/// press replaces the old one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnQueue {
  pending: Option<QueuedTurn>,
  taken: u64,
  expired: u64,
  /// Where the last turn was actually taken, so two sides can be compared.
  last_taken_at: Option<Cell>,
}

impl TurnQueue {
  pub fn new() -> Self {
    Self::default()
  }

  /// Asks for a turn, replacing whatever was pending.
  pub fn request(&mut self, dir: Dir, tick: u64) {
    self.pending = Some(QueuedTurn { dir, asked_tick: tick });
  }

  pub fn pending(&self) -> Option<QueuedTurn> {
    self.pending
  }

  pub fn clear(&mut self) {
    self.pending = None;
  }

  /// Turns taken, and turns that ran out of time waiting for a place.
  ///
  /// Two counters rather than one because they say opposite things about the
  /// buffer: a high expiry rate means it is too short for the maze (or the
  /// player is pressing into walls), and a buffer long enough to expire rarely
  /// is long enough to take corners nobody meant.
  pub fn stats(&self) -> (u64, u64) {
    (self.taken, self.expired)
  }

  pub fn last_taken_at(&self) -> Option<Cell> {
    self.last_taken_at
  }

  /// Decides what a player arriving at `cell` does with their pending turn.
  ///
  /// Called **at a cell boundary and nowhere else**, which is what makes this a
  /// place trigger. `heading` is what they are doing now, and is returned
  /// unchanged when nothing is taken.
  ///
  /// A reversal is deliberately allowed anywhere, not only at a junction:
  /// turning back the way you came is legal in any corridor and is the one
  /// escape a cornered runner has.
  pub fn resolve(&mut self, cell: Cell, heading: Dir, maze: &Maze, tick: u64, buffer_ms: u64) -> (Dir, Resolution) {
    let Some(turn) = self.pending else {
      return (heading, Resolution::Idle);
    };

    // Taken first, expiry second. A turn that becomes legal on the very tick it
    // would otherwise expire should be taken: the player pressed in time and
    // arrived in time, and refusing it there would be the buffer punishing them
    // for a rounding error.
    if cell.step(turn.dir).is_some_and(|next| maze.open(next)) {
      self.pending = None;
      self.taken += 1;
      self.last_taken_at = Some(cell);
      return (turn.dir, Resolution::Taken { dir: turn.dir, at: cell });
    }

    let age_ms = tick.saturating_sub(turn.asked_tick) * SIM_STEP_MS;
    if age_ms > buffer_ms {
      self.pending = None;
      self.expired += 1;
      return (heading, Resolution::Expired { dir: turn.dir });
    }

    (heading, Resolution::Held)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{Tile, MAZE_SEED};

  /// A straight corridor along `y = 1`, with a side passage at `x = 5`.
  fn corridor() -> Maze {
    let mut maze = Maze::default();
    for x in 1..10u8 {
      maze.set(Cell::new(x, 1), Tile::Corridor);
    }
    maze.set(Cell::new(5, 2), Tile::Corridor);
    maze
  }

  #[test]
  fn a_turn_waits_for_a_place_rather_than_a_time() {
    let maze = corridor();
    let mut queue = TurnQueue::new();
    queue.request(Dir::Down, 0);

    for x in 1..5u8 {
      let (heading, outcome) = queue.resolve(Cell::new(x, 1), Dir::Right, &maze, 0, 10_000);
      assert_eq!(heading, Dir::Right, "still running right at x={x}");
      assert_eq!(outcome, Resolution::Held);
    }
    let (heading, outcome) = queue.resolve(Cell::new(5, 1), Dir::Right, &maze, 0, 10_000);
    assert_eq!(heading, Dir::Down, "and taken where it becomes possible");
    assert_eq!(outcome, Resolution::Taken { dir: Dir::Down, at: Cell::new(5, 1) });
    assert_eq!(queue.stats(), (1, 0));
  }

  #[test]
  fn a_turn_that_never_becomes_possible_expires() {
    // Without this a press from long ago fires at a corner nobody meant, which
    // is the failure a player describes as the controls having a mind of their
    // own.
    let maze = corridor();
    let mut queue = TurnQueue::new();
    queue.request(Dir::Up, 0);

    // Running right along a corridor with no way up.
    let buffer = 200;
    let mut tick = 0;
    let mut outcome = Resolution::Idle;
    for x in 1..9u8 {
      tick += 4; // Roughly one cell of running.
      let result = queue.resolve(Cell::new(x, 1), Dir::Right, &maze, tick, buffer);
      outcome = result.1;
      if matches!(outcome, Resolution::Expired { .. }) {
        break;
      }
    }
    assert_eq!(outcome, Resolution::Expired { dir: Dir::Up });
    assert_eq!(queue.stats(), (0, 1));
    assert_eq!(queue.pending(), None, "and it is forgotten rather than retried");
  }

  #[test]
  fn a_turn_is_taken_on_the_tick_it_would_otherwise_expire() {
    let maze = corridor();
    let mut queue = TurnQueue::new();
    queue.request(Dir::Down, 0);
    let buffer = 100;
    let exactly = buffer / SIM_STEP_MS;
    let (heading, outcome) = queue.resolve(Cell::new(5, 1), Dir::Right, &maze, exactly, buffer);
    assert_eq!(heading, Dir::Down);
    assert!(matches!(outcome, Resolution::Taken { .. }));
  }

  #[test]
  fn a_new_press_replaces_the_old_one() {
    // A backlog of turns is what produces "it took a corner I pressed four
    // corners ago". A player can only mean one thing at a time.
    let maze = corridor();
    let mut queue = TurnQueue::new();
    queue.request(Dir::Up, 0);
    queue.request(Dir::Down, 1);
    assert_eq!(queue.pending().map(|t| t.dir), Some(Dir::Down));

    let (heading, _) = queue.resolve(Cell::new(5, 1), Dir::Right, &maze, 2, 10_000);
    assert_eq!(heading, Dir::Down, "the newest press is the one that means anything");
    assert_eq!(queue.stats(), (1, 0), "and the replaced one is not counted as expired");
  }

  #[test]
  fn reversing_is_legal_anywhere_a_corridor_goes_back() {
    let maze = corridor();
    let mut queue = TurnQueue::new();
    queue.request(Dir::Left, 0);
    let (heading, outcome) = queue.resolve(Cell::new(7, 1), Dir::Right, &maze, 0, 10_000);
    assert_eq!(heading, Dir::Left);
    assert!(matches!(outcome, Resolution::Taken { .. }));
  }

  #[test]
  fn nothing_queued_leaves_the_heading_alone() {
    let maze = corridor();
    let mut queue = TurnQueue::new();
    let (heading, outcome) = queue.resolve(Cell::new(5, 1), Dir::Right, &maze, 0, 200);
    assert_eq!(heading, Dir::Right);
    assert_eq!(outcome, Resolution::Idle);
    assert_eq!(queue.stats(), (0, 0));
  }

  #[test]
  fn where_a_turn_was_taken_is_recorded_so_two_sides_can_be_compared() {
    let maze = Maze::generate(MAZE_SEED);
    let mut queue = TurnQueue::new();
    assert_eq!(queue.last_taken_at(), None);

    let junction = maze.corridors().into_iter().find(|c| maze.is_junction(*c)).expect("a junction");
    let exit = maze.exits(junction)[0];
    queue.request(exit, 0);
    queue.resolve(junction, exit.opposite(), &maze, 0, 10_000);
    assert_eq!(queue.last_taken_at(), Some(junction));
  }
}
