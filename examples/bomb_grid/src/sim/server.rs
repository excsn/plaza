//! The authority: owns the board, the bombs, and who is alive.
//!
//! Two things here are worth reading even if the rest is ordinary.
//!
//! **The cascade** ([`Server::detonate`]). A bomb going off can fire another,
//! which fires another, and the arms of each are cut by walls that earlier arms
//! in the same instant have just removed. Resolving that as "each bomb explodes
//! when its own fuse ends" gives a different board depending on the order the
//! bombs happen to be stored in. So it is a breadth-first sweep to a fixed
//! point, and the whole cascade is one event with one timestamp.
//!
//! **Inputs are scheduled, not applied on arrival** ([`plaza_server_utils::InputSchedule`]).
//! Every input names the tick it is meant for and executes on that tick, so two
//! players who pressed at the same instant are resolved in the order they
//! pressed rather than in the order their packets landed. In a continuous game
//! that buys fairness in contested pickups; here it also decides who reaches the
//! only escape cell, which is the difference between a round won and lost.

use plaza_server_utils::{InputSchedule, InputWindow};

use crate::sim::protocol::{BlastEvent, Frame, Intent, RoundStart};
use crate::sim::rules;
use crate::sim::types::*;

/// A seat's occupant, for the tick that drives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
  /// Driven by a connected player; the schedule holds their inputs.
  Human,
  /// Driven by the house, so an empty arena is still a game.
  Bot,
}

/// What one tick produced, for a caller to put on the wire.
#[derive(Clone, Debug, Default)]
pub struct Tickout {
  /// Present on a send round only.
  pub frame: Option<Frame>,
  /// Cascades resolved this tick. Sent the moment they happen rather than
  /// waiting for the next frame, because a death is an event and a client that
  /// learns about it a send interval late plays the flash in the wrong place.
  pub blasts: Vec<BlastEvent>,
  /// `(winner, milliseconds until the next round)`.
  pub round_over: Option<(Option<PlayerId>, u64)>,
  /// A fresh board, when the interval has elapsed.
  pub round_start: Option<RoundStart>,
}

/// The most elapsed time one call may spend, so a stalled host falls behind
/// visibly rather than freezing while it repays a debt.
const MAX_CATCH_UP_MS: u64 = 250;

/// One live blast cell and when it stops burning.
#[derive(Clone, Copy, Debug)]
struct Fire {
  cell: Cell,
  until_ms: u64,
}

pub struct Server {
  pub grid: Grid,
  pub players: Vec<PlayerState>,
  pub bombs: Vec<BombState>,
  pub powerups: Vec<PowerupState>,

  /// The simulation clock. The tick is derived from it (see [`Server::tick`]),
  /// never counted beside it. Only ever a whole multiple of [`SIM_STEP_MS`],
  /// because [`Server::advance`] spends time in whole ticks.
  clock_ms: u64,
  /// Elapsed time received but not yet spent as a whole tick.
  accumulated_ms: u64,
  round: u32,
  seed: u64,
  /// Fire currently on the board, which is what kills.
  fire: Vec<Fire>,
  /// When the current round ends, once a winner is settled.
  round_ends_at_ms: Option<u64>,
  winner: Option<PlayerId>,

  seats: Vec<Seat>,
  /// One tick-addressed input queue per seat.
  schedules: Vec<InputSchedule<Intent>>,
  /// The direction each seat is holding, which persists until changed: a walk
  /// is a level, not an edge, so a player keeps going while the key is down
  /// even if no input arrives for a few ticks.
  held: Vec<Dir>,
  /// Bots' next decision time, so they do not re-plan every tick.
  bot_next_ms: Vec<u64>,
  last_send_ms: u64,

  pub kills: u64,
  pub walls_destroyed: u64,
  pub bombs_placed: u64,
  /// The largest cascade seen, in bombs. The number that says whether chain
  /// resolution is doing anything worth its complexity.
  pub longest_chain: usize,
}

