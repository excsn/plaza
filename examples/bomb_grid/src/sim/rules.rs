//! The rules both sides run, as one piece of code.
//!
//! Everything in here is called by the authoritative server *and* by a client
//! predicting its own player. That is not a convenience: it is the strongest
//! correlation the playgrounds in this repository have found, and it is close to
//! a controlled experiment, because both the horde and black hole examples
//! contain entities whose rule is shared and entities whose rule was written
//! twice, and only the second kind produced divergence bugs.
//!
//! On a lattice the stakes are higher than in a continuous game. A continuous
//! rule written twice diverges by a few pixels a second and the correction eases
//! it away invisibly. A discrete rule written twice puts the two sides in
//! *different cells*, and there is no fraction of a cell to ease across: the
//! client snaps, and the player sees it.
//!
//! These are free functions over plain state rather than methods, so the client
//! can run them against its predicted copy without owning a `Server`.

use crate::sim::types::{BombState, Cell, Dir, Grid, PlayerState, Step};

/// Whether a player standing in `from` may walk into `to`.
///
/// A bomb blocks, **except the one under your feet**: walking off a bomb you
/// just dropped has to be possible, or dropping one is suicide with no
/// counterplay. That exception is the reason this takes `from` at all.
pub fn passable(grid: &Grid, bombs: &[BombState], from: Cell, to: Cell) -> bool {
  if !grid.walkable(to) {
    return false;
  }
  !bombs.iter().any(|b| b.cell == to && b.cell != from)
}

/// Advances one player by `dt_ms` under a held direction.
///
/// **Time left over from a completed step carries into the next one.** Without
/// that carry, a walk loses whatever fraction of a tick was left at each cell
/// boundary, so a player crossing N cells arrives up to N ticks late. It is
/// invisible in a single step and compounds over a corridor, and on a lattice
/// it compounds into a whole cell of disagreement between a client predicting
/// at its frame rate and a server stepping at its own: the two lose different
/// remainders. Carrying the remainder is what makes the rule frame-rate
/// independent, which is the property prediction actually rests on.
///
/// A step that cannot start (a wall, a bomb) simply does not, and the player
/// stands still: refusing is the whole of collision here, because a cell is
/// either enterable or it is not.
pub fn advance_player(player: &mut PlayerState, held: Dir, grid: &Grid, bombs: &[BombState], dt_ms: u64) {
  if !player.alive {
    player.step = None;
    return;
  }
  let mut remaining = dt_ms;
  loop {
    if player.step.is_none() {
      if held == Dir::None {
        return;
      }
      try_start_step(player, held, grid, bombs);
      // Still nothing: the way is blocked, so the rest of the tick is spent
      // standing still.
      if player.step.is_none() {
        return;
      }
    }
    let Some(step) = player.step.as_mut() else {
      return;
    };
    let left = step.duration_ms.saturating_sub(step.progress_ms) as u64;
    if remaining < left {
      step.progress_ms = step.progress_ms.saturating_add(remaining as u16);
      return;
    }
    remaining -= left;
    let arrived = step.to;
    player.step = None;
    player.cell = arrived;
    if remaining == 0 {
      return;
    }
  }
}

/// Begins a walk into the next cell, if there is one and it is enterable.
pub fn try_start_step(player: &mut PlayerState, dir: Dir, grid: &Grid, bombs: &[BombState]) {
  let from = player.cell;
  let Some(to) = from.step(dir) else {
    return;
  };
  if !passable(grid, bombs, from, to) {
    return;
  }
  let duration = player.step_ms().min(u16::MAX as u64) as u16;
  player.step = Some(Step {
    from,
    to,
    progress_ms: 0,
    duration_ms: duration,
  });
}

/// Whether this player may drop a bomb right now, and where it would land.
///
/// Shared for the same reason the movement is: a client that predicts a bomb
/// has to refuse exactly the bombs the server will refuse, or every refusal is
/// a phantom the player watches disappear.
pub fn bomb_placement(player: &PlayerState, bombs: &[BombState]) -> Option<Cell> {
  if !player.alive {
    return None;
  }
  let cell = player.occupied();
  let live = bombs.iter().filter(|b| b.owner == player.id).count();
  if live >= player.bombs_max as usize {
    return None;
  }
  if bombs.iter().any(|b| b.cell == cell) {
    return None;
  }
  Some(cell)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{PlayerId, Tile, STEP_MS_BASE};

  fn board() -> Grid {
    let mut grid = Grid::default();
    for x in 0..crate::sim::types::GRID_W {
      grid.set(Cell::new(x, 0), Tile::Hard);
    }
    grid
  }

  fn player(id: PlayerId, cell: Cell) -> PlayerState {
    PlayerState::new(id, cell)
  }

  #[test]
  fn a_held_direction_walks_cell_after_cell_without_pausing() {
    let grid = board();
    let mut p = player(0, Cell::new(1, 1));
    for _ in 0..(STEP_MS_BASE * 2 / 16) {
      advance_player(&mut p, Dir::Right, &grid, &[], 16);
    }
    assert_eq!(p.cell, Cell::new(3, 1), "two whole cells, no stall at the boundary");
  }

  #[test]
  fn a_wall_refuses_the_step_rather_than_stopping_it_halfway() {
    let grid = board();
    let mut p = player(0, Cell::new(1, 1));
    advance_player(&mut p, Dir::Up, &grid, &[], 16);
    assert!(p.step.is_none(), "there is no half-entered cell to be in");
    assert_eq!(p.cell, Cell::new(1, 1));
  }

  #[test]
  fn your_own_bomb_lets_you_leave_and_then_blocks_you() {
    let grid = board();
    let mut p = player(0, Cell::new(1, 1));
    let bombs = vec![BombState {
      cell: Cell::new(1, 1),
      owner: 0,
      fires_at_ms: 9999,
      radius: 1,
    }];
    assert!(passable(&grid, &bombs, Cell::new(1, 1), Cell::new(2, 1)), "off it");
    assert!(!passable(&grid, &bombs, Cell::new(2, 1), Cell::new(1, 1)), "and not back");
  }

  #[test]
  fn the_carry_limit_and_the_occupied_cell_both_refuse_a_bomb() {
    let mut p = player(0, Cell::new(1, 1));
    assert_eq!(bomb_placement(&p, &[]), Some(Cell::new(1, 1)));

    let mine = vec![BombState {
      cell: Cell::new(1, 1),
      owner: 0,
      fires_at_ms: 1,
      radius: 1,
    }];
    assert_eq!(bomb_placement(&p, &mine), None, "one carry slot, already spent");

    p.bombs_max = 4;
    assert_eq!(bomb_placement(&p, &mine), None, "and the cell is taken regardless");
  }

  #[test]
  fn a_dead_player_neither_walks_nor_bombs() {
    let grid = board();
    let mut p = player(0, Cell::new(1, 1));
    p.alive = false;
    advance_player(&mut p, Dir::Right, &grid, &[], 16);
    assert!(p.step.is_none());
    assert_eq!(bomb_placement(&p, &[]), None);
  }
}
