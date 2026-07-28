//! The lattice, and everything standing on it.
//!
//! The one decision the rest of this example follows from: **a player's position
//! is a cell**, not a point. A step from one cell to the next takes time and is
//! drawn as motion, but the simulation only ever knows "in cell C" or "walking
//! from C to D, N ticks in". Nothing rounds a float to a cell, because there is
//! no float to round.
//!
//! That is what makes this the counterpoint to the continuous playgrounds. A
//! position error of two pixels can be eased away over a few frames and nobody
//! sees it. A cell error cannot: you are either in the blast or you are not, and
//! there is no halfway to ease through. Every correction here is discrete, which
//! means every correction is visible, which is the whole subject.

use serde::{Deserialize, Serialize};

/// Cells across. Odd, so the indestructible pillar lattice has a border on both
/// sides.
pub const GRID_W: u8 = 15;
/// Cells down.
pub const GRID_H: u8 = 13;

/// The simulation step, in milliseconds. The server's tick counter is derived
/// from its clock by this, never counted alongside it: two representations of
/// one fact eventually disagree, and the one that broke horde was exactly this
/// pair.
pub const SIM_STEP_MS: u64 = 16;

/// How long one cell of walking takes at speed level zero.
///
/// The unit the whole game is tuned in: a bomb's fuse is measured in cells of
/// escape distance, and a blast radius is measured in cells you have to clear.
pub const STEP_MS_BASE: u64 = 240;
/// What each speed pickup takes off a step, floored so a player cannot outrun
/// the tick.
pub const STEP_MS_PER_LEVEL: u64 = 35;
pub const STEP_MS_FLOOR: u64 = 100;
pub const MAX_SPEED_LEVEL: u8 = 3;

/// How long a bomb sits before it fires.
pub const FUSE_MS: u64 = 2400;
/// How long a blast stays on the board, both lethal and drawn.
pub const BLAST_MS: u64 = 420;
/// Blast arms at radius level zero, in cells beyond the bomb's own.
pub const BLAST_RADIUS_BASE: u8 = 1;
pub const MAX_BLAST_RADIUS: u8 = 6;
/// Bombs a player may have live at once, at level zero.
pub const BOMBS_BASE: u8 = 1;
pub const MAX_BOMBS: u8 = 6;

/// How long a dead player waits before the next round starts, once one player
/// (or nobody) is left.
pub const ROUND_END_MS: u64 = 2500;

/// Share of destroyed soft walls that reveal a pickup.
pub const POWERUP_IN: u32 = 3;

/// The seed the tests and the offline harness build their board from.
pub const B0MB_SEED: u64 = 0xB0_1B_5EED_u64;

/// Who a player is. A `u8` because the arena seats at most a handful, and it
/// rides in every frame.
pub type PlayerId = u8;

/// A board coordinate. Packed into a `u16` on the wire, because a cell is two
/// small numbers and a struct of two `u8`s costs a MessagePack array header it
/// does not need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "u16", from = "u16")]
pub struct Cell {
  pub x: u8,
  pub y: u8,
}

impl From<Cell> for u16 {
  fn from(c: Cell) -> Self {
    ((c.x as u16) << 8) | c.y as u16
  }
}

impl From<u16> for Cell {
  fn from(v: u16) -> Self {
    Cell {
      x: (v >> 8) as u8,
      y: (v & 0xff) as u8,
    }
  }
}

impl Cell {
  pub const fn new(x: u8, y: u8) -> Self {
    Self { x, y }
  }

  /// The neighbour in `dir`, or `None` off the board.
  ///
  /// Returning an `Option` rather than clamping is deliberate: clamping at the
  /// edge silently turns "walk into the wall" into "stay put", which is the
  /// right *outcome* but hides the case from anything that wants to know a move
  /// was refused. The caller decides.
  pub fn step(self, dir: Dir) -> Option<Cell> {
    let (dx, dy) = dir.delta();
    let x = self.x as i16 + dx as i16;
    let y = self.y as i16 + dy as i16;
    (x >= 0 && y >= 0 && x < GRID_W as i16 && y < GRID_H as i16).then(|| Cell::new(x as u8, y as u8))
  }