impl Clone for Server {
  /// `plaza` requires `Clone` on its state for the query command. Nothing on the
  /// hot path clones a `Server`; the schedules are rebuilt empty because a
  /// half-drained input queue is not a thing worth copying.
  fn clone(&self) -> Self {
    Self {
      grid: self.grid.clone(),
      players: self.players.clone(),
      bombs: self.bombs.clone(),
      powerups: self.powerups.clone(),
      clock_ms: self.clock_ms,
      accumulated_ms: self.accumulated_ms,
      round: self.round,
      seed: self.seed,
      fire: self.fire.clone(),
      round_ends_at_ms: self.round_ends_at_ms,
      winner: self.winner,
      seats: self.seats.clone(),
      schedules: self.seats.iter().map(|_| InputSchedule::new()).collect(),
      held: self.held.clone(),
      bot_next_ms: self.bot_next_ms.clone(),
      last_send_ms: self.last_send_ms,
      kills: self.kills,
      walls_destroyed: self.walls_destroyed,
      bombs_placed: self.bombs_placed,
      longest_chain: self.longest_chain,
    }
  }
}

impl std::fmt::Debug for Server {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Server")
      .field("round", &self.round)
      .field("clock_ms", &self.clock_ms)
      .field("alive", &self.players.iter().filter(|p| p.alive).count())
      .field("bombs", &self.bombs.len())
      .finish()
  }
}

