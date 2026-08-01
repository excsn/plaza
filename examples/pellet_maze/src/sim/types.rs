//! The maze, and the things running around in it.
//!
//! One decision shapes everything else, and it is not the one `bomb_grid` made.
//! There a player **stands still** until you press something. Here a player is
//! **always moving**, and a key press is a request to change direction *at the
//! next place where that turn is legal*. You cannot stop, and you cannot turn
//! mid-corridor.
//!
//! That makes an input's execution point a **place** rather than a time, which
//! is the whole subject of this example. A tick-addressed input answers "when",
//! and answering "when" is not enough when the trigger is "where": two sides can
//! agree perfectly about the tick and still fire the turn at different
//! intersections, and the two players then run down *different corridors*. The
//! error is unbounded, where a mispredicted cell is one cell.

use serde::{Deserialize, Serialize};

/// Cells across and down. Odd both ways, so the corridor lattice has a wall on
/// every outer edge.
pub const MAZE_W: u8 = 19;
pub const MAZE_H: u8 = 15;

/// The simulation step. The tick is derived from the clock by this and never
/// counted beside it.
pub const SIM_STEP_MS: u64 = 16;

/// How long one cell of running takes.
///
/// The unit everything is tuned in: a corridor is measured in these, and the
/// turn buffer below is measured against it.
pub const STEP_MS_RUNNER: u64 = 145;
/// Pursuers are **slower**, and by more than it looks.
///
/// Three of them converge from three directions, so a small speed edge for the
/// runner is not an edge at all: what matters is whether the runner can outrun
/// the one closing on it while the others are still crossing the maze.
pub const STEP_MS_PURSUER: u64 = 205;
/// A pursuer that has just been eaten walks home this slowly.
pub const STEP_MS_EATEN: u64 = 90;

/// How long a queued turn stays live before it is forgotten.
///
/// **A place-triggered input still needs a time bound**, and this is the whole
/// of why. Without one, a turn pressed while running down a long corridor waits
/// for the *next* legal intersection however far away that is, so a press from
/// two seconds ago fires at a corner nobody meant to take. With one, an
/// unreachable turn simply expires and the player keeps going, which is what
/// they would expect.
///
/// It is deliberately a little longer than one cell, so pressing slightly early
/// into a corner works, which is the entire reason a player wants the buffer.
pub const TURN_BUFFER_MS: u64 = 260;

/// Pellets eaten to clear the maze.
pub const PELLET_VALUE: u32 = 1;
/// What catching the runner is worth.
pub const CATCH_VALUE: u32 = 25;
/// What eating a pursuer is worth, while energized.
pub const EAT_VALUE: u32 = 15;
/// Rounds in a match. Clearing a maze outright is rare with three pursuers
/// hunting, so the game is a **cumulative score over a match** rather than a
/// race to empty the board: every round contributes, and losing one early does
/// not end your afternoon.
pub const MATCH_ROUNDS: u32 = 5;
/// How long an energizer lasts.
pub const ENERGIZE_MS: u64 = 6_000;

/// How long before an energizer runs out that its victims start flashing.
pub const INVERSION_WARNING_MS: u64 = 1_800;
/// How long a vanish lasts.
pub const VANISH_MS: u64 = 4_500;
/// Roughly one power-up per this many corridor cells.
pub const POWERUP_DENSITY: usize = 22;
/// How long a caught runner waits before the round resets.
pub const ROUND_END_MS: u64 = 2200;
/// How long everybody is held still at the start of a round.
///
/// Not decoration. A player is dropped into a fresh maze in a role that may
/// have just changed, and a game where you are running before you have read
/// either is a game you lose to the interface. Held by the **server**, and
/// declared to clients as an instant rather than a duration, so every client
/// starts on the same tick rather than on whenever its own countdown finished.
pub const ROUND_START_MS: u64 = 3000;

/// How long the final table stays up before the next match is laid out.
///
/// Longer than the interval between rounds, because it is the only moment in
/// five rounds where the whole match is readable, and a table that clears into
/// a countdown is a table nobody read.
pub const MATCH_END_MS: u64 = 5000;
/// How close a pursuer must be to catch the runner: the same cell.
pub const CATCH_DISTANCE: u16 = 0;

/// The seed the tests and the offline harness build their maze from.
pub const MAZE_SEED: u64 = 0x9A2E_5EED;

pub type PlayerId = u8;