  /// Manhattan distance, which on a lattice with no diagonals is the real one.
  pub fn distance(self, other: Cell) -> u16 {
    self.x.abs_diff(other.x) as u16 + self.y.abs_diff(other.y) as u16
  }
}

/// A direction, including standing still.
///
/// `None` is a variant rather than an `Option<Dir>` because it is a real input:
/// releasing the key is an intent the server must hear, and wrapping it costs a
/// byte on a message sent every tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Dir {
  #[default]
  None,
  Up,
  Down,
  Left,
  Right,
}

impl Dir {
  pub const fn delta(self) -> (i8, i8) {
    match self {
      Dir::None => (0, 0),
      Dir::Up => (0, -1),
      Dir::Down => (0, 1),
      Dir::Left => (-1, 0),
      Dir::Right => (1, 0),
    }
  }

  pub const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];
}

impl From<Dir> for u8 {
  fn from(d: Dir) -> Self {
    match d {
      Dir::None => 0,
      Dir::Up => 1,
      Dir::Down => 2,
      Dir::Left => 3,
      Dir::Right => 4,
    }
  }
}

impl TryFrom<u8> for Dir {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Dir::None),
      1 => Ok(Dir::Up),
      2 => Ok(Dir::Down),
      3 => Ok(Dir::Left),
      4 => Ok(Dir::Right),
      other => Err(format!("unknown Dir {other}")),
    }
  }
}

/// What is standing in a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Tile {
  #[default]
  Empty,
  /// Destructible. May hide a pickup.
  Soft,
  /// The pillar lattice. Never changes, which is why the grid is only ever sent
  /// once per round.
  Hard,
}

impl Tile {
  pub const fn walkable(self) -> bool {
    matches!(self, Tile::Empty)
  }
  /// Whether a blast arm stops *after* this cell. A soft wall takes the hit and
  /// absorbs the rest of the arm; a hard wall stops it before the cell.
  pub const fn absorbs_blast(self) -> bool {
    matches!(self, Tile::Soft)
  }
}

impl From<Tile> for u8 {
  fn from(t: Tile) -> Self {
    match t {
      Tile::Empty => 0,
      Tile::Soft => 1,
      Tile::Hard => 2,
    }
  }
}

impl TryFrom<u8> for Tile {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Tile::Empty),
      1 => Ok(Tile::Soft),
      2 => Ok(Tile::Hard),
      other => Err(format!("unknown Tile {other}")),
    }
  }
}

/// What a destroyed soft wall can be hiding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Powerup {
  /// One more bomb live at a time.
  ExtraBomb,
  /// One more cell of blast in every arm.
  LongerBlast,
  /// A shorter step.
  Speed,
}

impl From<Powerup> for u8 {
  fn from(p: Powerup) -> Self {
    match p {
      Powerup::ExtraBomb => 0,
      Powerup::LongerBlast => 1,
      Powerup::Speed => 2,
    }
  }
}

impl TryFrom<u8> for Powerup {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Powerup::ExtraBomb),
      1 => Ok(Powerup::LongerBlast),
      2 => Ok(Powerup::Speed),
      other => Err(format!("unknown Powerup {other}")),
    }
  }
}

impl Powerup {
  pub const fn label(self) -> &'static str {
    match self {
      Powerup::ExtraBomb => "bomb",
      Powerup::LongerBlast => "range",
      Powerup::Speed => "speed",
    }
  }

  /// A deterministic mix, so a replay of the same seed reveals the same pickups.
  pub fn from_seed(seed: u32) -> Self {
    match seed.wrapping_mul(2_246_822_519) % 3 {
      0 => Powerup::ExtraBomb,
      1 => Powerup::LongerBlast,
      _ => Powerup::Speed,
    }
  }
}

/// The board. Only the tiles: everything that moves lives elsewhere.
///
/// Sent whole exactly once per round, in the `Welcome` or the round start,
/// because a 15x13 board is 195 bytes and a delta of it would be more machinery
/// than the thing it compresses. What *changes* rides as an event: a blast says
/// which cells it cleared, and clearing is monotonic within a round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
  pub tiles: Vec<Tile>,
}