impl Server {
  pub fn new(players: usize, seed: u64) -> Self {
    let count = players.clamp(1, 4);
    let grid = Grid::generate(seed, count);
    let states = spawns(count).into_iter().enumerate().map(|(i, cell)| PlayerState::new(i as PlayerId, cell)).collect();
    Self {
      grid,
      players: states,
      bombs: Vec::new(),
      powerups: Vec::new(),
      clock_ms: 0,
      accumulated_ms: 0,
      round: 1,
      seed,
      fire: Vec::new(),
      round_ends_at_ms: None,
      winner: None,
      seats: vec![Seat::Bot; count],
      schedules: (0..count).map(|_| InputSchedule::new()).collect(),
      held: vec![Dir::None; count],
      bot_next_ms: vec![0; count],
      last_send_ms: 0,
      kills: 0,
      walls_destroyed: 0,
      bombs_placed: 0,
      longest_chain: 0,
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  /// The tick the simulation is on.
  ///
  /// **Derived from the clock, never counted alongside it.** A separate counter
  /// has to be kept in step through every path that touches either, and a
  /// rebuild is such a path: the horde example reset one and preserved the
  /// other, and every client input was refused as impossibly stale from then
  /// on, with no symptom except a player who could not move.
  pub fn tick(&self) -> u64 {
    self.clock_ms / SIM_STEP_MS
  }

  pub fn round(&self) -> u32 {
    self.round
  }

  /// Whether the round is settled and the world is being held still.
  pub fn round_over_pending(&self) -> bool {
    self.round_ends_at_ms.is_some()
  }

  pub fn seats(&self) -> usize {
    self.seats.len()
  }

  pub fn take_seat(&mut self, seat: usize) {
    if let Some(slot) = self.seats.get_mut(seat) {
      *slot = Seat::Human;
    }
    if let Some(dir) = self.held.get_mut(seat) {
      *dir = Dir::None;
    }
    if let Some(schedule) = self.schedules.get_mut(seat) {
      schedule.clear();
    }
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(slot) = self.seats.get_mut(seat) {
      *slot = Seat::Bot;
    }
    if let Some(dir) = self.held.get_mut(seat) {
      *dir = Dir::None;
    }
    if let Some(schedule) = self.schedules.get_mut(seat) {
      schedule.clear();
    }
  }

  /// Offers an input for a named tick. The window decides; see
  /// [`InputSchedule`], which owns the reject-not-correct rule.
  pub fn submit(&mut self, seat: usize, tick: u64, intent: Intent, controls: &Controls) -> bool {
    let Some(schedule) = self.schedules.get_mut(seat) else {
      return false;
    };
    if !controls.input_playout {
      // The naive path, kept so the difference is measurable rather than
      // argued: whatever arrives takes effect on the next tick.
      return match intent {
        Intent::Walk(dir) => {
          self.held[seat] = dir;
          true
        }
        Intent::Bomb => {
          self.place_bomb(seat);
          true
        }
      };
    }
    let window = InputWindow {
      max_late: controls.input_max_late_ticks,
      max_early: controls.input_max_early_ticks,
    };
    let current = self.clock_ms / SIM_STEP_MS;
    schedule.submit(tick, intent, current, window).accepted()
  }

  /// Admission verdicts per seat: `(accepted, late, closed, ahead, last margin)`.
  ///
  /// The host-side half of a client's own readout. A client cannot see this: an
  /// input is acknowledged on arrival, before admission, so a refused one and an
  /// applied one look identical from there.
  pub fn input_verdicts(&self) -> Vec<(u64, u64, u64, u64, Option<i64>)> {
    self
      .schedules
      .iter()
      .map(|s| {
        let (closed, ahead) = s.rejected_split();
        (s.accepted(), s.late(), closed, ahead, s.last_reject_margin())
      })
      .collect()
  }

  /// The state a joiner needs to start drawing.
  pub fn round_start(&self) -> RoundStart {
    RoundStart {
      round: self.round,
      grid: self.grid.clone(),
      players: self.players.clone(),
      server_time_ms: self.clock_ms,
      tick: self.tick(),
    }
  }

  pub fn frame(&self) -> Frame {
    Frame {
      server_time_ms: self.clock_ms,
      tick: self.tick(),
      players: self.players.clone(),
      bombs: self.bombs.clone(),
      powerups: self.powerups.clone(),
    }
  }

  /// Cells currently on fire, for a renderer that has the truth.
  pub fn fire_cells(&self) -> Vec<Cell> {
    self.fire.iter().map(|f| f.cell).collect()
  }

  /// Advances the world by `dt_ms`, in **whole ticks**.
  ///
  /// The elapsed time is accumulated and spent in fixed [`SIM_STEP_MS`] steps,
  /// never applied raw, and that is a correctness requirement rather than
  /// tidiness: a tick driver hands over the *measured* elapsed time, so a 62 Hz
  /// driver delivers 16, 17, 16, 16, 17 and so on. Advancing the world by that
  /// directly makes the simulation's rate a property of the host's scheduler.
  ///
  /// Nothing can predict that. A client stepping in exact 16 ms ticks and a
  /// server stepping in measured ones accumulate a walk at different rates and
  /// cross each cell boundary a tick apart, which on a lattice is a whole cell
  /// of disagreement at every crossing. It cost 2.2 snaps per hundred frames on
  /// a link with no loss, no jitter worth the name, and every input accepted on
  /// time, which is a bug that looks exactly like a network problem.
  ///
  /// So the authority advances in the same quantum its clients predict in. The
  /// clock then only ever holds whole multiples of the step, which is also what
  /// keeps [`Server::tick`] exact.
  pub fn advance(&mut self, dt_ms: u64, controls: &Controls) -> Tickout {
    // A long stall (a debugger, a suspended host) must not be repaid as a
    // hundred steps in one call. The world falls behind instead, which is
    // visible, rather than freezing while it catches up, which is not.
    self.accumulated_ms += dt_ms.min(MAX_CATCH_UP_MS);
    let mut out = Tickout::default();
    while self.accumulated_ms >= SIM_STEP_MS {
      self.accumulated_ms -= SIM_STEP_MS;
      self.step(controls, &mut out);
    }
    out
  }

  /// One tick of exactly [`SIM_STEP_MS`].
  fn step(&mut self, controls: &Controls, out: &mut Tickout) {
    let dt_ms = SIM_STEP_MS;
    self.clock_ms += dt_ms;

    // A finished round holds still until its interval elapses, so the last
    // explosion is on screen long enough to see what happened.
    if let Some(ends_at) = self.round_ends_at_ms {
      self.expire_fire();
      if self.clock_ms >= ends_at {
        out.round_start = Some(self.begin_round());
      }
      if self.send_due(controls) {
        out.frame = Some(self.frame());
      }
      return;
    }

    self.drive_bots(controls);
    self.execute_due(controls);
    self.advance_steps(dt_ms);
    self.collect_powerups();

    let mut cascades = self.fire_due_bombs();
    self.expire_fire();
    let killed_now = self.burn_players();
    if !killed_now.is_empty() {
      // Deaths from fire lit on an earlier tick still belong to a blast the
      // client has already been told about, so they ride the newest cascade
      // rather than inventing an event with no explosion in it.
      if let Some(last) = cascades.last_mut() {
        last.killed.extend(killed_now);
      } else {
        cascades.push(BlastEvent {
          at_ms: self.clock_ms,
          killed: killed_now,
          ..Default::default()
        });
      }
    }
    out.blasts.extend(cascades);

    if let Some(result) = self.check_round_over() {
      out.round_over = Some(result);
    }
    if self.send_due(controls) {
      out.frame = Some(self.frame());
    }
  }

  fn send_due(&mut self, controls: &Controls) -> bool {
    let interval = controls.sync_interval_ms();
    if self.clock_ms.saturating_sub(self.last_send_ms) < interval {
      return false;
    }
    self.last_send_ms = self.clock_ms;
    true
  }

  /// Runs every input whose tick has come, oldest first.
  fn execute_due(&mut self, _controls: &Controls) {
    let current = self.clock_ms / SIM_STEP_MS;
    for seat in 0..self.schedules.len() {
      // Drained per *step*, not per network frame: consuming the queue once a
      // frame would collapse everything that arrived between two ticks onto
      // whichever tick happened to run next.
      let due: Vec<Intent> = self.schedules[seat].drain_due(current).collect();
      for intent in due {
        match intent {
          Intent::Walk(dir) => self.held[seat] = dir,
          Intent::Bomb => self.place_bomb(seat),
        }
      }
    }
  }

  /// Starts and finishes walks, through the rule a predicting client also runs.
  fn advance_steps(&mut self, dt_ms: u64) {
    for seat in 0..self.players.len() {
      rules::advance_player(&mut self.players[seat], self.held[seat], &self.grid, &self.bombs, dt_ms);
    }
  }

  /// Drops a bomb, if [`rules::bomb_placement`] allows it.
  fn place_bomb(&mut self, seat: usize) {
    let Some(player) = self.players.get(seat) else {
      return;
    };
    let Some(cell) = rules::bomb_placement(player, &self.bombs) else {
      return;
    };
    self.bombs.push(BombState {
      cell,
      owner: player.id,
      fires_at_ms: self.clock_ms + FUSE_MS,
      radius: player.blast_radius,
    });
    self.bombs_placed += 1;
  }

  /// Fires every bomb whose fuse has run out, plus everything they chain into.
  fn fire_due_bombs(&mut self) -> Vec<BlastEvent> {
    let ready: Vec<Cell> = self.bombs.iter().filter(|b| b.fires_at_ms <= self.clock_ms).map(|b| b.cell).collect();
    if ready.is_empty() {
      return Vec::new();
    }
    vec![self.detonate(&ready)]
  }

  /// Resolves one cascade to a fixed point.
  ///
  /// Breadth-first over bombs rather than a pass per bomb, because a chained
  /// bomb fires *now* and its own arms can reach a third, and because each arm
  /// is cut by walls the cascade itself is removing. Evaluating that in storage
  /// order would make the outcome depend on which bomb happened to be pushed
  /// first, which is exactly the kind of thing that reproduces once in fifty
  /// rounds and cannot be debugged from a report.
  fn detonate(&mut self, seeds: &[Cell]) -> BlastEvent {
    let mut event = BlastEvent {
      at_ms: self.clock_ms,
      ..Default::default()
    };
    let mut queue: Vec<Cell> = seeds.to_vec();
    let mut fired: Vec<Cell> = Vec::new();

    while let Some(cell) = queue.pop() {
      if fired.contains(&cell) {
        continue;
      }
      let Some(index) = self.bombs.iter().position(|b| b.cell == cell) else {
        continue;
      };
      let bomb = self.bombs.remove(index);
      fired.push(bomb.cell);
      event.bombs.push(bomb.cell);

      // The bomb's own cell always burns, then each arm walks outward until a
      // wall stops it.
      if !event.cells.contains(&bomb.cell) {
        event.cells.push(bomb.cell);
      }
      for dir in Dir::ALL {
        let mut cursor = bomb.cell;
        for _ in 0..bomb.radius.min(MAX_BLAST_RADIUS) {
          let Some(next) = cursor.step(dir) else {
            break;
          };
          cursor = next;
          let tile = self.grid.get(cursor);
          if tile == Tile::Hard {
            break;
          }
          if !event.cells.contains(&cursor) {
            event.cells.push(cursor);
          }
          // Another bomb in the arm joins this cascade rather than waiting for
          // its own fuse.
          if self.bombs.iter().any(|b| b.cell == cursor) && !fired.contains(&cursor) {
            queue.push(cursor);
          }
          if tile.absorbs_blast() {
            self.grid.set(cursor, Tile::Empty);
            self.walls_destroyed += 1;
            event.cleared.push(cursor);
            if let Some(kind) = self.reveal(cursor) {
              let drop = PowerupState { cell: cursor, kind };
              self.powerups.push(drop);
              event.revealed.push(drop);
            }
            // The wall takes the hit and the arm stops here.
            break;
          }
        }
      }
    }

    self.longest_chain = self.longest_chain.max(fired.len());

    // Fire on the board, which is what actually kills. Kept as its own list
    // with its own expiry so a blast that overlaps an older one does not cut
    // the older one short.
    let until = self.clock_ms + BLAST_MS;
    for cell in &event.cells {
      self.fire.push(Fire { cell: *cell, until_ms: until });
    }

    // Pickups in the fire are destroyed, so a contested one cannot sit in the
    // open forever waiting for whoever is nearest.
    let burning = event.cells.clone();
    self.powerups.retain(|p| {
      let burned = burning.contains(&p.cell) && !event.revealed.iter().any(|r| r.cell == p.cell);
      if burned {
        event.burned.push(p.cell);
      }
      !burned
    });

    event.killed = self.burn_players();
    event
  }

  /// Whether a destroyed wall was hiding something, and what.
  ///
  /// Deterministic in the cell and the seed, so two builds of this example
  /// reveal the same pickups from the same board. A running counter would make
  /// it depend on the order walls happened to be destroyed in.
  fn reveal(&self, cell: Cell) -> Option<Powerup> {
    let mix = (self.seed as u32)
      .wrapping_mul(2_654_435_761)
      .wrapping_add((cell.x as u32) << 8 | cell.y as u32)
      .wrapping_mul(2_246_822_519);
    (mix % POWERUP_IN == 0).then(|| Powerup::from_seed(mix >> 8))
  }

  fn expire_fire(&mut self) {
    let now = self.clock_ms;
    self.fire.retain(|f| f.until_ms > now);
  }

  /// Kills whoever is standing in fire. Returns who died this tick.
  fn burn_players(&mut self) -> Vec<PlayerId> {
    let mut killed = Vec::new();
    for player in self.players.iter_mut().filter(|p| p.alive) {
      if self.fire.iter().any(|f| f.cell == player.occupied()) {
        player.alive = false;
        player.step = None;
        killed.push(player.id);
      }
    }
    self.kills += killed.len() as u64;
    killed
  }

  fn collect_powerups(&mut self) {
    for player in self.players.iter_mut().filter(|p| p.alive) {
      let at = player.occupied();
      let Some(index) = self.powerups.iter().position(|p| p.cell == at) else {
        continue;
      };
      let pickup = self.powerups.remove(index);
      match pickup.kind {
        Powerup::ExtraBomb => player.bombs_max = (player.bombs_max + 1).min(MAX_BOMBS),
        Powerup::LongerBlast => player.blast_radius = (player.blast_radius + 1).min(MAX_BLAST_RADIUS),
        Powerup::Speed => player.speed_level = (player.speed_level + 1).min(MAX_SPEED_LEVEL),
      }
    }
  }

  /// Settles the round when one player or nobody is left standing.
  fn check_round_over(&mut self) -> Option<(Option<PlayerId>, u64)> {
    if self.round_ends_at_ms.is_some() {
      return None;
    }
    let alive: Vec<PlayerId> = self.players.iter().filter(|p| p.alive).map(|p| p.id).collect();
    // A one-seat arena is a practice board and never ends; anywhere else, one
    // survivor or none settles it. A draw is a real outcome: a shared blast
    // kills everyone standing in it, which happens more often than it sounds.
    if self.players.len() < 2 || alive.len() > 1 {
      return None;
    }
    let winner = alive.first().copied();
    if let Some(id) = winner {
      if let Some(player) = self.players.iter_mut().find(|p| p.id == id) {
        player.wins += 1;
      }
    }
    self.winner = winner;
    self.round_ends_at_ms = Some(self.clock_ms + ROUND_END_MS);
    Some((winner, ROUND_END_MS))
  }

  /// A fresh board, everyone back in a corner, upgrades surrendered.
  fn begin_round(&mut self) -> RoundStart {
    self.round += 1;
    self.seed = self.seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let count = self.players.len();
    self.grid = Grid::generate(self.seed, count);
    for (player, cell) in self.players.iter_mut().zip(spawns(count)) {
      player.reset_for_round(cell);
    }
    self.bombs.clear();
    self.powerups.clear();
    self.fire.clear();
    self.round_ends_at_ms = None;
    self.winner = None;
    for schedule in &mut self.schedules {
      schedule.clear();
    }
    for dir in &mut self.held {
      *dir = Dir::None;
    }
    self.round_start()
  }

  /// The house players. Deliberately simple: walk somewhere legal, drop a bomb
  /// near a wall now and then, and run from fire.
  ///
  /// They exist so a single joiner has a game, not to be good. What they must
  /// not do is stand still in fire, because a bot that never dies makes every
  /// round a draw by timeout and the round machinery would never be exercised.
  fn drive_bots(&mut self, controls: &Controls) {
    if !controls.bots {
      return;
    }
    for seat in 0..self.seats.len() {
      if self.seats[seat] != Seat::Bot || !self.players[seat].alive {
        continue;
      }
      if self.clock_ms < self.bot_next_ms[seat] {
        continue;
      }
      // Re-decide about three times a second, so a bot commits to a direction
      // for long enough to actually cross a cell.
      self.bot_next_ms[seat] = self.clock_ms + 300;

      let here = self.players[seat].occupied();
      let danger = |s: &Self, cell: Cell| s.fire.iter().any(|f| f.cell == cell) || s.bombs.iter().any(|b| b.cell.distance(cell) <= b.radius as u16 + 1);

      let mut options: Vec<Dir> = Dir::ALL
        .into_iter()
        .filter(|d| here.step(*d).is_some_and(|to| rules::passable(&self.grid, &self.bombs, here, to)))
        .collect();
      // Prefer somewhere not about to be on fire.
      let safe: Vec<Dir> = options.iter().copied().filter(|d| here.step(*d).is_some_and(|to| !danger(self, to))).collect();
      if !safe.is_empty() {
        options = safe;
      }

      let roll = self
        .clock_ms
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(seat as u64 * 2_654_435_761)
        .rotate_left(17);
      if options.is_empty() {
        self.held[seat] = Dir::None;
      } else {
        self.held[seat] = options[(roll as usize) % options.len()];
      }

      // Drop one when there is a wall worth breaking and it is not obviously
      // fatal to stand here.
      let wall_adjacent = Dir::ALL.into_iter().any(|d| here.step(d).is_some_and(|c| self.grid.get(c) == Tile::Soft));
      if wall_adjacent && !danger(self, here) && roll % 3 == 0 {
        self.place_bomb(seat);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn controls() -> Controls {
    Controls {
      bots: false,
      input_playout: false,
      players: 2,
      ..Controls::default()
    }
  }

  /// Runs the sim forward, one 16 ms step at a time.
  fn run(server: &mut Server, ms: u64, controls: &Controls) -> Vec<BlastEvent> {
    let mut blasts = Vec::new();
    for _ in 0..(ms / SIM_STEP_MS) {
      blasts.extend(server.advance(SIM_STEP_MS, controls).blasts);
    }
    blasts
  }

  #[test]
  fn a_walk_takes_a_whole_step_and_lands_on_a_cell() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.submit(0, 0, Intent::Walk(Dir::Right), &c);
    // Half a step in: the committed cell has not moved, but the drawn one has.
    run(&mut server, STEP_MS_BASE / 2 - SIM_STEP_MS, &c);
    assert_eq!(server.players[0].cell, Cell::new(1, 1), "the commit happens on arrival");
    assert!(server.players[0].step.is_some());
    run(&mut server, STEP_MS_BASE, &c);
    assert_eq!(server.players[0].cell, Cell::new(2, 1), "and then it is a whole cell over");
  }

  #[test]
  fn walking_into_a_wall_is_refused_not_clamped() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    // Player 0 spawns at (1,1); up is the hard border.
    server.submit(0, 0, Intent::Walk(Dir::Up), &c);
    run(&mut server, STEP_MS_BASE * 2, &c);
    assert_eq!(server.players[0].cell, Cell::new(1, 1));
    assert!(server.players[0].step.is_none(), "no step was ever started");
  }

  #[test]
  fn a_bomb_fires_on_its_declared_fuse_and_burns_a_cross() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.submit(0, 0, Intent::Bomb, &c);
    assert_eq!(server.bombs.len(), 1);

    let early = run(&mut server, FUSE_MS - 100, &c);
    assert!(early.is_empty(), "nothing fires before the fuse ends");

    let blasts = run(&mut server, 200, &c);
    assert_eq!(blasts.len(), 1, "one cascade");
    let blast = &blasts[0];
    assert!(blast.cells.contains(&Cell::new(1, 1)), "the bomb's own cell burns");
    // The arms reach one cell in each direction that is not a hard wall. At
    // (1,1) that is right and down; up and left are the border.
    assert!(blast.cells.contains(&Cell::new(2, 1)) || blast.cells.contains(&Cell::new(1, 2)));
    assert!(!blast.cells.contains(&Cell::new(0, 1)), "a hard wall stops the arm before the cell");
    assert!(server.bombs.is_empty(), "and the bomb is gone");
  }

  #[test]
  fn a_bomb_in_the_blast_chains_rather_than_waiting_for_its_own_fuse() {
    // The property the cascade exists for. Two bombs two cells apart, the
    // second dropped later so its own fuse is nowhere near ending.
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    // Clear a lane so the two bombs can see each other.
    for x in 1..5u8 {
      server.grid.set(Cell::new(x, 1), Tile::Empty);
    }
    server.players[0].blast_radius = 3;
    server.submit(0, 0, Intent::Bomb, &c);
    run(&mut server, 600, &c);

    // A second bomb, from the other player, well inside the first one's reach.
    server.players[1].cell = Cell::new(3, 1);
    server.submit(1, 0, Intent::Bomb, &c);
    let second_fuse_ends = server.bombs.iter().map(|b| b.fires_at_ms).max().unwrap();

    let blasts = run(&mut server, FUSE_MS, &c);
    assert_eq!(blasts.len(), 1, "one cascade, not two explosions");
    assert_eq!(blasts[0].bombs.len(), 2, "both bombs went off together");
    assert!(blasts[0].at_ms < second_fuse_ends, "the chained bomb fired early, which is the whole point");
    assert_eq!(server.longest_chain, 2);
  }

  #[test]
  fn a_soft_wall_absorbs_the_arm_and_may_hide_a_pickup() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.players[0].blast_radius = 4;
    server.grid.set(Cell::new(2, 1), Tile::Soft);
    server.grid.set(Cell::new(3, 1), Tile::Empty);
    server.submit(0, 0, Intent::Bomb, &c);
    let blasts = run(&mut server, FUSE_MS + 100, &c);

    let blast = &blasts[0];
    assert!(blast.cleared.contains(&Cell::new(2, 1)), "the wall was destroyed");
    assert!(blast.cells.contains(&Cell::new(2, 1)), "and it burned");
    assert!(!blast.cells.contains(&Cell::new(3, 1)), "but the arm stopped there");
    assert_eq!(server.grid.get(Cell::new(2, 1)), Tile::Empty);
  }

  #[test]
  fn standing_in_the_fire_is_fatal_and_announced() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.submit(0, 0, Intent::Bomb, &c);
    let blasts = run(&mut server, FUSE_MS + 100, &c);
    assert!(blasts.iter().any(|b| b.killed.contains(&0)), "the death rides the blast that caused it");
    assert!(!server.players[0].alive);
  }