/// A maze coordinate, packed to a `u16` on the wire.
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

  /// The neighbour in `dir`, or `None` off the maze.
  pub fn step(self, dir: Dir) -> Option<Cell> {
    let (dx, dy) = dir.delta();
    let x = self.x as i16 + dx as i16;
    let y = self.y as i16 + dy as i16;
    (x >= 0 && y >= 0 && x < MAZE_W as i16 && y < MAZE_H as i16).then(|| Cell::new(x as u8, y as u8))
  }

  pub fn distance(self, other: Cell) -> u16 {
    self.x.abs_diff(other.x) as u16 + self.y.abs_diff(other.y) as u16
  }
}

/// A direction. Unlike `bomb_grid` there is no `None`: a runner is always
/// running, so "no direction" is not an intent a player can express.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Dir {
  #[default]
  Left,
  Right,
  Up,
  Down,
}

impl Dir {
  pub const fn delta(self) -> (i8, i8) {
    match self {
      Dir::Left => (-1, 0),
      Dir::Right => (1, 0),
      Dir::Up => (0, -1),
      Dir::Down => (0, 1),
    }
  }

  pub const fn opposite(self) -> Dir {
    match self {
      Dir::Left => Dir::Right,
      Dir::Right => Dir::Left,
      Dir::Up => Dir::Down,
      Dir::Down => Dir::Up,
    }
  }

  pub const ALL: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];
}

impl From<Dir> for u8 {
  fn from(d: Dir) -> Self {
    match d {
      Dir::Left => 0,
      Dir::Right => 1,
      Dir::Up => 2,
      Dir::Down => 3,
    }
  }
}

impl TryFrom<u8> for Dir {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Dir::Left),
      1 => Ok(Dir::Right),
      2 => Ok(Dir::Up),
      3 => Ok(Dir::Down),
      other => Err(format!("unknown Dir {other}")),
    }
  }
}

/// What a player is this round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Role {
  /// Eats pellets, is caught on contact.
  Runner,
  /// Chases the runner.
  Pursuer,
}

impl From<Role> for u8 {
  fn from(r: Role) -> Self {
    match r {
      Role::Runner => 0,
      Role::Pursuer => 1,
    }
  }
}

impl TryFrom<u8> for Role {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Role::Runner),
      1 => Ok(Role::Pursuer),
      other => Err(format!("unknown Role {other}")),
    }
  }
}

impl Role {
  pub const fn step_ms(self) -> u64 {
    match self {
      Role::Runner => STEP_MS_RUNNER,
      Role::Pursuer => STEP_MS_PURSUER,
    }
  }

  pub const fn label(self) -> &'static str {
    match self {
      Role::Runner => "runner",
      Role::Pursuer => "pursuer",
    }
  }
}

/// What a power-up does.
///
/// Both are **timed, server-authoritative state changes**, which is why they
/// are interesting here rather than only fun: a client predicting its movement
/// through the moment one starts or ends will disagree with the server about
/// who is dangerous to whom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Power {
  /// The roles invert for a while: the runner eats pursuers instead of being
  /// caught by them.
  Energize,
  /// The runner stops being **sent** to the other players.
  ///
  /// Not drawn dimly, not skipped by the renderer: omitted from the frames
  /// those players receive. A client that is handed a position it is not
  /// supposed to see has already lost the secret, whatever it chooses to draw,
  /// because a cheat client reads the buffer rather than the screen.
  Vanish,
}

impl From<Power> for u8 {
  fn from(p: Power) -> Self {
    match p {
      Power::Energize => 0,
      Power::Vanish => 1,
    }
  }
}

impl TryFrom<u8> for Power {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Power::Energize),
      1 => Ok(Power::Vanish),
      other => Err(format!("unknown Power {other}")),
    }
  }
}

impl Power {
  pub const fn label(self) -> &'static str {
    match self {
      Power::Energize => "energize",
      Power::Vanish => "vanish",
    }
  }

  pub const fn duration_ms(self) -> u64 {
    match self {
      Power::Energize => ENERGIZE_MS,
      Power::Vanish => VANISH_MS,
    }
  }

  /// A deterministic mix, so a seed lays out the same board twice.
  pub fn from_seed(seed: u64) -> Self {
    if seed % 2 == 0 { Power::Energize } else { Power::Vanish }
  }
}

/// A power-up lying in the maze.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerupState {
  pub cell: Cell,
  pub kind: Power,
}

/// A maze tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Tile {
  #[default]
  Wall,
  Corridor,
}