impl Default for Grid {
  fn default() -> Self {
    Self {
      tiles: vec![Tile::Empty; GRID_W as usize * GRID_H as usize],
    }
  }
}

impl Grid {
  /// The classic layout: a hard border, hard pillars on even/even, soft walls
  /// scattered over the rest, and the four spawn corners kept clear.
  ///
  /// `seed` drives the soft-wall scatter, so a round is reproducible from one
  /// number, which is what lets the offline harness replay a round exactly.
  pub fn generate(seed: u64, players: usize) -> Self {
    let mut grid = Grid::default();
    for y in 0..GRID_H {
      for x in 0..GRID_W {
        let border = x == 0 || y == 0 || x == GRID_W - 1 || y == GRID_H - 1;
        let pillar = x % 2 == 0 && y % 2 == 0;
        if border || pillar {
          grid.set(Cell::new(x, y), Tile::Hard);
        }
      }
    }

    // The spawn pockets: a player must be able to step out of the corner in two
    // directions on the first tick, or the opening move is a coin flip between
    // walking into a wall and dying to your own first bomb.
    let mut clear: Vec<Cell> = Vec::new();
    for spawn in spawns(players) {
      clear.push(spawn);
      for dir in Dir::ALL {
        if let Some(c) = spawn.step(dir) {
          clear.push(c);
        }
        if let Some(c) = spawn.step(dir).and_then(|c| c.step(dir)) {
          clear.push(c);
        }
      }
    }

    let mut state = seed | 1;
    for y in 1..GRID_H - 1 {
      for x in 1..GRID_W - 1 {
        let cell = Cell::new(x, y);
        if grid.get(cell) != Tile::Empty || clear.contains(&cell) {
          continue;
        }
        // xorshift, written out because the whole point is that two builds of
        // this example agree on the board from the seed alone.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if state % 100 < 72 {
          grid.set(cell, Tile::Soft);
        }
      }
    }
    grid
  }

  #[inline]
  fn index(cell: Cell) -> usize {
    cell.y as usize * GRID_W as usize + cell.x as usize
  }

  pub fn get(&self, cell: Cell) -> Tile {
    self.tiles.get(Self::index(cell)).copied().unwrap_or(Tile::Hard)
  }

  pub fn set(&mut self, cell: Cell, tile: Tile) {
    let i = Self::index(cell);
    if let Some(slot) = self.tiles.get_mut(i) {
      *slot = tile;
    }
  }

  pub fn walkable(&self, cell: Cell) -> bool {
    self.get(cell).walkable()
  }

  pub fn soft_walls(&self) -> usize {
    self.tiles.iter().filter(|t| **t == Tile::Soft).count()
  }
}

/// Where each player starts, corners first.
pub fn spawns(players: usize) -> Vec<Cell> {
  let corners = [
    Cell::new(1, 1),
    Cell::new(GRID_W - 2, GRID_H - 2),
    Cell::new(GRID_W - 2, 1),
    Cell::new(1, GRID_H - 2),
  ];
  corners.into_iter().take(players.clamp(1, 4)).collect()
}

/// A walk in progress: which way, and how far through it.
///
/// `progress_ms` rather than a fraction, because the server counts in
/// milliseconds and a fraction would have to be recomputed against a step
/// duration that changes when a speed pickup lands mid-walk.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
  pub from: Cell,
  pub to: Cell,
  pub progress_ms: u16,
  pub duration_ms: u16,
}

impl Step {
  /// How far along, `0..=1`. **Presentation only**: no rule reads this, because
  /// a rule that did would be deciding something on a fraction of a cell, and
  /// the whole design is that the simulation knows cells.
  pub fn t(&self) -> f32 {
    if self.duration_ms == 0 {
      1.0
    } else {
      (self.progress_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }
  }
}

/// One player, as both sides model them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
  pub id: PlayerId,
  /// The cell this player **occupies**. During a step it is the cell being left
  /// until the step completes, so a player is never in two cells at once and a
  /// blast never has to decide which half of a walk it caught.
  pub cell: Cell,
  pub step: Option<Step>,
  pub alive: bool,
  pub bombs_max: u8,
  pub blast_radius: u8,
  pub speed_level: u8,
  /// Rounds won, kept across rounds so the scoreboard means something.
  pub wins: u16,
}