  #[test]
  fn you_can_walk_off_your_own_bomb_but_not_back_onto_it() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.submit(0, 0, Intent::Bomb, &c);
    server.submit(0, 0, Intent::Walk(Dir::Right), &c);
    run(&mut server, STEP_MS_BASE + SIM_STEP_MS, &c);
    assert_eq!(server.players[0].cell, Cell::new(2, 1), "walked off it");

    server.submit(0, 0, Intent::Walk(Dir::Left), &c);
    run(&mut server, STEP_MS_BASE * 2, &c);
    assert_eq!(server.players[0].cell, Cell::new(2, 1), "and cannot walk back onto it");
  }

  #[test]
  fn a_last_survivor_wins_the_round_and_a_new_board_follows() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    let before = server.round();
    server.players[1].alive = false;
    let out = server.advance(SIM_STEP_MS, &c);
    let (winner, _) = out.round_over.expect("one survivor settles the round");
    assert_eq!(winner, Some(0));
    assert_eq!(server.players[0].wins, 1);

    // The board is held for a beat, then rebuilt.
    let mut started = None;
    for _ in 0..(ROUND_END_MS / SIM_STEP_MS + 4) {
      if let Some(round) = server.advance(SIM_STEP_MS, &c).round_start {
        started = Some(round);
        break;
      }
    }
    let round = started.expect("the next round begins");
    assert_eq!(round.round, before + 1);
    assert!(server.players.iter().all(|p| p.alive), "everyone is back");
    assert_eq!(server.players[0].wins, 1, "but the scoreboard survives");
    assert_eq!(server.players[0].bombs_max, BOMBS_BASE, "and the upgrades do not");
  }

  #[test]
  fn everyone_dying_together_is_a_draw_rather_than_a_win() {
    // A shared blast is the ordinary way a round ends between two players who
    // both know what they are doing, so it cannot be an edge case.
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.players[0].alive = false;
    server.players[1].alive = false;
    let out = server.advance(SIM_STEP_MS, &c);
    let (winner, _) = out.round_over.expect("nobody left settles it too");
    assert_eq!(winner, None);
  }

  #[test]
  fn an_input_named_for_a_closed_tick_is_refused() {
    // The playout window, which is what makes two players' presses resolve in
    // the order they were pressed rather than the order they arrived.
    let mut server = Server::new(2, B0MB_SEED);
    let c = Controls {
      input_playout: true,
      ..controls()
    };
    run(&mut server, 2000, &c);
    let current = server.tick();
    let stale = current - c.input_max_late_ticks - 5;
    assert!(!server.submit(0, stale, Intent::Walk(Dir::Right), &c), "a closed tick is refused");
    let (_, _, closed, ahead, margin) = server.input_verdicts()[0];
    assert_eq!((closed, ahead), (1, 0), "and refused on the late side");
    assert!(margin.is_some_and(|m| m < 0), "with a negative margin: {margin:?}");
  }

  #[test]
  fn a_scheduled_input_executes_on_the_tick_it_named() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = Controls {
      input_playout: true,
      ..controls()
    };
    let target = server.tick() + 5;
    assert!(server.submit(0, target, Intent::Walk(Dir::Right), &c));
    // Before its tick, nothing has moved.
    run(&mut server, SIM_STEP_MS * 3, &c);
    assert!(server.players[0].step.is_none(), "it waits for the tick it named");
    run(&mut server, SIM_STEP_MS * 4, &c);
    assert!(server.players[0].step.is_some(), "and then it runs");
  }

  #[test]
  fn a_bomb_cannot_be_stacked_or_exceed_the_carry_limit() {
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    server.submit(0, 0, Intent::Bomb, &c);
    server.submit(0, 0, Intent::Bomb, &c);
    assert_eq!(server.bombs.len(), 1, "one bomb, one cell, one carry slot");

    server.players[0].bombs_max = 2;
    server.submit(0, 0, Intent::Walk(Dir::Right), &c);
    run(&mut server, STEP_MS_BASE + SIM_STEP_MS, &c);
    server.submit(0, 0, Intent::Bomb, &c);
    assert_eq!(server.bombs.len(), 2, "a second slot buys a second bomb elsewhere");
  }

  #[test]
  fn the_tick_is_derived_from_the_clock() {
    // The pair that broke horde: a counter kept beside the clock drifts from it
    // through any path that touches one and not the other.
    let mut server = Server::new(2, B0MB_SEED);
    let c = controls();
    run(&mut server, 1600, &c);
    assert_eq!(server.tick(), server.now_ms() / SIM_STEP_MS);
  }

  #[test]
  fn an_irregular_tick_driver_produces_the_same_world_as_a_regular_one() {
    // `TickDriver` hands over the *measured* elapsed time, not the nominal
    // interval, so a 62 Hz driver delivers 16, 17, 16, 16, 17 and so on.
    // Advancing the world by that directly would make the simulation's rate a
    // property of the host's scheduler, and nothing can predict that: a client
    // stepping in exact ticks and a server stepping in measured ones cross each
    // cell boundary a tick apart, which on a lattice is a whole cell.
    //
    // It cost 2.2 snaps per hundred frames on a link with no loss and every
    // input accepted on time, which is a bug wearing a network problem's
    // clothes.
    let c = controls();
    let mut regular = Server::new(2, B0MB_SEED);
    let mut jittery = Server::new(2, B0MB_SEED);
    regular.submit(0, 0, Intent::Walk(Dir::Right), &c);
    jittery.submit(0, 0, Intent::Walk(Dir::Right), &c);

    // The same total elapsed time, delivered in two different shapes.
    let uneven = [16u64, 17, 16, 16, 17, 15, 18, 16];
    let mut spent = 0u64;
    for dt in uneven.iter().cycle().take(240) {
      jittery.advance(*dt, &c);
      spent += dt;
    }
    while regular.now_ms() + SIM_STEP_MS <= spent {
      regular.advance(SIM_STEP_MS, &c);
    }

    assert_eq!(jittery.tick(), regular.tick(), "the same elapsed time is the same number of ticks");
    assert_eq!(
      jittery.players[0].cell, regular.players[0].cell,
      "and the same number of ticks is the same world"
    );
  }

  #[test]
  fn the_clock_only_ever_holds_whole_ticks() {
    // What keeps `tick()` exact, and therefore what keeps a client's named tick
    // meaning the same thing on both sides.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    for dt in [7u64, 13, 29, 4, 51, 16, 17] {
      server.advance(dt, &c);
      assert_eq!(server.now_ms() % SIM_STEP_MS, 0, "the clock is always a whole number of ticks");
    }
  }
}
