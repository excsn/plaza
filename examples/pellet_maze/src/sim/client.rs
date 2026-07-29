//! What a client believes, and how it discovers it turned at the wrong corner.
//!
//! The prediction machinery is `bomb_grid`'s, arrived at the hard way and
//! reused deliberately: run on the server's tick grid, never on the frame's;
//! compare against what this client believed *on the frame's own tick*; keep an
//! input until the prediction has run it, not until it is acknowledged. Those
//! are not restated here beyond the code that implements them.
//!
//! What is new is the failure this example exists to show.
//!
//! # A cell error is bounded. A junction error is not.
//!
//! When a mispredicted *cell* is corrected, the player jumps one cell and
//! everything afterwards is the same. When a *turn* is taken at the wrong
//! junction, the two sides run down **different corridors**, and every tick
//! after that increases the distance. The correction, when it arrives, is not a
//! step: it is a route.
//!
//! That is why [`Client::wrong_junction`] is counted separately from
//! [`Client::snaps`]. They are different failures with different costs, and a
//! single "corrections" number would average one into the other and hide the
//! expensive one.

use std::collections::VecDeque;

use crate::sim::protocol::{Frame, RoundStart, TurnTaken};
use crate::sim::rules;
use crate::sim::turn_queue::{Resolution, TurnQueue};
use crate::sim::types::*;

/// Ticks of predicted history kept, for comparing a frame at its own tick.
const HISTORY: usize = 64;
/// The most ticks the prediction catches up in one call, so a lurching clock
/// cannot stall the renderer.
const CATCH_UP_TICKS: u64 = 64;
/// How many of this client's own turns are remembered while waiting for the
/// server to say where it took them.
const TURN_MEMORY: usize = 8;

/// One turn request sent and not yet run locally.
#[derive(Clone, Copy, Debug)]
struct Pending {
  seq: u64,
  tick: u64,
  dir: Dir,
  applied: bool,
}

/// A turn this client took, waiting to be compared against the server's.
#[derive(Clone, Copy, Debug)]
struct PredictedTurn {
  dir: Dir,
  at: Cell,
  tick: u64,
}

pub struct Client {
  pub me: PlayerId,
  pub maze: Maze,
  pub players: Vec<PlayerState>,
  pub pellets: Vec<Cell>,
  pub pellets_left: u32,
  pub powerups: Vec<PowerupState>,
  /// Which round of the match this is, and how many there are.
  pub round: u32,
  pub match_rounds: u32,

  predicted: PlayerState,
  queue: TurnQueue,
  predict: bool,
  paused: bool,
  pending: VecDeque<Pending>,
  history: VecDeque<(u64, Cell)>,
  next_tick: u64,
  /// Turns this client has taken and the server has not yet confirmed a place
  /// for. Oldest first.
  predicted_turns: VecDeque<PredictedTurn>,
  /// Server reports waiting for this client to reach the tick they describe.
  ///
  /// A report can arrive **before** the client has simulated that tick: on a
  /// fast link it overtakes the local prediction. Comparing then matches the
  /// server's turn against some older turn of the client's and manufactures a
  /// disagreement out of arrival order. Same rule as a frame: no opinion about
  /// a tick not yet reached.
  incoming_turns: VecDeque<TurnTaken>,

  server_now_ms: u64,
  render_delay_ms: u64,
  turn_buffer_ms: u64,
  /// The server instant play begins. Until then this client holds still for
  /// exactly the reason the server does, derived from the same declared number
  /// rather than from a countdown of its own: two countdowns started at
  /// different moments would end at different ones.
  starts_at_ms: u64,

