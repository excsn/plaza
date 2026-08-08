//! The authoritative state. Only [`crate::logic::ForgeLogic`] mutates it.

use std::collections::HashMap;

use bomb_grid::sim::server::Server;
use bomb_grid::sim::types::{spawns, Cell, Controls, Grid, Tile};
use plaza::agent::Agent;
use plaza::app_common::locking::state::LockManager;

use crate::protocol::{
  parse_tile_key, ForgePhase, ForgeView, Meters, PlayerId, BOARD_H, BOARD_W, REGIONS, TILE_HARD, TILE_SOFT,
};

/// A live playtest: bomb_grid's authoritative simulation, seated by editors.
pub struct Playtest {
  pub server: Server,
  pub controls: Controls,
  /// Which editor holds which of the simulation's seats.
  pub seat_of: Vec<PlayerId>,
}

pub struct ForgeState {
  pub phase: ForgePhase,
  /// The board **is** the property object: key "x,y", value a tile name.
  pub board: HashMap<String, String>,
  /// The spawn roster, ordered; order is seat order at playtest.
  pub spawns: Vec<(u32, (u8, u8))>,
  pub locks: LockManager<String, PlayerId>,
  pub editors: Vec<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
  pub meters: Meters,
  pub playtests_run: u32,
  pub playtest: Option<Playtest>,
  pub tick: u64,
}

impl std::fmt::Debug for ForgeState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("ForgeState")
  }
}

impl Default for ForgeState {
  fn default() -> Self {
    Self::new()
  }
}

impl ForgeState {
  pub fn new() -> Self {
    Self {
      phase: ForgePhase::Forge,
      board: HashMap::new(),
      spawns: Vec::new(),
      locks: LockManager::new(),
      editors: Vec::new(),
      agents: HashMap::new(),
      meters: Meters::default(),
      playtests_run: 0,
      playtest: None,
      tick: 0,
    }
  }

  pub fn is_editor(&self, player: PlayerId) -> bool {
    self.editors.contains(&player)
  }

  pub fn view(&self) -> ForgeView {
    ForgeView {
      phase: self.phase,
      board: self.board.clone(),
      spawns: self.spawns.clone(),
      locks: REGIONS
        .iter()
        .filter_map(|r| self.locks.get_lock_owner(&r.to_string()).map(|o| (r.to_string(), *o)))
        .collect(),
      editors: self.editors.clone(),
      meters: self.meters,
      playtests_run: self.playtests_run,
    }
  }

  /// The crossing: the property store becomes the grid bomb_grid plays.
  /// Everything unset is open floor; the outer ring is always wall, because a
  /// bomb chasing a player off the board is nobody's authored intent.
  pub fn to_grid(&self, players: usize) -> Grid {
    let mut grid = Grid::generate(1, players.max(1));
    for y in 0..BOARD_H {
      for x in 0..BOARD_W {
        grid.set(Cell::new(x, y), Tile::Empty);
      }
    }
    for (key, value) in &self.board {
      let Some((x, y)) = parse_tile_key(key) else { continue };
      if x >= BOARD_W || y >= BOARD_H {
        continue;
      }
      let tile = match value.as_str() {
        TILE_SOFT => Tile::Soft,
        TILE_HARD => Tile::Hard,
        _ => Tile::Empty,
      };
      grid.set(Cell::new(x, y), tile);
    }
    for x in 0..BOARD_W {
      grid.set(Cell::new(x, 0), Tile::Hard);
      grid.set(Cell::new(x, BOARD_H - 1), Tile::Hard);
    }
    for y in 0..BOARD_H {
      grid.set(Cell::new(0, y), Tile::Hard);
      grid.set(Cell::new(BOARD_W - 1, y), Tile::Hard);
    }
    grid
  }

  /// Where the roster puts each simulation seat; the defaults cover a roster
  /// shorter than the party.
  pub fn spawn_cells(&self, players: usize) -> Vec<Cell> {
    let defaults = spawns(players.max(1));
    (0..players)
      .map(|i| {
        self
          .spawns
          .get(i)
          .map(|(_, (x, y))| Cell::new((*x).clamp(1, BOARD_W - 2), (*y).clamp(1, BOARD_H - 2)))
          .unwrap_or(defaults[i % defaults.len()])
      })
      .collect()
  }
}
