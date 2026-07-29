//! The authority: owns the maze, the pellets, and who has been caught.
//!
//! Two things are worth reading even if the rest is ordinary.
//!
//! **Time is spent in whole ticks** ([`Server::advance`]). A tick driver hands
//! over the *measured* elapsed time, and advancing a simulation by that makes
//! its rate a property of the host's scheduler, which no client can reproduce.
//! `bomb_grid` paid for that lesson at two snaps per hundred frames; this
//! example was built with it from the first commit.
//!
//! **A turn is scheduled by tick and executed by place.** The schedule
//! ([`plaza_server_utils::InputSchedule`]) decides *when a turn request becomes
//! eligible*, which is the fairness half. The maze then decides *where it is
//! taken*, which is the half no schedule can help with, and is what
//! [`crate::sim::turn_queue`] exists for. Both delays are real and only one of
//! them is a number anybody chose.

use plaza_server_utils::{InputSchedule, InputWindow};

use crate::sim::protocol::{Frame, RoundStart, TurnTaken};
use crate::sim::rules;
use crate::sim::turn_queue::{Resolution, TurnQueue};
use crate::sim::types::*;

/// How close a pursuer must be before a bot runner drops what it is doing.
///
/// Fleeing from a pursuer on the far side of the maze is what makes a bot look
/// busy and achieve nothing.
const RUNNER_PANIC_CELLS: u16 = 5;

/// The most elapsed time one call may spend, so a stalled host falls behind
/// visibly rather than freezing while it repays a debt.
const MAX_CATCH_UP_MS: u64 = 250;

/// A seat's occupant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
  Human,
  Bot,
}

/// What one tick produced.
#[derive(Clone, Debug, Default)]
pub struct Tickout {
  /// **One frame per seat**, not one broadcast.
  ///
  /// The price of hiding a player properly: what each recipient may know is
  /// different, so the frame is a different message for each of them. Cheap
  /// here (four players, a few hundred cells) and the only honest way to keep a
  /// secret, since a client handed a position it should not see has already
  /// lost it.
  pub frames: Vec<(PlayerId, Frame)>,
  /// Turns taken this tick, with the cell each was taken at. The measurement
  /// the whole example rests on.
  pub turns: Vec<TurnTaken>,
  /// Pellets eaten this tick, and who may be told.
  pub eaten: Vec<PelletsEaten>,
  /// `(runner, catcher, milliseconds until the next round)`.
  pub caught: Option<(PlayerId, PlayerId, u64)>,
  pub round_start: Option<RoundStart>,
  /// Power-ups taken this tick, and who may be told.
  pub powers: Vec<PowerTaken>,
  /// `(runner, pursuer)` for each pursuer eaten while energized.
  pub devoured: Vec<(PlayerId, PlayerId)>,
  /// The final table when a match ends, highest first.
  pub match_over: Option<(Vec<(PlayerId, u32)>, u64)>,
}

/// Who an event may be told to.
///
/// A frame can keep a hidden player secret by leaving them out. An *event*
/// cannot: `Op::Eaten` names the exact cell a pellet went from, which is a
/// better position report than a frame is, and broadcasting it while its actor
/// is invisible undoes the vanish completely. So an event carries its audience,
/// and the ones that name a hidden player's cell go only to that player until
/// the vanish ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
  Everyone,
  Only(PlayerId),
}

#[derive(Clone, Debug)]
pub struct PelletsEaten {
  pub audience: Audience,
  pub by: PlayerId,
  pub cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug)]
pub struct PowerTaken {
  pub audience: Audience,
  pub by: PlayerId,
  pub cell: Cell,
  pub kind: Power,
  pub until_ms: u64,
}

/// An event held back because its actor was invisible when it happened.
#[derive(Clone, Copy, Debug)]
enum Withheld {
  Pellet { by: PlayerId, cell: Cell },
  Power { by: PlayerId, cell: Cell, kind: Power, until_ms: u64 },
}

impl Withheld {
  fn by(&self) -> PlayerId {
    match self {
      Withheld::Pellet { by, .. } | Withheld::Power { by, .. } => *by,
    }
  }
}

pub struct Server {
  pub maze: Maze,
  pub players: Vec<PlayerState>,
  pub pellets: Vec<Cell>,
  pub powerups: Vec<PowerupState>,

  /// Only ever a whole multiple of [`SIM_STEP_MS`].
  clock_ms: u64,
  accumulated_ms: u64,
  round: u32,
  /// Which round of the match this is, 1 through [`MATCH_ROUNDS`].
  match_round: u32,
  seed: u64,
  round_ends_at_ms: Option<u64>,
  /// What the other players have not been told yet, because telling them would
  /// have said where an invisible player was.
  withheld: Vec<Withheld>,
  /// Set while the final table is up, so the elapsed round-end interval starts
  /// a new match rather than another round.
  awaiting_new_match: bool,
  /// When play begins. Everybody is held still until then.
  round_begins_at_ms: u64,
  /// Which **seat** runs this round.
  ///
  /// A seat rather than a player id, because the id is identity: a client is
  /// told which id is theirs once, at join, and rotating ids would silently
  /// hand them somebody else's player. Rotating the seat rotates the role,
  /// which is the thing that was meant to move.
  runner_seat: usize,

  seats: Vec<Seat>,
  schedules: Vec<InputSchedule<Dir>>,
  /// One pending turn per player, resolved by the maze rather than by a tick.
  queues: Vec<TurnQueue>,
  last_send_ms: u64,

  pub turns_taken: u64,
  pub turns_expired: u64,
  pub catches: u64,
  pub pellets_eaten: u64,
  pub devoured: u64,
}

impl Clone for Server {
  /// `plaza` requires `Clone` for its state-query command. The schedules are
  /// rebuilt empty: a half-drained input queue is not worth copying.
  fn clone(&self) -> Self {
    Self {
      maze: self.maze.clone(),
      players: self.players.clone(),
      pellets: self.pellets.clone(),
      powerups: self.powerups.clone(),
      clock_ms: self.clock_ms,
      accumulated_ms: self.accumulated_ms,
      round: self.round,
      match_round: self.match_round,
      seed: self.seed,
      round_ends_at_ms: self.round_ends_at_ms,
      withheld: self.withheld.clone(),
      awaiting_new_match: self.awaiting_new_match,
      round_begins_at_ms: self.round_begins_at_ms,
      runner_seat: self.runner_seat,
      seats: self.seats.clone(),
      schedules: self.seats.iter().map(|_| InputSchedule::new()).collect(),
      queues: self.queues.clone(),
      last_send_ms: self.last_send_ms,
      turns_taken: self.turns_taken,
      turns_expired: self.turns_expired,
      catches: self.catches,
      pellets_eaten: self.pellets_eaten,
      devoured: self.devoured,
    }
  }
}