  /// A cell correction: bounded, one jump, then over.
  pub snaps: u64,
  pub snapped_cells: u64,
  /// A turn taken somewhere the server did not take it. **Unbounded**: the two
  /// sides are now in different corridors and the gap grows until a frame
  /// arrives to settle it.
  pub wrong_junction: u64,
  /// The worst distance between where this client turned and where the server
  /// did, in cells. One junction wrong by one cell is a very different mistake
  /// from one wrong by five.
  pub worst_junction_error: u16,
  /// Turns predicted at all: the denominator.
  pub predicted_turns_total: u64,
  /// The last disagreement, as `(where this client turned, where the server
  /// did)`, so a renderer can mark both. A counter alone climbs with nothing on
  /// screen to connect it to.
  pub last_wrong_junction: Option<(Cell, Cell)>,
  pub frames_seen: u64,
  pub unreached_frames: u64,
  newest_frame_tick: u64,
  pub rounds_seen: u32,
}

impl Client {
  pub fn new(me: PlayerId) -> Self {
    Self {
      me,
      maze: Maze::default(),
      players: Vec::new(),
      pellets: Vec::new(),
      pellets_left: 0,
      powerups: Vec::new(),
      round: 1,
      match_rounds: MATCH_ROUNDS,
      predicted: PlayerState::new(me, Role::Runner, Cell::new(1, 1), Dir::Right),
      queue: TurnQueue::new(),
      predict: true,
      paused: false,
      pending: VecDeque::new(),
      history: VecDeque::new(),
      next_tick: 0,
      predicted_turns: VecDeque::new(),
      incoming_turns: VecDeque::new(),
      server_now_ms: 0,
      render_delay_ms: 140,
      turn_buffer_ms: TURN_BUFFER_MS,
      starts_at_ms: 0,
      snaps: 0,
      snapped_cells: 0,
      wrong_junction: 0,
      worst_junction_error: 0,
      predicted_turns_total: 0,
      last_wrong_junction: None,
      frames_seen: 0,
      unreached_frames: 0,
      newest_frame_tick: 0,
      rounds_seen: 0,
    }
  }

  pub fn ready(&self) -> bool {
    !self.players.is_empty()
  }

  pub fn set_paused(&mut self, paused: bool) {
    self.paused = paused;
  }

  pub fn is_paused(&self) -> bool {
    self.paused
  }

  /// Milliseconds until play begins, or `None` once it has.
  pub fn countdown_ms(&self) -> Option<u64> {
    (self.server_now_ms < self.starts_at_ms).then(|| self.starts_at_ms - self.server_now_ms)
  }

  pub fn render_at_ms(&self) -> u64 {
    self.server_now_ms.saturating_sub(self.render_delay_ms)
  }

  /// What this client is currently waiting to turn into, for the panel: the
  /// one piece of state a player cannot otherwise see, and the one that
  /// explains why nothing has happened yet.
  pub fn queued_turn(&self) -> Option<Dir> {
    self.queue.pending().map(|t| t.dir)
  }

  pub fn turn_stats(&self) -> (u64, u64) {
    self.queue.stats()
  }

  pub fn my_player(&self) -> &PlayerState {
    if self.predict {
      &self.predicted
    } else {
      self.players.iter().find(|p| p.id == self.me).unwrap_or(&self.predicted)
    }
  }

  /// How far ahead of the newest frame this client's simulation is running.
  pub fn tick_lead(&self) -> i64 {
    self.next_tick.saturating_sub(1) as i64 - self.newest_frame_tick as i64
  }

  pub fn on_round(&mut self, round: &RoundStart) {
    self.maze = round.maze.clone();
    self.players = round.players.clone();
    self.pellets = round.pellets.clone();
    self.pellets_left = round.pellets.len() as u32;
    self.powerups = round.powerups.clone();
    self.round = round.match_round;
    self.match_rounds = round.match_rounds;
    self.pending.clear();
    self.history.clear();
    self.predicted_turns.clear();
    self.incoming_turns.clear();
    self.queue.clear();
    self.paused = false;
    self.server_now_ms = round.server_time_ms;
    self.starts_at_ms = round.starts_at_ms;
    // `round.tick` is the tick the state is *at*, so it has already been
    // simulated: the next one to run is the one after. Off by one here means
    // the client runs one extra tick at every round start, which is invisible
    // for a player standing still and is immediately a whole cell for one who
    // is always running.
    self.next_tick = round.tick + 1;
    if let Some(mine) = round.players.iter().find(|p| p.id == self.me) {
      self.predicted = mine.clone();
    }
    self.rounds_seen += 1;
  }