impl Tile {
  pub const fn open(self) -> bool {
    matches!(self, Tile::Corridor)
  }
}

impl From<Tile> for u8 {
  fn from(t: Tile) -> Self {
    match t {
      Tile::Wall => 0,
      Tile::Corridor => 1,
    }
  }
}

impl TryFrom<u8> for Tile {
  type Error = String;
  fn try_from(v: u8) -> Result<Self, Self::Error> {
    match v {
      0 => Ok(Tile::Wall),
      1 => Ok(Tile::Corridor),
      other => Err(format!("unknown Tile {other}")),
    }
  }
}

/// The maze, sent once per round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maze {
  pub tiles: Vec<Tile>,
}

impl Default for Maze {
  fn default() -> Self {
    Self {
      tiles: vec![Tile::Wall; MAZE_W as usize * MAZE_H as usize],
    }
  }
}

impl Maze {
  /// A maze with plenty of loops, which is what this example needs.
  ///
  /// A perfect maze (exactly one path between any two cells) would be the
  /// obvious generator and the wrong one: it is nearly all corridor and
  /// dead end, and the interesting case here is an **intersection**, where a
  /// queued turn has somewhere to go. So the generator carves a grid of
  /// corridors on the odd lattice and then knocks extra holes in it, which
  /// makes junctions the common case rather than the rare one.
  pub fn generate(seed: u64) -> Self {
    let mut maze = Maze::default();
    let mut state = seed | 1;
    let mut roll = move || {
      state ^= state << 13;
      state ^= state >> 7;
      state ^= state << 17;
      state
    };

    // Every odd cell is a corridor, and every odd cell is joined to its
    // neighbours, which alone gives a full lattice of junctions.
    for y in (1..MAZE_H - 1).step_by(2) {
      for x in (1..MAZE_W - 1).step_by(2) {
        maze.set(Cell::new(x, y), Tile::Corridor);
      }
    }
    // Then the links between them, most of which are opened.
    for y in (1..MAZE_H - 1).step_by(2) {
      for x in (1..MAZE_W - 1).step_by(2) {
        if x + 2 < MAZE_W - 1 && roll() % 100 < 78 {
          maze.set(Cell::new(x + 1, y), Tile::Corridor);
        }
        if y + 2 < MAZE_H - 1 && roll() % 100 < 78 {
          maze.set(Cell::new(x, y + 1), Tile::Corridor);
        }
      }
    }
    // Anything walled off entirely would strand whoever spawned there, so every
    // corridor cell is given at least one way out.
    for y in (1..MAZE_H - 1).step_by(2) {
      for x in (1..MAZE_W - 1).step_by(2) {
        let cell = Cell::new(x, y);
        if Dir::ALL.iter().any(|d| cell.step(*d).is_some_and(|c| maze.open(c))) {
          continue;
        }
        // **Chosen from the directions that can actually be opened.** Picking
        // freely and discarding an out-of-bounds choice does nothing at all for
        // a corner cell, and whether a seed strands somebody is then luck.
        let openable: Vec<Cell> = Dir::ALL
          .iter()
          .filter_map(|d| cell.step(*d))
          .filter(|c| c.x > 0 && c.y > 0 && c.x < MAZE_W - 1 && c.y < MAZE_H - 1)
          .collect();
        if !openable.is_empty() {
          maze.set(openable[(roll() as usize) % openable.len()], Tile::Corridor);
        }
      }
    }
    maze
  }

  #[inline]
  fn index(cell: Cell) -> usize {
    cell.y as usize * MAZE_W as usize + cell.x as usize
  }

  pub fn get(&self, cell: Cell) -> Tile {
    self.tiles.get(Self::index(cell)).copied().unwrap_or(Tile::Wall)
  }

  pub fn set(&mut self, cell: Cell, tile: Tile) {
    let i = Self::index(cell);
    if let Some(slot) = self.tiles.get_mut(i) {
      *slot = tile;
    }
  }

  pub fn open(&self, cell: Cell) -> bool {
    self.get(cell).open()
  }

  /// Which directions lead somewhere from `cell`.
  pub fn exits(&self, cell: Cell) -> Vec<Dir> {
    Dir::ALL.into_iter().filter(|d| cell.step(*d).is_some_and(|c| self.open(c))).collect()
  }