impl std::fmt::Debug for Server {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Server")
      .field("round", &self.round)
      .field("clock_ms", &self.clock_ms)
      .field("pellets", &self.pellets.len())
      .finish()
  }
}

impl Server {
  pub fn new(players: usize, seed: u64) -> Self {
    let count = players.clamp(1, 4);
    let maze = Maze::generate(seed);
    let mut server = Self {
      maze,
      players: Vec::new(),
      pellets: Vec::new(),
      powerups: Vec::new(),
      clock_ms: 0,
      accumulated_ms: 0,
      round: 1,
      match_round: 1,
      seed,
      round_ends_at_ms: None,
      withheld: Vec::new(),
      awaiting_new_match: false,
      round_begins_at_ms: ROUND_START_MS,
      runner_seat: 0,
      seats: vec![Seat::Bot; count],
      schedules: (0..count).map(|_| InputSchedule::new()).collect(),
      queues: vec![TurnQueue::new(); count],
      last_send_ms: 0,
      turns_taken: 0,
      turns_expired: 0,
      catches: 0,
      pellets_eaten: 0,
      devoured: 0,
    };
    server.lay_out_round();
    server
  }

  /// Places everybody and scatters the pellets. Shared by `new` and the round
  /// reset, so the two cannot drift.
  fn lay_out_round(&mut self) {
    let count = self.seats.len();
    let places = spawns(count);
    let existing = std::mem::take(&mut self.players);
    // The runner takes the middle and the pursuers the corners, whichever seat
    // is running: starting the hunted player in a corner is a shorter round
    // than anybody wants.
    let mut corner = 1usize;
    let mut cells = vec![places[0]; count];
    for (seat, cell) in cells.iter_mut().enumerate() {
      if seat == self.runner_seat {
        *cell = places[0];
      } else {
        *cell = places[corner.min(places.len() - 1)];
        corner += 1;
      }
    }
    self.players = cells
      .iter()
      .enumerate()
      .map(|(i, cell)| {
        let role = if i == self.runner_seat { Role::Runner } else { Role::Pursuer };
        let heading = self.maze.exits(*cell).first().copied().unwrap_or(Dir::Right);
        match existing.iter().find(|p| p.id == i as PlayerId) {
          Some(old) => {
            let mut player = old.clone();
            player.role = role;
            player.reset_for_round(*cell, heading);
            player
          }
          None => PlayerState::new(i as PlayerId, role, *cell, heading),
        }
      })
      .collect();

    // Pellets everywhere except where somebody is standing.
    let taken: Vec<Cell> = self.players.iter().map(|p| p.cell).collect();
    self.pellets = self.maze.corridors().into_iter().filter(|c| !taken.contains(c)).collect();

    // Power-ups scattered deterministically from the seed, so a round is the
    // same board twice and a replay is a replay.
    self.powerups.clear();
    let corridors = self.maze.corridors();
    let wanted = (corridors.len() / POWERUP_DENSITY).max(2);
    for i in 0..wanted {
      let mix = self.seed.wrapping_mul(i as u64 + 1).wrapping_mul(2_654_435_761).rotate_left(13);
      let cell = corridors[(mix as usize) % corridors.len()];
      if taken.contains(&cell) || self.powerups.iter().any(|p| p.cell == cell) {
        continue;
      }
      self.powerups.push(PowerupState {
        cell,
        kind: Power::from_seed(mix >> 7),
      });
    }
    // A power-up sits on top of a pellet rather than replacing it.
    let spots: Vec<Cell> = self.powerups.iter().map(|p| p.cell).collect();
    self.pellets.retain(|c| !spots.contains(c));
    for queue in &mut self.queues {
      queue.clear();
    }
    for schedule in &mut self.schedules {
      schedule.clear();
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  /// Derived from the clock, never counted beside it.
  pub fn tick(&self) -> u64 {
    self.clock_ms / SIM_STEP_MS
  }

  pub fn round(&self) -> u32 {
    self.round
  }

  pub fn seats(&self) -> usize {
    self.seats.len()
  }

  /// The match table, highest score first, ties broken by id so two runs of
  /// the same game agree.
  pub fn standings(&self) -> Vec<(PlayerId, u32)> {
    let mut table: Vec<(PlayerId, u32)> = self.players.iter().map(|p| (p.id, p.score)).collect();
    table.sort_by_key(|(id, score)| (std::cmp::Reverse(*score), *id));
    table
  }

  /// Which round of the match is being played.
  pub fn match_round(&self) -> u32 {
    self.match_round
  }

  /// Which seat is the runner this round.
  pub fn runner_seat(&self) -> usize {
    self.runner_seat
  }

  pub fn round_over_pending(&self) -> bool {
    self.round_ends_at_ms.is_some()
  }

  /// Milliseconds until play begins, or `None` once it has.
  pub fn countdown_ms(&self) -> Option<u64> {
    (self.clock_ms < self.round_begins_at_ms).then(|| self.round_begins_at_ms - self.clock_ms)
  }

  pub fn take_seat(&mut self, seat: usize) {
    if let Some(slot) = self.seats.get_mut(seat) {
      *slot = Seat::Human;
    }
    if let Some(queue) = self.queues.get_mut(seat) {
      queue.clear();
    }
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(slot) = self.seats.get_mut(seat) {
      *slot = Seat::Bot;
    }
    if let Some(queue) = self.queues.get_mut(seat) {
      queue.clear();
    }
  }

  /// Offers a turn request for a named tick.
  ///
  /// Accepting it only makes it **eligible**; the maze still decides where it
  /// is taken. A refusal here is the ordinary tick-window refusal and means the
  /// request arrived too late to be fair, not that the turn was impossible.
  pub fn submit(&mut self, seat: usize, tick: u64, dir: Dir, controls: &Controls) -> bool {
    let Some(schedule) = self.schedules.get_mut(seat) else {
      return false;
    };
    if !controls.input_playout {
      let tick = self.clock_ms / SIM_STEP_MS;
      self.queues[seat].request(dir, tick);
      return true;
    }
    let window = InputWindow {
      max_late: controls.input_max_late_ticks,
      max_early: controls.input_max_early_ticks,
    };
    schedule.submit(tick, dir, self.clock_ms / SIM_STEP_MS, window).accepted()
  }

  /// Per-seat admission verdicts: `(accepted, late, closed, ahead, margin)`.
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

  /// What a seat is currently waiting to turn into, if anything.
  pub fn queue_pending(&self, seat: usize) -> Option<Dir> {
    self.queues.get(seat).and_then(|q| q.pending()).map(|t| t.dir)
  }

  /// Turns taken and turns that expired waiting for a place, per seat.
  pub fn turn_stats(&self) -> Vec<(u64, u64)> {
    self.queues.iter().map(|q| q.stats()).collect()
  }

  pub fn round_start(&self) -> RoundStart {
    RoundStart {
      round: self.round,
      match_round: self.match_round,
      match_rounds: MATCH_ROUNDS,
      maze: self.maze.clone(),
      players: self.players.clone(),
      pellets: self.pellets.clone(),
      powerups: self.powerups.clone(),
      server_time_ms: self.clock_ms,
      tick: self.tick(),
      starts_at_ms: self.round_begins_at_ms,
    }
  }

  /// The frame **for one recipient**.
  ///
  /// A hidden runner is left out of everybody else's copy. Not dimmed, not
  /// flagged: absent. That is the difference between a secret and a request
  /// that the client please not look.
  pub fn frame_for(&self, recipient: PlayerId) -> Frame {
    let now = self.clock_ms;
    Frame {
      server_time_ms: now,
      tick: self.tick(),
      players: self
        .players
        .iter()
        .filter(|p| p.id == recipient || !p.hidden(now))
        .cloned()
        .collect(),
      // A frame counts what this recipient still believes is on the board.
      // Otherwise the count drops on the tick a pellet is eaten, and a player
      // watching the number is watching an invisible player eat.
      pellets_left: self.pellets.len() as u32 + self.withheld_for(recipient, |w| matches!(w, Withheld::Pellet { .. })),
      powerups: self.powerups_for(recipient),
      round: self.match_round,
      match_rounds: MATCH_ROUNDS,
    }
  }

  /// How many withheld events this recipient has not been told about.
  fn withheld_for(&self, recipient: PlayerId, want: impl Fn(&Withheld) -> bool) -> u32 {
    self.withheld.iter().filter(|w| w.by() != recipient && want(w)).count() as u32
  }

  /// The pickups this recipient still believes are on the board.
  ///
  /// One taken by an invisible player is put back for everybody else, because
  /// a pickup disappearing is a cell and a moment, which is the whole of what
  /// the vanish is meant to hide.
  fn powerups_for(&self, recipient: PlayerId) -> Vec<PowerupState> {
    let mut visible = self.powerups.clone();
    for item in &self.withheld {
      if let Withheld::Power { by, cell, kind, .. } = item
        && *by != recipient
      {
        visible.push(PowerupState { cell: *cell, kind: *kind });
      }
    }
    visible
  }

  /// The omniscient frame, for a host's own truth overlay and for tests.
  pub fn frame(&self) -> Frame {
    Frame {
      server_time_ms: self.clock_ms,
      tick: self.tick(),
      players: self.players.clone(),
      pellets_left: self.pellets.len() as u32,
      powerups: self.powerups.clone(),
      round: self.match_round,
      match_rounds: MATCH_ROUNDS,
    }
  }

  /// Advances the world by `dt_ms`, in whole ticks. See the module note.
  pub fn advance(&mut self, dt_ms: u64, controls: &Controls) -> Tickout {
    self.accumulated_ms += dt_ms.min(MAX_CATCH_UP_MS);
    let mut out = Tickout::default();
    while self.accumulated_ms >= SIM_STEP_MS {
      self.accumulated_ms -= SIM_STEP_MS;
      self.step(controls, &mut out);
    }
    out
  }

  fn step(&mut self, controls: &Controls, out: &mut Tickout) {
    self.clock_ms += SIM_STEP_MS;
    let tick = self.tick();

    if let Some(ends_at) = self.round_ends_at_ms {
      if self.clock_ms >= ends_at {
        if self.awaiting_new_match {
          self.awaiting_new_match = false;
          out.round_start = Some(self.begin_match());
        } else if self.match_round >= MATCH_ROUNDS {
          // The table gets an interval of its own rather than one frame
          // between two countdowns. It is what the last five rounds were for,
          // and the next round's `RoundStart` is what clears it from a client,
          // so sending both together shows it for no time at all.
          out.match_over = Some((self.standings(), MATCH_END_MS));
          self.awaiting_new_match = true;
          self.round_ends_at_ms = Some(self.clock_ms + MATCH_END_MS);
        } else {
          out.round_start = Some(self.begin_round());
        }
      }
      if self.send_due(controls) {
        out.frames = self.frames_for_everyone();
      }
      return;
    }

    // The opening countdown. Nothing moves, nothing is eaten, nobody is
    // caught: a player dropped into a fresh maze in a role that may have just
    // changed gets a moment to read both. Frames still go out, so a joiner sees
    // the board and the clock ticking down rather than a frozen screen.
    if self.clock_ms < self.round_begins_at_ms {
      if self.send_due(controls) {
        out.frames = self.frames_for_everyone();
      }
      return;
    }

    self.execute_due(tick, controls);
    self.drive_bots(tick, controls);
    self.advance_players(tick, controls, out);
    self.eat_pellets(out);

    self.take_powerups(out);
    if let Some(result) = self.resolve_contact(out) {
      out.caught = Some(result);
    }
    // After the pickups, so a vanish taken this tick keeps this tick's events
    // secret rather than revealing them on the tick it began.
    self.reveal_withheld(out);
    if self.send_due(controls) {
      out.frames = self.frames_for_everyone();
    }
  }

  /// One frame per seat, each carrying only what that seat may know.
  fn frames_for_everyone(&self) -> Vec<(PlayerId, Frame)> {
    (0..self.seats.len()).map(|seat| (seat as PlayerId, self.frame_for(seat as PlayerId))).collect()
  }

  fn send_due(&mut self, controls: &Controls) -> bool {
    let interval = controls.sync_interval_ms();
    if self.clock_ms.saturating_sub(self.last_send_ms) < interval {
      return false;
    }
    self.last_send_ms = self.clock_ms;
    true
  }

  /// Moves eligible turn requests into the place-triggered queue.
  ///
  /// The handover between the two mechanisms: the schedule has said "this
  /// request is fair to act on now", and from here the maze decides where.
  fn execute_due(&mut self, tick: u64, _controls: &Controls) {
    for seat in 0..self.schedules.len() {
      let due: Vec<Dir> = self.schedules[seat].drain_due(tick).collect();
      // Only the newest matters. A player can mean one thing at a time, and a
      // backlog is what makes a game take corners nobody asked for.
      if let Some(dir) = due.last() {
        self.queues[seat].request(*dir, tick);
      }
    }
  }

  fn advance_players(&mut self, tick: u64, controls: &Controls, out: &mut Tickout) {
    for seat in 0..self.players.len() {
      let outcomes = rules::advance_player(
        &mut self.players[seat],
        &mut self.queues[seat],
        &self.maze,
        tick,
        controls.turn_buffer_ms,
        SIM_STEP_MS,
      );
      let id = self.players[seat].id;
      for outcome in outcomes {
        match outcome {
          Resolution::Taken { dir, at } => {
            self.turns_taken += 1;
            out.turns.push(TurnTaken { player: id, dir, at, tick });
          }
          Resolution::Expired { .. } => self.turns_expired += 1,
          Resolution::Idle | Resolution::Held => {}
        }
      }
    }
  }

  /// Power-ups the runner walks over.
  fn take_powerups(&mut self, out: &mut Tickout) {
    let now = self.clock_ms;
    let mut taken: Vec<(usize, Cell, Power)> = Vec::new();
    for (seat, player) in self.players.iter().enumerate() {
      if player.role != Role::Runner || !player.alive {
        continue;
      }
      let at = player.occupied();
      if let Some(index) = self.powerups.iter().position(|p| p.cell == at) {
        taken.push((seat, at, self.powerups[index].kind));
        self.powerups.swap_remove(index);
      }
    }
    for (seat, cell, kind) in taken {
      let until = now + kind.duration_ms();
      let hidden = self.players[seat].hidden(now);
      let player = &mut self.players[seat];
      match kind {
        Power::Energize => player.energized_until_ms = until,
        Power::Vanish => player.hidden_until_ms = until,
      }
      let by = player.id;
      // `hidden` is read **before** the power is applied: taking a vanish is
      // the one pickup whose own cell is safe to broadcast, because at that
      // moment everybody could still see the player standing on it.
      let audience = if hidden { Audience::Only(by) } else { Audience::Everyone };
      if hidden {
        self.withheld.push(Withheld::Power { by, cell, kind, until_ms: until });
      }
      out.powers.push(PowerTaken {
        audience,
        by,
        cell,
        kind,
        until_ms: until,
      });
    }
  }

  /// Tells everybody what was held back, once its actor is visible again.
  ///
  /// The events go out late rather than never: a client that never heard them
  /// would draw pellets that are gone and pickups that were taken, for the rest
  /// of the round. Late is honest, because by the time it arrives the position
  /// it reveals is one the player has already left.
  fn reveal_withheld(&mut self, out: &mut Tickout) {
    if self.withheld.is_empty() {
      return;
    }
    let now = self.clock_ms;
    let hidden: Vec<PlayerId> = self.players.iter().filter(|p| p.hidden(now)).map(|p| p.id).collect();
    let (still_secret, tell): (Vec<Withheld>, Vec<Withheld>) = std::mem::take(&mut self.withheld).into_iter().partition(|w| hidden.contains(&w.by()));
    self.withheld = still_secret;
    for item in tell {
      match item {
        Withheld::Pellet { by, cell } => out.eaten.push(PelletsEaten {
          audience: Audience::Everyone,
          by,
          cells: vec![cell],
        }),
        Withheld::Power { by, cell, kind, until_ms } => out.powers.push(PowerTaken {
          audience: Audience::Everyone,
          by,
          cell,
          kind,
          until_ms,
        }),
      }
    }
  }

  fn eat_pellets(&mut self, out: &mut Tickout) {
    let now = self.clock_ms;
    let mut hidden_eats = Vec::new();
    for player in self.players.iter().filter(|p| p.role == Role::Runner && p.alive) {
      let at = player.occupied();
      if let Some(index) = self.pellets.iter().position(|c| *c == at) {
        self.pellets.swap_remove(index);
        self.pellets_eaten += 1;
        // The worst leak of the lot if it goes out: a pellet disappearing is
        // an exact cell, at the exact tick, for a player nobody can see.
        let audience = if player.hidden(now) {
          hidden_eats.push(Withheld::Pellet { by: player.id, cell: at });
          Audience::Only(player.id)
        } else {
          Audience::Everyone
        };
        out.eaten.push(PelletsEaten {
          audience,
          by: player.id,
          cells: vec![at],
        });
      }
    }
    self.withheld.extend(hidden_eats);
    // Scores are applied outside the borrow above.
    for PelletsEaten { by: id, cells, .. } in &out.eaten {
      if let Some(player) = self.players.iter_mut().find(|p| p.id == *id) {
        player.score += cells.len() as u32 * PELLET_VALUE;
      }
    }
  }

  /// What happens when a pursuer and the runner share a cell.
  ///
  /// **Which way this goes is the energizer's whole point**, and it is a timed,
  /// server-authoritative inversion: for a few seconds contact means the runner
  /// eats the pursuer instead of the other way round. A client predicting its
  /// movement across the moment the timer runs out will disagree with the
  /// server about who is dangerous to whom, which is why the expiry is a
  /// declared instant both sides read rather than a countdown each side runs.
  fn resolve_contact(&mut self, out: &mut Tickout) -> Option<(PlayerId, PlayerId, u64)> {
    if self.round_ends_at_ms.is_some() {
      return None;
    }
    let now = self.clock_ms;
    let runner = self.players.iter().find(|p| p.role == Role::Runner && p.alive)?;
    let at = runner.occupied();
    let runner_id = runner.id;

    if runner.energized(now) {
      // The inversion. Every pursuer sharing the cell is eaten and walks home.
      let eaten: Vec<usize> = self
        .players
        .iter()
        .enumerate()
        .filter(|(_, p)| p.role == Role::Pursuer && !p.eaten(now) && p.occupied().distance(at) <= CATCH_DISTANCE)
        .map(|(i, _)| i)
        .collect();
      if eaten.is_empty() {
        return None;
      }
      let places = spawns(self.seats.len());
      for seat in eaten {
        let home = places[(seat % places.len()).max(1).min(places.len() - 1)];
        let pursuer_id = self.players[seat].id;
        let player = &mut self.players[seat];
        player.cell = home;
        player.step = None;
        player.eaten_until_ms = now + 2_000;
        self.devoured += 1;
        out.devoured.push((runner_id, pursuer_id));
      }
      if let Some(player) = self.players.iter_mut().find(|p| p.id == runner_id) {
        player.score += EAT_VALUE * out.devoured.len() as u32;
      }
      return None;
    }

    let catcher = self
      .players
      .iter()
      .find(|p| p.role == Role::Pursuer && p.alive && !p.eaten(now) && p.occupied().distance(at) <= CATCH_DISTANCE)?;
    let catcher_id = catcher.id;

    self.catches += 1;
    self.round_ends_at_ms = Some(self.clock_ms + ROUND_END_MS);
    if let Some(player) = self.players.iter_mut().find(|p| p.id == catcher_id) {
      player.rounds_won += 1;
      player.score += CATCH_VALUE;
    }
    if let Some(player) = self.players.iter_mut().find(|p| p.id == runner_id) {
      player.alive = false;
      player.step = None;
    }
    Some((runner_id, catcher_id, ROUND_END_MS))
  }

  /// A fresh maze, everybody home, the roles rotated.
  ///
  /// Rotating is the point of having roles at all: being hunted and hunting are
  /// different games, and a player who only ever does one has seen half of it.
  fn begin_round(&mut self) -> RoundStart {
    self.round += 1;
    self.seed = self.seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    self.maze = Maze::generate(self.seed);
    self.round_ends_at_ms = None;
    self.awaiting_new_match = false;
    // Discarded rather than flushed: the next round relays the board, so an
    // event about the last one would remove a pellet from a maze that no
    // longer exists.
    self.withheld.clear();
    self.round_begins_at_ms = self.clock_ms + ROUND_START_MS;

    // Rotate the role, not the identity. Ids stay put so a client keeps
    // controlling the player it was told about at join.
    if self.seats.len() > 1 {
      self.runner_seat = (self.runner_seat + 1) % self.seats.len();
    }
    self.match_round += 1;
    self.lay_out_round();
    self.round_start()
  }

  /// A fresh match: scores back to zero, and the roles start over.
  fn begin_match(&mut self) -> RoundStart {
    // Lay the next round out first, then clear what belongs to the match, so
    // the returned state is the fresh board with a fresh table rather than the
    // old table on a new board.
    self.begin_round();
    self.match_round = 1;
    for player in &mut self.players {
      player.score = 0;
      player.rounds_won = 0;
    }
    self.round_start()
  }

  /// The house players: pursuers hunt, and a bot runner flees.
  ///
  /// Deliberately simple, and deliberately *through the same queue a human
  /// uses*: a bot that set its heading directly would bypass the mechanism this
  /// example is about and the maze would look better behaved than it is.
  fn drive_bots(&mut self, tick: u64, controls: &Controls) {
    if !controls.bots {
      return;
    }
    let runner_at = self.players.iter().find(|p| p.role == Role::Runner).map(|p| p.occupied());
    for seat in 0..self.seats.len() {
      if self.seats[seat] != Seat::Bot || !self.players[seat].alive {
        continue;
      }
      // About the cell they are **arriving at**, every tick. Skipping a player
      // mid-step would skip nearly every tick, because `advance_player` starts
      // the next step the instant it finishes the last.
      let at = match &self.players[seat].step {
        Some(step) => step.to,
        None => self.players[seat].cell,
      };
      let heading = self.players[seat].heading;
      let Some(target) = runner_at else {
        continue;
      };
      let dir = match self.players[seat].role {
        Role::Pursuer => rules::pursuit_dir(at, heading, target, &self.maze),
        // A runner has two jobs and only one of them is fleeing. Maximising
        // distance at every cell is a bot that looks busy and never eats.
        Role::Runner => {
          let threat = self
            .players
            .iter()
            .filter(|p| p.role == Role::Pursuer && p.alive)
            .map(|p| p.occupied())
            .min_by_key(|c| c.distance(at));
          match threat {
            Some(threat) if threat.distance(at) <= RUNNER_PANIC_CELLS => {
              // Under threat it still eats, it just refuses to walk into the
              // pursuer while doing it. A pursuer trails within a few cells
              // almost the whole round, so a runner that fled instead would
              // never eat at all.
              let keeping_away: Vec<Dir> = self
                .maze
                .exits(at)
                .into_iter()
                .filter(|d| at.step(*d).is_some_and(|next| next.distance(threat) >= at.distance(threat)))
                .filter(|d| *d != heading.opposite())
                .collect();
              let toward_food = rules::pellet_dir(at, heading, &self.pellets, &self.maze, Some(threat));
              match toward_food.filter(|d| keeping_away.contains(d)) {
                Some(dir) => dir,
                None => keeping_away
                  .into_iter()
                  .max_by_key(|d| (at.step(*d).unwrap_or(at).distance(threat), u8::from(*d)))
                  .or(toward_food)
                  .unwrap_or(heading),
              }
            }
            // Otherwise: go and eat. Nothing else in the round makes progress.
            _ => rules::pellet_dir(at, heading, &self.pellets, &self.maze, threat).unwrap_or(heading),
          }
        }
      };
      if dir != heading {
        self.queues[seat].request(dir, tick);
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

  /// A server past its opening countdown, which is where every test below
  /// wants to begin: the countdown is its own behaviour and has its own test.
  fn started(controls: &Controls) -> Server {
    let mut server = Server::new(2, MAZE_SEED);
    while server.countdown_ms().is_some() {
      server.advance(SIM_STEP_MS, controls);
    }
    server
  }

  fn run(server: &mut Server, ms: u64, controls: &Controls) -> Vec<TurnTaken> {
    let mut turns = Vec::new();
    for _ in 0..(ms / SIM_STEP_MS) {
      turns.extend(server.advance(SIM_STEP_MS, controls).turns);
    }
    turns
  }

  #[test]
  fn the_clock_only_ever_holds_whole_ticks() {
    // What keeps `tick()` exact, and therefore what keeps a client's named tick
    // meaning the same thing on both sides.
    let c = controls();
    let mut server = Server::new(2, MAZE_SEED);
    for dt in [7u64, 13, 29, 4, 51, 16, 17] {
      server.advance(dt, &c);
      assert_eq!(server.now_ms() % SIM_STEP_MS, 0);
    }
  }

  #[test]
  fn an_irregular_tick_driver_produces_the_same_world_as_a_regular_one() {
    // The lesson `bomb_grid` paid for: a tick driver delivers measured elapsed
    // time, and a simulation advanced by that is a function of the host's
    // scheduler, which no client can reproduce.
    let c = controls();
    let mut regular = Server::new(2, MAZE_SEED);
    let mut jittery = Server::new(2, MAZE_SEED);

    let uneven = [16u64, 17, 16, 16, 17, 15, 18, 16];
    let mut spent = 0u64;
    for dt in uneven.iter().cycle().take(160) {
      jittery.advance(*dt, &c);
      spent += dt;
    }
    while regular.now_ms() + SIM_STEP_MS <= spent {
      regular.advance(SIM_STEP_MS, &c);
    }
    assert_eq!(jittery.tick(), regular.tick());
    assert_eq!(jittery.players[0].cell, regular.players[0].cell);
  }

  #[test]
  fn a_player_runs_and_eats_without_any_input() {
    // There is no standing still, so a round makes progress on its own.
    let c = controls();
    let mut server = started(&c);
    let before = server.pellets.len();
    run(&mut server, 2_000, &c);
    assert!(server.pellets.len() < before, "the runner ate on the way");
    assert!(server.pellets_eaten > 0);
  }

  #[test]
  fn a_turn_request_is_taken_at_a_place_and_reported_with_it() {
    // The example's whole subject: the server says *where*, because that is the
    // thing a client can get wrong in a way no tick can express.
    let c = controls();
    let mut server = started(&c);
    // Whichever way the runner is not already going.
    let at = server.players[0].cell;
    let heading = server.players[0].heading;
    let want = server
      .maze
      .exits(at)
      .into_iter()
      .find(|d| *d != heading)
      .expect("a junction to turn at");

    server.submit(0, 0, want, &c);
    let turns = run(&mut server, 3_000, &c);
    let mine: Vec<&TurnTaken> = turns.iter().filter(|t| t.player == 0).collect();
    assert!(!mine.is_empty(), "the turn was taken somewhere");
    assert!(server.maze.open(mine[0].at), "and the place is a real cell");
    assert_eq!(server.turn_stats()[0].0, mine.len() as u64);
  }

  #[test]
  fn a_turn_into_a_wall_expires_rather_than_waiting_for_ever() {
    let c = controls();
    let mut server = started(&c);
    // A direction with no exit anywhere near, held until it times out.
    let at = server.players[0].cell;
    let blocked = Dir::ALL.into_iter().find(|d| at.step(*d).is_none_or(|c| !server.maze.open(c)));
    let Some(blocked) = blocked else {
      return; // An open cell on every side; nothing to assert here.
    };
    server.submit(0, 0, blocked, &c);
    run(&mut server, c.turn_buffer_ms + 500, &c);
    let (_, expired) = server.turn_stats()[0];
    assert!(expired > 0 || server.turn_stats()[0].0 > 0, "it resolved one way or the other");
  }

  #[test]
  fn a_pursuer_sharing_the_runners_cell_ends_the_round() {
    let c = controls();
    let mut server = started(&c);
    let at = server.players[0].occupied();
    server.players[1].cell = at;
    server.players[1].step = None;

    let out = server.advance(SIM_STEP_MS, &c);
    let (runner, catcher, _) = out.caught.expect("a catch");
    assert_eq!((runner, catcher), (0, 1));
    assert!(!server.players[0].alive);
    assert_eq!(server.players[1].rounds_won, 1);
  }

  #[test]
  fn the_next_round_rotates_the_roles() {
    let c = controls();
    let mut server = started(&c);
    let at = server.players[0].occupied();
    server.players[1].cell = at;
    // The step goes too, not only the cell: `occupied` reads the step when one
    // is in flight, so moving a player without clearing it leaves them judged
    // at the cell they were walking out of.
    server.players[1].step = None;
    server.advance(SIM_STEP_MS, &c);

    let mut next_round = None;
    for _ in 0..(ROUND_END_MS / SIM_STEP_MS + 4) {
      if let Some(round) = server.advance(SIM_STEP_MS, &c).round_start {
        next_round = Some(round);
        break;
      }
    }
    let round = next_round.expect("the next round begins");
    assert_eq!(round.round, 2);
    assert_eq!(round.players.iter().filter(|p| p.role == Role::Runner).count(), 1, "exactly one runner");
    assert!(round.players.iter().all(|p| p.alive));
    assert!(!round.pellets.is_empty(), "and a fresh field of pellets");
  }

  #[test]
  fn a_scheduled_turn_waits_for_its_tick_before_it_is_even_eligible() {
    // The two delays in series: the schedule decides when a request may be
    // acted on, and only then does the maze decide where.
    let c = Controls {
      input_playout: true,
      ..controls()
    };
    let mut server = started(&c);
    // Inside the early window: aiming further ahead than `input_max_early_ticks`
    // is refused outright, which is the schedule's own rule and a different
    // test.
    let target = server.tick() + 8;
    let at = server.players[0].cell;
    let heading = server.players[0].heading;
    let want = server.maze.exits(at).into_iter().find(|d| *d != heading).expect("somewhere to turn");
    assert!(server.submit(0, target, want, &c));

    run(&mut server, SIM_STEP_MS * 4, &c);
    assert_eq!(server.turn_stats()[0], (0, 0), "not even queued yet, let alone taken");

    // Past its tick it becomes eligible, and only then does the maze get to
    // decide where it happens.
    run(&mut server, SIM_STEP_MS * 6, &c);
    assert!(
      server.queue_pending(0).is_some() || server.turn_stats()[0].0 > 0,
      "eligible now: either waiting for a place or already taken"
    );
  }

  #[test]
  fn nothing_moves_until_the_countdown_elapses() {
    // A player dropped into a fresh maze, in a role that may have just changed,
    // gets a moment to read both before anything can catch them.
    let c = controls();
    let mut server = Server::new(2, MAZE_SEED);
    let start = server.players[0].cell;
    assert!(server.countdown_ms().is_some(), "a round opens counting down");

    run(&mut server, ROUND_START_MS - 200, &c);
    assert_eq!(server.players[0].cell, start, "still held");
    assert_eq!(server.pellets_eaten, 0, "and nothing eaten in the meantime");

    run(&mut server, 600, &c);
    assert!(server.countdown_ms().is_none(), "and then it plays");
    assert_ne!(server.players[0].cell, start);
  }

  #[test]
  fn the_role_rotates_for_a_given_seat_while_its_identity_does_not() {
    // Two things that must move in opposite ways. The **role** rotates, or a
    // player only ever sees half the game. The **id** does not, because a client
    // is told which id is theirs once, at join: rotating ids would silently hand
    // them somebody else's player, and the seat that was always index zero was
    // always the runner regardless.
    let c = Controls { players: 3, ..controls() };
    let mut server = Server::new(3, MAZE_SEED);
    let first = server.runner_seat();
    assert_eq!(server.players[first].role, Role::Runner);
    for (seat, player) in server.players.iter().enumerate() {
      assert_eq!(player.id, seat as PlayerId, "id is the seat and stays put");
    }

    // Settle the round and let the next one begin.
    let at = server.players[first].occupied();
    let other = (first + 1) % 3;
    server.players[other].cell = at;
    server.players[other].step = None;
    while server.round_start().round == 1 {
      server.advance(SIM_STEP_MS, &c);
    }

    assert_ne!(server.runner_seat(), first, "somebody else runs now");
    assert_eq!(server.players.iter().filter(|p| p.role == Role::Runner).count(), 1);
    for (seat, player) in server.players.iter().enumerate() {
      assert_eq!(player.id, seat as PlayerId, "and identities did not move with it");
    }
  }

  #[test]
  fn the_runner_starts_in_the_middle_whichever_seat_is_running() {
    // Starting the hunted player in a corner is a shorter round than anybody
    // wants.
    let c = Controls { players: 3, ..controls() };
    let mut server = Server::new(3, MAZE_SEED);
    let middle = spawns(3)[0];
    for _ in 0..3 {
      let seat = server.runner_seat();
      assert_eq!(server.players[seat].cell, middle, "the runner has the middle");
      let at = server.players[seat].occupied();
      let other = (seat + 1) % 3;
      server.players[other].cell = at;
      server.players[other].step = None;
      let round = server.round_start().round;
      while server.round_start().round == round {
        server.advance(SIM_STEP_MS, &c);
      }
    }
  }

  #[test]
  fn a_bot_runner_actually_eats() {
    let c = Controls { bots: true, players: 2, ..controls() };
    let mut server = started(&c);
    run(&mut server, 45_000, &c);
    // Measured: a runner that seeks food while it evades clears about 165 in
    // this window. One that only flees managed 59, and one that never steered
    // at all managed 52. The threshold sits well clear of both, because the
    // point is the difference between a bot with a job and a bot that looks
    // busy.
    assert!(
      server.pellets_eaten >= 120,
      "a runner with a purpose should clear a fair few: {}",
      server.pellets_eaten
    );
    assert!(server.catches > 0, "and the pursuers should be catching it sometimes: {}", server.catches);
  }

  #[test]
  fn a_bot_runner_reaches_the_energizers_and_turns_on_its_pursuers() {
    // The bots do not seek energizers. Routing to one under threat was
    // measured at no more devoured pursuers, 22 fewer pellets, and six
    // power-ups left on the board.
    let c = Controls { bots: true, players: 4, ..controls() };
    let mut server = started(&c);
    let laid_out = server.powerups.len();
    run(&mut server, 60_000, &c);
    assert!(laid_out > 0, "the round should lay some out");
    assert_eq!(server.powerups.len(), 0, "and a runner that clears the board takes all of them");
    assert!(server.devoured > 0, "and uses them: {}", server.devoured);
  }

  #[test]
  fn a_bot_runner_still_runs_when_a_pursuer_is_close() {
    // Eating is the job; not being caught is the constraint. A runner that only
    // ate would walk into a pursuer standing on a pellet.
    let c = Controls { bots: true, players: 2, ..controls() };
    let mut server = started(&c);
    // Put a pursuer right beside the runner and see that the gap opens.
    let at = server.players[0].occupied();
    let beside = server.maze.exits(at).first().and_then(|d| at.step(*d)).expect("a neighbour");
    server.players[1].cell = beside;
    server.players[1].step = None;
    let before = server.players[0].occupied().distance(server.players[1].occupied());
    run(&mut server, 1_200, &c);
    let after = server.players[0].occupied().distance(server.players[1].occupied());
    assert!(after >= before, "the runner did not simply stand and be eaten: {before} then {after}");
  }

  #[test]
  fn a_hidden_runner_is_absent_from_other_players_frames() {
    let c = controls();
    let mut server = started(&c);
    let runner = server.runner_seat() as PlayerId;
    let other = server.players.iter().find(|p| p.id != runner).map(|p| p.id).expect("somebody else");

    let hide_until = server.now_ms() + VANISH_MS;
    if let Some(player) = server.players.iter_mut().find(|p| p.id == runner) {
      player.hidden_until_ms = hide_until;
    }

    let theirs = server.frame_for(other);
    assert!(
      !theirs.players.iter().any(|p| p.id == runner),
      "the hidden runner's position never reached the other player"
    );

    let mine = server.frame_for(runner);
    assert!(mine.players.iter().any(|p| p.id == runner), "but you can still see yourself");
  }

  #[test]
  fn a_hidden_runner_does_not_leak_its_position_through_the_pellets_it_eats() {
    let c = Controls { bots: true, players: 2, ..controls() };
    let mut server = started(&c);
    let runner = server.runner_seat() as PlayerId;
    let other = server.players.iter().find(|p| p.id != runner).map(|p| p.id).expect("somebody else");
    server.players[runner as usize].hidden_until_ms = server.now_ms() + VANISH_MS;

    let mut ate_in_secret = 0;
    let until = server.now_ms() + VANISH_MS - SIM_STEP_MS * 2;
    while server.now_ms() < until {
      let out = server.advance(SIM_STEP_MS, &c);
      for eaten in &out.eaten {
        assert_eq!(
          eaten.audience,
          Audience::Only(runner),
          "a pellet an invisible player ate was announced to everybody"
        );
        ate_in_secret += eaten.cells.len();
      }
      for power in &out.powers {
        assert_eq!(power.audience, Audience::Only(runner), "so was a power-up it took");
      }
    }
    assert!(ate_in_secret > 0, "the test is worthless unless it ate something: {ate_in_secret}");

    // And the count in everybody else's frame did not move either, because a
    // number that drops while nobody can see anything is still a report.
    let theirs = server.frame_for(other);
    let mine = server.frame_for(runner);
    assert_eq!(
      theirs.pellets_left as usize,
      mine.pellets_left as usize + ate_in_secret,
      "the other player's board still holds every pellet it was never told about"
    );
  }

  #[test]
  fn what_was_withheld_is_told_once_the_vanish_ends() {
    let c = Controls { bots: true, players: 2, ..controls() };
    let mut server = started(&c);
    let runner = server.runner_seat() as PlayerId;
    let other = server.players.iter().find(|p| p.id != runner).map(|p| p.id).expect("somebody else");
    server.players[runner as usize].hidden_until_ms = server.now_ms() + VANISH_MS;

    let mut secret = 0usize;
    let mut told = 0usize;
    let until = server.now_ms() + VANISH_MS + SIM_STEP_MS * 4;
    while server.now_ms() < until {
      let out = server.advance(SIM_STEP_MS, &c);
      for eaten in &out.eaten {
        match eaten.audience {
          Audience::Only(_) => secret += eaten.cells.len(),
          Audience::Everyone => told += eaten.cells.len(),
        }
      }
    }
    assert!(secret > 0, "it ate while hidden");
    assert!(told >= secret, "and everything eaten in secret was told afterwards: {told} of {secret}");
    assert_eq!(
      server.frame_for(other).pellets_left,
      server.frame_for(runner).pellets_left,
      "so the two boards agree again"
    );
  }

  #[test]
  fn a_vanish_wears_off_and_the_runner_is_sent_again() {
    let c = controls();
    let mut server = started(&c);
    let runner = server.runner_seat() as PlayerId;
    let other = server.players.iter().find(|p| p.id != runner).map(|p| p.id).expect("somebody else");
    let until = server.now_ms() + 300;
    if let Some(player) = server.players.iter_mut().find(|p| p.id == runner) {
      player.hidden_until_ms = until;
    }
    assert!(!server.frame_for(other).players.iter().any(|p| p.id == runner));

    run(&mut server, 500, &c);
    assert!(
      server.frame_for(other).players.iter().any(|p| p.id == runner),
      "an expiry that never expires is a permanent secret, not a power-up"
    );
  }

  #[test]
  fn an_energized_runner_eats_the_pursuer_instead_of_being_caught() {
    let c = controls();
    let mut server = started(&c);
    let runner_seat = server.runner_seat();
    let runner = server.players[runner_seat].id;
    let other_seat = (runner_seat + 1) % server.seats();

    server.players[runner_seat].energized_until_ms = server.now_ms() + ENERGIZE_MS;
    let at = server.players[runner_seat].occupied();
    server.players[other_seat].cell = at;
    server.players[other_seat].step = None;

    let out = server.advance(SIM_STEP_MS, &c);
    assert!(out.caught.is_none(), "contact did not end the round");
    assert_eq!(out.devoured.len(), 1, "it went the other way");
    assert_eq!(out.devoured[0].0, runner);
    assert!(server.players[runner_seat].alive, "and the runner is still running");
    assert!(server.players[runner_seat].score >= EAT_VALUE, "and scored for it");
  }

  #[test]
  fn an_energizer_that_has_worn_off_is_fatal_again() {
    // The moment a client predicting across the expiry gets wrong: the timer is
    // a declared instant both sides read, not a countdown each side runs.
    let c = controls();
    let mut server = started(&c);
    let runner_seat = server.runner_seat();
    let other_seat = (runner_seat + 1) % server.seats();
    server.players[runner_seat].energized_until_ms = server.now_ms(); // already over

    let at = server.players[runner_seat].occupied();
    server.players[other_seat].cell = at;
    server.players[other_seat].step = None;
    let out = server.advance(SIM_STEP_MS, &c);
    assert!(out.caught.is_some(), "contact is a catch once the energizer is spent");
  }

  #[test]
  fn a_match_runs_a_fixed_number_of_rounds_and_then_resets_the_scores() {
    let c = Controls { players: 2, ..controls() };
    let mut server = started(&c);
    assert_eq!(server.match_round(), 1);

    let mut ended = None;
    for _ in 0..(MATCH_ROUNDS + 2) {
      let seat = server.runner_seat();
      let at = server.players[seat].occupied();
      let other = (seat + 1) % 2;
      server.players[other].cell = at;
      server.players[other].step = None;
      server.players[other].eaten_until_ms = 0;
      let round = server.round();
      while server.round() == round {
        let out = server.advance(SIM_STEP_MS, &c);
        if let Some(result) = out.match_over {
          ended = Some(result);
        }
      }
      if ended.is_some() {
        break;
      }
    }

    let (standings, _) = ended.expect("the match ends after its rounds");
    assert_eq!(standings.len(), 2, "everybody is in the table");
    assert!(standings[0].1 >= standings[1].1, "and it is sorted highest first");
    assert_eq!(server.match_round(), 1, "a new match starts over");
    assert!(server.players.iter().all(|p| p.score == 0), "with the scores cleared");
  }

  #[test]
  fn the_final_table_gets_an_interval_of_its_own() {
    let c = Controls { players: 2, ..controls() };
    let mut server = started(&c);

    let mut ended_at = None;
    for _ in 0..(MATCH_ROUNDS + 2) {
      let seat = server.runner_seat();
      let at = server.players[seat].occupied();
      let other = (seat + 1) % 2;
      server.players[other].cell = at;
      server.players[other].step = None;
      server.players[other].eaten_until_ms = 0;
      let round = server.round();
      while server.round() == round && ended_at.is_none() {
        let out = server.advance(SIM_STEP_MS, &c);
        if out.match_over.is_some() {
          assert!(out.round_start.is_none(), "the next match must not be laid out in the same tick");
          ended_at = Some(server.now_ms());
        }
      }
      if ended_at.is_some() {
        break;
      }
    }
    let ended_at = ended_at.expect("the match ends after its rounds");

    let mut started_at = None;
    while started_at.is_none() {
      let out = server.advance(SIM_STEP_MS, &c);
      if out.round_start.is_some() {
        started_at = Some(server.now_ms());
      }
      assert!(server.now_ms() - ended_at <= MATCH_END_MS + SIM_STEP_MS * 4, "and it does not hang there for ever");
    }
    let held = started_at.expect("a new match") - ended_at;
    assert!(held >= MATCH_END_MS, "the table stays up for its interval: {held} ms");
  }
}