  pub fn set_render_delay(&mut self, ms: u64) {
    self.render_delay_ms = ms;
  }

  pub fn set_turn_buffer(&mut self, ms: u64) {
    self.turn_buffer_ms = ms;
  }

  /// Records a turn request, scheduled for the tick it named.
  ///
  /// Scheduled rather than applied, for the reason `bomb_grid` found: the
  /// server runs a tick-addressed input on the tick it named, so a client that
  /// ran it the instant the key went down would be a playout depth ahead of the
  /// server on every single input.
  pub fn schedule_turn(&mut self, seq: u64, tick: u64, dir: Dir) {
    self.pending.push_back(Pending {
      seq,
      tick,
      dir,
      applied: false,
    });
    while self.pending.len() > 128 {
      self.pending.pop_front();
    }
  }

  /// Advances the prediction to whatever tick the clock says it is.
  pub fn tick(&mut self, server_now_ms: u64, controls: &Controls) {
    self.server_now_ms = server_now_ms;
    self.predict = controls.predict_local;
    self.render_delay_ms = controls.render_delay_ms;
    self.turn_buffer_ms = controls.turn_buffer_ms;

    let target = server_now_ms / SIM_STEP_MS;
    if target.saturating_sub(self.next_tick) > CATCH_UP_TICKS {
      self.next_tick = target.saturating_sub(CATCH_UP_TICKS);
    }
    // Held for the opening countdown, on the server's declared instant.
    let counting_down = server_now_ms < self.starts_at_ms;
    if self.predict && !self.paused && !counting_down {
      while self.next_tick <= target {
        let tick = self.next_tick;
        self.next_tick += 1;
        self.step_once(tick);
      }
    } else {
      self.next_tick = target + 1;
    }

    self.compare_turns();

    match self.history.back_mut() {
      Some((t, cell)) if *t == target => *cell = self.predicted.cell,
      _ => self.history.push_back((target, self.predicted.cell)),
    }
    while self.history.len() > HISTORY {
      self.history.pop_front();
    }
  }

  /// One tick: this tick's requests, then one step of the shared rule.
  fn step_once(&mut self, tick: u64) {
    let mut due: Vec<Pending> = Vec::new();
    for input in self.pending.iter_mut() {
      if !input.applied && input.tick <= tick {
        input.applied = true;
        due.push(*input);
      }
    }
    due.sort_by_key(|p| p.tick);
    // Only the newest, matching the server: a backlog of turns is what makes a
    // game take corners nobody asked for.
    if let Some(last) = due.last() {
      self.queue.request(last.dir, tick);
    }

    let outcomes = rules::advance_player(&mut self.predicted, &mut self.queue, &self.maze, tick, self.turn_buffer_ms, SIM_STEP_MS);
    for outcome in outcomes {
      if let Resolution::Taken { dir, at } = outcome {
        self.predicted_turns_total += 1;
        self.predicted_turns.push_back(PredictedTurn { dir, at, tick });
        while self.predicted_turns.len() > TURN_MEMORY {
          self.predicted_turns.pop_front();
        }
      }
    }
  }

  /// The server's word on where a turn actually happened.
  ///
  /// The measurement this example is built on. A client cannot detect a
  /// wrong-junction turn on its own: its heading is right, its cell is right
  /// for a while, and by the time the positions disagree the cause is several
  /// cells back. Only the server knows where it took the turn, so only the
  /// server can say.
  pub fn on_turn_taken(&mut self, taken: &TurnTaken) {
    if taken.player != self.me {
      return;
    }
    self.incoming_turns.push_back(*taken);
    while self.incoming_turns.len() > TURN_MEMORY * 2 {
      self.incoming_turns.pop_front();
    }
    self.compare_turns();
  }