  /// Whether a queued turn has anywhere to go from here. More than two exits,
  /// or two that are not a straight line, is a junction: the place a turn can
  /// actually be taken.
  pub fn is_junction(&self, cell: Cell) -> bool {
    let exits = self.exits(cell);
    match exits.len() {
      0 | 1 => false,
      2 => exits[0] != exits[1].opposite(),
      _ => true,
    }
  }

  pub fn corridors(&self) -> Vec<Cell> {
    let mut out = Vec::new();
    for y in 0..MAZE_H {
      for x in 0..MAZE_W {
        let cell = Cell::new(x, y);
        if self.open(cell) {
          out.push(cell);
        }
      }
    }
    out
  }
}

/// Where each player starts. The runner in the middle, pursuers in corners, so
/// nobody is caught before they have moved.
pub fn spawns(players: usize) -> Vec<Cell> {
  let mid = Cell::new(MAZE_W / 2 | 1, MAZE_H / 2 | 1);
  let corners = [Cell::new(1, 1), Cell::new(MAZE_W - 2, MAZE_H - 2), Cell::new(MAZE_W - 2, 1), Cell::new(1, MAZE_H - 2)];
  std::iter::once(mid).chain(corners).take(players.clamp(1, 4)).collect()
}

/// A move between two cells, in progress.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
  pub from: Cell,
  pub to: Cell,
  pub progress_ms: u16,
  pub duration_ms: u16,
}

impl Step {
  /// How far along, `0..=1`. **Presentation only.**
  pub fn t(&self) -> f32 {
    if self.duration_ms == 0 {
      1.0
    } else {
      (self.progress_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }
  }
}

/// One player.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
  pub id: PlayerId,
  pub role: Role,
  /// The cell last committed to.
  pub cell: Cell,
  /// Where this player is heading. Always set: a player never stands still
  /// unless the wall in front of them says so.
  pub heading: Dir,
  pub step: Option<Step>,
  pub alive: bool,
  /// Cumulative across the **match**, not the round. Reset when a match ends.
  pub score: u32,
  pub rounds_won: u16,
  /// Server time this player stops being energized, or zero.
  pub energized_until_ms: u64,
  /// Server time this player stops being hidden, or zero.
  pub hidden_until_ms: u64,
  /// Server time an eaten pursuer is back in play, or zero.
  pub eaten_until_ms: u64,
}

impl PlayerState {
  pub fn new(id: PlayerId, role: Role, cell: Cell, heading: Dir) -> Self {
    Self {
      id,
      role,
      cell,
      heading,
      step: None,
      alive: true,
      score: 0,
      rounds_won: 0,
      energized_until_ms: 0,
      hidden_until_ms: 0,
      eaten_until_ms: 0,
    }
  }

  pub fn energized(&self, now_ms: u64) -> bool {
    now_ms < self.energized_until_ms
  }

  pub fn hidden(&self, now_ms: u64) -> bool {
    now_ms < self.hidden_until_ms
  }

  /// Whether this pursuer is walking home after being eaten, and so harmless.
  pub fn eaten(&self, now_ms: u64) -> bool {
    now_ms < self.eaten_until_ms
  }

  /// The cell rules judge this player in: the step commits at its halfway
  /// point, so what a pursuer catches agrees with what a player can see.
  pub fn occupied(&self) -> Cell {
    match &self.step {
      Some(step) if step.progress_ms as u32 * 2 >= step.duration_ms as u32 => step.to,
      Some(step) => step.from,
      None => self.cell,
    }
  }

  /// Where to draw this player. The only fractional position, derived here for
  /// the renderer so nothing can simulate against it.
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

  pub fn step_ms(&self) -> u64 {
    self.role.step_ms()
  }

  /// The step this player is actually taking now, which is not always their
  /// role's: an eaten pursuer hurries home.
  pub fn current_step_ms(&self, now_ms: u64) -> u64 {
    if self.eaten(now_ms) {
      STEP_MS_EATEN
    } else {
      self.role.step_ms()
    }
  }

  /// Resets what a round owns. **The score survives**, because it is the
  /// match's, and the match is the thing being played.
  pub fn reset_for_round(&mut self, cell: Cell, heading: Dir) {
    self.cell = cell;
    self.heading = heading;
    self.step = None;
    self.alive = true;
    self.energized_until_ms = 0;
    self.hidden_until_ms = 0;
    self.eaten_until_ms = 0;
  }
}