impl PlayerState {
  pub fn new(id: PlayerId, cell: Cell) -> Self {
    Self {
      id,
      cell,
      step: None,
      alive: true,
      bombs_max: BOMBS_BASE,
      blast_radius: BLAST_RADIUS_BASE,
      speed_level: 0,
      wins: 0,
    }
  }

  /// How long one cell takes for this player.
  pub fn step_ms(&self) -> u64 {
    STEP_MS_BASE.saturating_sub(STEP_MS_PER_LEVEL * self.speed_level as u64).max(STEP_MS_FLOOR)
  }

  /// The cell every **rule** judges this player in: what a blast catches, what
  /// a pickup is collected from, where a bomb is dropped.
  ///
  /// A step commits at its halfway point. That is the one place a fraction of a
  /// step reaches a rule, and it is deliberate: committing on *arrival* means
  /// stepping out of a bomb's cell leaves you dying in it for a whole step
  /// after you visibly left, and committing on *departure* lets you claim a
  /// cell you have not reached. Halfway is the only choice that agrees with
  /// what the player can see, and it is still discrete: one cell, never two.
  ///
  /// [`Self::cell`] remains the committed cell, and is what a correction is
  /// compared against.
  pub fn occupied(&self) -> Cell {
    match &self.step {
      Some(step) if step.progress_ms as u32 * 2 >= step.duration_ms as u32 => step.to,
      Some(step) => step.from,
      None => self.cell,
    }
  }

  /// Where to *draw* this player: the cell, or between two cells mid-step.
  ///
  /// The only place a fractional position exists, and it is derived here for
  /// the renderer rather than stored, so nothing can accidentally simulate
  /// against it.
  pub fn draw_pos(&self) -> (f32, f32) {
    match &self.step {
      Some(step) => {
        let t = step.t();
        let (fx, fy) = (step.from.x as f32, step.from.y as f32);
        let (tx, ty) = (step.to.x as f32, step.to.y as f32);
        (fx + (tx - fx) * t, fy + (ty - fy) * t)
      }
      None => (self.cell.x as f32, self.cell.y as f32),
    }
  }

  /// Resets everything a new round should not inherit. Wins survive.
  pub fn reset_for_round(&mut self, cell: Cell) {
    self.cell = cell;
    self.step = None;
    self.alive = true;
    self.bombs_max = BOMBS_BASE;
    self.blast_radius = BLAST_RADIUS_BASE;
    self.speed_level = 0;
  }
}

/// A bomb sitting on the board.
///
/// `fires_at_ms` is on the **server clock**, declared rather than counted down,
/// which is what lets a client draw an accurate fuse without a countdown of its
/// own drifting against the server's. A chain reaction changes this number, and
/// the change is announced, because a chained bomb fires early and a client
/// counting its own fuse would be wrong for exactly as long as the fuse had
/// left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BombState {
  pub cell: Cell,
  pub owner: PlayerId,
  pub fires_at_ms: u64,
  pub radius: u8,
}

/// A pickup lying on the board.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PowerupState {
  pub cell: Cell,
  pub kind: Powerup,
}