  /// Compares every report whose tick this client has now simulated.
  fn compare_turns(&mut self) {
    let reached = self.next_tick.saturating_sub(1);
    while self.incoming_turns.front().is_some_and(|t| t.tick <= reached) {
      let taken = self.incoming_turns.pop_front().expect("just checked");
      self.compare_one(&taken);
    }
  }

  fn compare_one(&mut self, taken: &TurnTaken) {
    // Matched by direction rather than by index: a turn the client predicted
    // and the server refused (or vice versa) would otherwise shift every
    // comparison after it by one and report a run of failures from one.
    let Some(index) = self
      .predicted_turns
      .iter()
      .enumerate()
      .filter(|(_, t)| t.dir == taken.dir)
      .min_by_key(|(_, t)| t.tick.abs_diff(taken.tick))
      .map(|(i, _)| i)
    else {
      return;
    };
    let mine = self.predicted_turns.remove(index).expect("just found");
    // Everything older than the matched turn is unmatchable now, so it goes
    // rather than accumulating into a false match later.
    while self.predicted_turns.len() > TURN_MEMORY - 1 {
      self.predicted_turns.pop_front();
    }
    if mine.at != taken.at {
      self.wrong_junction += 1;
      self.worst_junction_error = self.worst_junction_error.max(mine.at.distance(taken.at));
      self.last_wrong_junction = Some((mine.at, taken.at));
    }
  }

  pub fn on_eaten(&mut self, cells: &[Cell]) {
    self.pellets.retain(|c| !cells.contains(c));
  }

  /// A power-up was taken. Removed at once rather than on the next frame, for
  /// the same reason a blast clears a wall at once in `bomb_grid`: it is an
  /// input to what this client predicts, and holding a pickup the server has
  /// already given away means predicting against a board that no longer exists.
  pub fn on_power_taken(&mut self, cell: Cell) {
    self.powerups.retain(|p| p.cell != cell);
  }

  /// Folds in one authoritative frame.
  pub fn on_frame(&mut self, frame: &Frame, controls: &Controls) {
    self.frames_seen += 1;
    self.players = frame.players.clone();
    self.pellets_left = frame.pellets_left;
    self.powerups = frame.powerups.clone();
    self.round = frame.round;
    self.match_rounds = frame.match_rounds;
    self.newest_frame_tick = self.newest_frame_tick.max(frame.tick);

    let Some(authoritative) = frame.players.iter().find(|p| p.id == self.me).cloned() else {
      return;
    };
    if !controls.predict_local {
      self.predicted = authoritative;
      return;
    }

    let reached = self.next_tick.saturating_sub(1);
    if frame.tick > reached {
      // No opinion about a tick this client has not simulated: comparing anyway
      // reports its own lag as a misprediction.
      self.unreached_frames += 1;
      return;
    }
    let believed = self.believed_at_tick(frame.tick);
    let died = self.predicted.alive && !authoritative.alive;

    if believed.is_some_and(|cell| cell != authoritative.cell) && !died {
      let jump = believed.map_or(1, |cell| cell.distance(authoritative.cell)).max(1);
      self.snaps += 1;
      self.snapped_cells += jump as u64;
      self.resync(authoritative, frame.tick);
    } else {
      self.predicted.alive = authoritative.alive;
      self.predicted.role = authoritative.role;
      self.predicted.score = authoritative.score;
      self.predicted.rounds_won = authoritative.rounds_won;
      if died {
        self.predicted.cell = authoritative.cell;
        self.predicted.step = None;
      }
    }
  }

  /// Adopts an authoritative state and re-runs the tick loop over it.
  ///
  /// **The queue is cleared rather than replayed**, and that is the one place
  /// this differs from a continuous reconciliation. A pending turn was aimed at
  /// a junction the client is no longer approaching, so replaying it would take
  /// the correction and immediately turn somewhere else wrong. A turn that
  /// still matters will be re-sent by the player, who is holding the key.
  fn resync(&mut self, authoritative: PlayerState, at_tick: u64) {
    self.predicted = authoritative;
    self.queue.clear();
    self.predicted_turns.clear();

    let last = self.next_tick.saturating_sub(1);
    let mut replay: Vec<Pending> = self.pending.iter().copied().filter(|p| p.tick > at_tick).collect();
    replay.sort_by_key(|p| p.tick);

    let mut next = 0usize;
    let mut tick = at_tick;
    while tick < last {
      tick += 1;
      while next < replay.len() && replay[next].tick <= tick {
        self.queue.request(replay[next].dir, replay[next].tick);
        next += 1;
      }
      rules::advance_player(&mut self.predicted, &mut self.queue, &self.maze, tick, self.turn_buffer_ms, SIM_STEP_MS);
    }
  }