/// What the panel can change.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  /// What a lost packet costs, which is a property of the link rather than of
  /// this simulation. The transport underneath is a WebSocket, so the truthful
  /// answer is a retransmission: the frame is late and nothing is missing. The
  /// netcode above is written for the other answer, where the packet is gone,
  /// which is the one worth demonstrating here.
  pub datagram_link: bool,
  /// How long the server holds an input before it becomes eligible.
  ///
  /// Note "eligible", not "executed": a turn is scheduled for a tick like any
  /// other input, and then still has to wait for a place. Two delays in series,
  /// and only one of them is a number anybody chose.
  pub playout_delay_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub input_playout: bool,
  /// How long a queued turn stays live. The panel's most interesting slider:
  /// short is precise and unforgiving, long takes corners you did not mean.
  pub turn_buffer_ms: u64,
  pub predict_local: bool,
  pub sync_hz: u32,
  pub render_delay_ms: u64,
  pub players: usize,
  pub bots: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 40,
      jitter_ms: 15,
      loss_pct: 0.0,
      datagram_link: true,
      playout_delay_ms: 100,
      input_max_late_ticks: 4,
      input_max_early_ticks: 10,
      input_playout: true,
      turn_buffer_ms: TURN_BUFFER_MS,
      predict_local: true,
      sync_hz: 20,
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
    for x in 0..MAZE_W {
      for y in 0..MAZE_H {
        let cell = Cell::new(x, y);
        assert_eq!(Cell::from(u16::from(cell)), cell);
      }
    }
  }

  #[test]
  fn a_generated_maze_is_a_function_of_its_seed() {
    assert_eq!(Maze::generate(7), Maze::generate(7));
    assert_ne!(Maze::generate(7), Maze::generate(8));
  }

  #[test]
  fn the_maze_has_junctions_rather_than_being_all_corridor() {
    let maze = Maze::generate(MAZE_SEED);
    let junctions = maze.corridors().iter().filter(|c| maze.is_junction(**c)).count();
    let corridors = maze.corridors().len();
    assert!(
      junctions * 4 > corridors,
      "junctions should be common: {junctions} of {corridors} cells"
    );
  }

  #[test]
  fn every_corridor_cell_has_a_way_out_on_every_seed() {
    // A walled-in cell strands whoever spawns in it: they cannot move at all,
    // which is an unplayable round rather than an awkward one.
    //
    // **Many seeds, not one.** A test over one sample of a random generator is
    // a test of that sample.
    for seed in 0..400u64 {
      let maze = Maze::generate(seed.wrapping_mul(2_654_435_761).wrapping_add(1));
      for cell in maze.corridors() {
        assert!(!maze.exits(cell).is_empty(), "seed {seed}: {cell:?} is walled in");
      }
    }
  }

  #[test]
  fn every_spawn_can_move_on_every_seed() {
    for seed in 0..400u64 {
      let maze = Maze::generate(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
      for spawn in spawns(4) {
        assert!(maze.open(spawn), "seed {seed}: spawn {spawn:?} is inside a wall");
        assert!(!maze.exits(spawn).is_empty(), "seed {seed}: spawn {spawn:?} is walled in");
      }
    }
  }

  #[test]
  fn a_straight_corridor_is_not_a_junction() {
    let mut maze = Maze::default();
    for x in 1..6u8 {
      maze.set(Cell::new(x, 1), Tile::Corridor);
    }
    assert!(!maze.is_junction(Cell::new(3, 1)), "straight through is not a choice");
    assert!(!maze.is_junction(Cell::new(1, 1)), "a dead end is not a choice either");

    maze.set(Cell::new(3, 2), Tile::Corridor);
    assert!(maze.is_junction(Cell::new(3, 1)), "a side passage makes it one");
  }

  #[test]
  fn every_spawn_is_in_a_corridor() {
    let maze = Maze::generate(MAZE_SEED);
    for spawn in spawns(4) {
      assert!(maze.open(spawn), "{spawn:?} is inside a wall");
    }
  }

  #[test]
  fn a_drawn_position_is_derived_and_never_stored() {
    let mut player = PlayerState::new(0, Role::Runner, Cell::new(1, 1), Dir::Right);
    player.step = Some(Step {
      from: Cell::new(1, 1),
      to: Cell::new(2, 1),
      progress_ms: 75,
      duration_ms: 150,
    });
    let (x, y) = player.draw_pos();
    assert!((x - 1.5).abs() < 0.01 && (y - 1.0).abs() < 0.01);
    assert_eq!(player.cell, Cell::new(1, 1), "the simulation still knows one cell");
  }
}