/// What the panel can change, on either side.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  /// How long the server holds an input before executing it.
  ///
  /// The fairness knob, and it matters more here than in a continuous game: two
  /// players reaching for the same pickup, or the same escape cell, is decided
  /// by whoever the server processes first. Scheduling by *press time* means
  /// that is decided by who pressed first rather than by who is nearer the
  /// server.
  pub playout_delay_ms: u64,
  /// Ticks either side of its named tick that an input is still accepted.
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub input_playout: bool,
  /// Predict local movement, and snap when the server disagrees. Off, the
  /// player is drawn only where the server says, so the same link feels like a
  /// round trip of input lag: the switch that makes the trade visible.
  pub predict_local: bool,
  /// Predict a bomb the instant it is asked for. A bomb is the discrete event
  /// with no way to ease a mistake, so a refused one has to vanish.
  pub predict_bombs: bool,
  /// How often the server sends state.
  pub sync_hz: u32,
  /// How far behind the server clock a client draws remote state.
  pub render_delay_ms: u64,
  pub players: usize,
  /// Fill empty seats with bots, so a single player still has a game.
  pub bots: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 40,
      jitter_ms: 15,
      loss_pct: 0.0,
      playout_delay_ms: 100,
      // Roughly the playout depth in 16 ms steps, plus slack for jitter.
      input_max_late_ticks: 4,
      input_max_early_ticks: 10,
      input_playout: true,
      predict_local: true,
      predict_bombs: true,
      sync_hz: 20,
      // one_way (40) + jitter (15) + one send interval (50), plus margin. The
      // same budget the other playgrounds pay, and for the same reason:
      // interpolation needs two samples bracketing the instant being drawn.
      render_delay_ms: 140,
      players: 4,
      bots: true,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_cell_round_trips_through_its_packed_form() {
    for x in 0..GRID_W {
      for y in 0..GRID_H {
        let cell = Cell::new(x, y);
        assert_eq!(Cell::from(u16::from(cell)), cell);
      }
    }
  }

  #[test]
  fn stepping_off_the_board_is_refused_rather_than_clamped() {
    // Clamping would turn "walked into the wall" into "stood still", which is
    // the right outcome by accident and hides the refusal from the caller.
    assert_eq!(Cell::new(0, 0).step(Dir::Up), None);
    assert_eq!(Cell::new(0, 0).step(Dir::Left), None);
    assert_eq!(Cell::new(0, 0).step(Dir::Right), Some(Cell::new(1, 0)));
    assert_eq!(Cell::new(GRID_W - 1, GRID_H - 1).step(Dir::Right), None);
  }

  #[test]
  fn a_generated_board_is_a_function_of_its_seed() {
    // The offline harness replays rounds; if the board were not reproducible
    // from the seed, a replay would be of a different game.
    assert_eq!(Grid::generate(7, 4), Grid::generate(7, 4));
    assert_ne!(Grid::generate(7, 4), Grid::generate(8, 4));
  }

  #[test]
  fn every_spawn_can_step_out_in_two_directions() {
    // Otherwise the opening move is a coin flip between walking into a wall and
    // dying to your own first bomb.
    let grid = Grid::generate(B0MB_SEED, 4);
    for spawn in spawns(4) {
      let outs = Dir::ALL.iter().filter(|d| spawn.step(**d).is_some_and(|c| grid.walkable(c))).count();
      assert!(outs >= 2, "spawn {spawn:?} has only {outs} way(s) out");
    }
  }

  #[test]
  fn the_border_and_the_pillars_are_never_walkable() {
    let grid = Grid::generate(99, 4);
    for x in 0..GRID_W {
      assert_eq!(grid.get(Cell::new(x, 0)), Tile::Hard);
      assert_eq!(grid.get(Cell::new(x, GRID_H - 1)), Tile::Hard);
    }
    assert_eq!(grid.get(Cell::new(2, 2)), Tile::Hard, "the even/even pillar lattice");
    assert_eq!(grid.get(Cell::new(4, 6)), Tile::Hard);
  }

  #[test]
  fn a_speed_pickup_shortens_a_step_but_never_below_the_floor() {
    let mut player = PlayerState::new(0, Cell::new(1, 1));
    let base = player.step_ms();
    player.speed_level = 1;
    assert!(player.step_ms() < base);
    player.speed_level = 200;
    assert_eq!(player.step_ms(), STEP_MS_FLOOR, "a player cannot outrun the tick");
  }

  #[test]
  fn a_drawn_position_is_derived_and_never_stored() {
    // The presentation-only rule, as a test: mid-step the drawn position is
    // between two cells while `cell` still names exactly one.
    let mut player = PlayerState::new(0, Cell::new(1, 1));
    player.step = Some(Step {
      from: Cell::new(1, 1),
      to: Cell::new(2, 1),
      progress_ms: 120,
      duration_ms: 240,
    });
    let (x, y) = player.draw_pos();
    assert!((x - 1.5).abs() < 0.01 && (y - 1.0).abs() < 0.01);
    assert_eq!(player.cell, Cell::new(1, 1), "the simulation still knows one cell");
  }
}