  fn believed_at_tick(&self, tick: u64) -> Option<Cell> {
    self.history.iter().rev().find(|(t, _)| *t <= tick).map(|(_, cell)| *cell)
  }

  /// Retires acknowledged requests, **except any not yet run locally**.
  ///
  /// The server acknowledges on arrival, which is before the tick the request
  /// named; trimming on the sequence alone discards inputs the prediction has
  /// not run.
  pub fn on_input_ack(&mut self, seq: u64) {
    self.pending.retain(|p| p.seq > seq || !p.applied);
  }

  /// Share of predicted turns the server took somewhere else.
  pub fn wrong_junction_rate(&self) -> f64 {
    if self.predicted_turns_total == 0 {
      0.0
    } else {
      self.wrong_junction as f64 * 100.0 / self.predicted_turns_total as f64
    }
  }

  pub fn snap_rate(&self) -> f64 {
    if self.frames_seen == 0 {
      0.0
    } else {
      self.snaps as f64 * 100.0 / self.frames_seen as f64
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::server::Server;

  fn controls() -> Controls {
    Controls {
      bots: false,
      input_playout: false,
      players: 2,
      ..Controls::default()
    }
  }

  /// A server past its opening countdown.
  fn started(controls: &Controls) -> Server {
    let mut server = Server::new(2, MAZE_SEED);
    while server.countdown_ms().is_some() {
      server.advance(SIM_STEP_MS, controls);
    }
    server
  }

  fn joined(server: &Server, me: PlayerId) -> Client {
    let mut client = Client::new(me);
    client.on_round(&server.round_start());
    client
  }

  #[test]
  fn a_client_that_agrees_never_snaps_and_never_turns_wrong() {
    let c = controls();
    let mut server = started(&c);
    let mut client = joined(&server, 0);

    let at = server.players[0].cell;
    let heading = server.players[0].heading;
    let want = server.maze.exits(at).into_iter().find(|d| *d != heading).expect("a turn");
    client.schedule_turn(1, 0, want);
    server.submit(0, 0, want, &c);

    for _ in 0..300 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      for taken in &out.turns {
        client.on_turn_taken(taken);
      }
      if let Some((_, frame)) = out.frames.iter().find(|(id, _)| *id == client.me) {
        client.on_frame(frame, &c);
      }
    }
    assert_eq!(client.snaps, 0, "identical rules, identical inputs");
    assert_eq!(client.wrong_junction, 0, "and the same junctions");
    assert!(client.predicted_turns_total > 0, "the test needs a turn to have happened");
  }

  #[test]
  fn a_turn_the_server_took_elsewhere_is_counted_as_a_wrong_junction() {
    let c = controls();
    let server = started(&c);
    let mut client = joined(&server, 0);

    let at = client.predicted.cell;
    let heading = client.predicted.heading;
    let want = client.maze.exits(at).into_iter().find(|d| *d != heading).expect("a turn");
    // From the server's clock, not from zero: zero is inside the opening
    // countdown, where nothing moves by design.
    let base = server.now_ms();
    client.schedule_turn(1, base / SIM_STEP_MS, want);
    for tick in 0..80u64 {
      client.tick(base + tick * SIM_STEP_MS, &c);
      if client.predicted_turns_total > 0 {
        break;
      }
    }
    assert!(client.predicted_turns_total > 0, "the client took the turn somewhere");

    // The server reports the same turn taken two cells away.
    let elsewhere = Cell::new(at.x.saturating_add(2), at.y);
    client.on_turn_taken(&TurnTaken {
      player: 0,
      dir: want,
      at: elsewhere,
      tick: 0,
    });
    assert_eq!(client.wrong_junction, 1);
    assert!(client.worst_junction_error > 0);
    assert!(client.wrong_junction_rate() > 0.0);
  }

  #[test]
  fn a_turn_taken_where_the_server_took_it_is_not_counted() {
    let c = controls();
    let server = started(&c);
    let mut client = joined(&server, 0);

    let at = client.predicted.cell;
    let heading = client.predicted.heading;
    let want = client.maze.exits(at).into_iter().find(|d| *d != heading).expect("a turn");
    // From the server's clock, not from zero: zero is inside the opening
    // countdown, where nothing moves by design.
    let base = server.now_ms();
    client.schedule_turn(1, base / SIM_STEP_MS, want);
    for tick in 0..80u64 {
      client.tick(base + tick * SIM_STEP_MS, &c);
      if client.predicted_turns_total > 0 {
        break;
      }
    }
    let mine = client.predicted_turns.front().copied().expect("a predicted turn");
    client.on_turn_taken(&TurnTaken {
      player: 0,
      dir: mine.dir,
      at: mine.at,
      tick: 0,
    });
    assert_eq!(client.wrong_junction, 0);
  }

  #[test]
  fn the_prediction_is_driven_by_the_clock_not_by_how_often_it_is_polled() {
    let c = controls();
    let server = started(&c);
    let mut once = joined(&server, 0);
    let mut twice = joined(&server, 0);

    let base = server.now_ms();
    for i in 0..40u64 {
      let now = base + i * SIM_STEP_MS;
      once.tick(now, &c);
      twice.tick(now, &c);
      twice.tick(now, &c);
    }
    assert_eq!(twice.my_player().cell, once.my_player().cell, "polling twice must not run twice as far");
  }

  #[test]
  fn a_correction_drops_an_aim_from_before_it() {
    // A turn still waiting for a place was aimed at a junction on the route the
    // client was on. A correction puts it on a different route, so carrying the
    // aim across would take the correction and then immediately turn somewhere
    // else wrong: a second error caused by fixing the first.
    //
    // Requests *newer* than the correction are a different matter and are
    // replayed, because the server has not run them yet either.
    let c = controls();
    let mut server = started(&c);
    let mut client = joined(&server, 0);
    let base = server.now_ms();
    client.tick(base, &c);

    // Aim somewhere that cannot be taken here, so it sits in the queue.
    let blocked = Dir::ALL
      .into_iter()
      .find(|d| client.predicted.cell.step(*d).is_none_or(|cell| !client.maze.open(cell)));
    let Some(blocked) = blocked else {
      return; // Open on every side; nothing to hold.
    };
    let aim_tick = base / SIM_STEP_MS + 1;
    client.schedule_turn(1, aim_tick, blocked);
    client.tick(base + SIM_STEP_MS, &c);
    assert_eq!(client.queued_turn(), Some(blocked), "the aim is waiting for a place");

    // A frame describing a *later* tick than the aim, so the aim is behind the
    // correction rather than ahead of it.
    server.advance(SIM_STEP_MS * 4, &c);
    let mut frame = server.frame();
    frame.players[0].cell = server.maze.corridors()[3];
    client.tick(server.now_ms(), &c);
    client.on_frame(&frame, &c);

    assert_eq!(client.queued_turn(), None, "the stale aim is dropped, not carried into the new route");
  }

  #[test]
  fn switching_prediction_off_hands_the_authoritative_state_through() {
    let c = Controls {
      predict_local: false,
      ..controls()
    };
    let mut server = started(&c);
    let mut client = joined(&server, 0);
    for _ in 0..60 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      if let Some((_, frame)) = out.frames.iter().find(|(id, _)| *id == client.me) {
        client.on_frame(frame, &c);
      }
    }
    assert_eq!(client.snaps, 0, "there is no prediction to be wrong");
    assert_eq!(client.my_player().cell, server.players[0].cell);
  }
}
