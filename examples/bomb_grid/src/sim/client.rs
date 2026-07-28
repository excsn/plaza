//! What a client believes, and what it does when the server disagrees.
//!
//! This is the file the example exists for.
//!
//! # Why there is no `PredictedPlayer` here
//!
//! `plaza_client_utils::PredictedPlayer` is the right tool for a continuous
//! entity, and both other networked playgrounds use it. It cannot be used here,
//! and the reason is not that it is missing a feature: its whole shape is
//! *seen* position, *settled* position, and an ease between them over a few
//! frames. On a lattice there is nothing between two cells. A correction of one
//! cell has no fraction to travel through, so an ease would be drawing the
//! player somewhere they have never been, in a game where where you are decides
//! whether you are on fire.
//!
//! So a correction here **snaps**, and the honest thing to do is count the snaps
//! and put the number on screen. That is the trade a grid game makes and it
//! cannot be smoothed away.
//!
//! # What is predicted, and what is not
//!
//! Only the local player, and only through [`rules`], which is the same code the
//! server runs. Remote players, bombs and pickups are drawn from what arrived,
//! at one render instant behind the server clock, exactly as the other
//! playgrounds draw remote state.
//!
//! A dropped bomb is predicted too, and it is the sharper case: a bomb is a
//! discrete event with a discrete refusal (the carry limit, an occupied cell),
//! so a refused prediction is a bomb the player watched appear and then vanish.
//! Counting those is how the panel makes optimism's cost legible.

use std::collections::VecDeque;

use crate::sim::protocol::{BlastEvent, Frame, Intent, RoundStart};
use crate::sim::rules;
use crate::sim::types::*;

/// How much predicted history to keep, for comparing a frame against what this
/// client believed at the instant that frame describes.
///
/// A second at 60 Hz. The comparison has to be against the *same moment*, or a
/// client with 200 ms of latency reports a snap on every frame simply for being
/// ahead, which is the reading that makes a prediction counter useless.
const HISTORY: usize = 64;

/// The most ticks the prediction will catch up in one call.
///
/// A clock that lurches forward (a resumed tab, a fit that moved a long way at
/// once) would otherwise run thousands of steps inside one frame and stall the
/// renderer. Past this the prediction skips ahead and lets the next frame settle
/// the difference: one snap rather than a freeze.
const CATCH_UP_TICKS: u64 = 64;

/// One input this client has sent and the server has not yet acknowledged.
#[derive(Clone, Copy, Debug)]
struct Pending {
  seq: u64,
  /// The tick this input named, which is the tick the server will run it on and
  /// therefore the tick this client must run it on too.
  tick: u64,
  at_ms: u64,
  intent: Intent,
  /// Whether the prediction has already run this one. A flag rather than a
  /// high-water tick, because tick zero is a legitimate tick and a watermark
  /// starting at zero silently swallows it.
  applied: bool,
}

/// A bomb this client drew before the server confirmed it.
#[derive(Clone, Copy, Debug)]
struct Phantom {
  bomb: BombState,
  /// The client clock when it was predicted, so it can be retired if no frame
  /// confirms it within a round trip and a bit.
  at_ms: u64,
}

/// What this client believes the world looks like.
pub struct Client {
  pub me: PlayerId,
  pub grid: Grid,
  /// Every player as the server last described them, **except** the local one
  /// under prediction, which is [`Client::predicted`].
  pub players: Vec<PlayerState>,
  pub bombs: Vec<BombState>,
  pub powerups: Vec<PowerupState>,
  /// Fire currently drawn, with the server time it stops burning.
  pub fire: Vec<(Cell, u64)>,

  /// The local player, run forward from this client's own input.
  predicted: PlayerState,
  /// Whether prediction is on at all. Off, [`Client::my_player`] returns the
  /// authoritative copy and the game plays with a full round trip of input lag,
  /// which is the comparison the panel switch exists to make.
  predict: bool,
  /// Inputs sent and not yet acknowledged, replayed over an authoritative state
  /// when one arrives.
  pending: VecDeque<Pending>,
  /// What this client believed its cell was, **keyed by tick**. Compared
  /// against the tick a frame describes rather than against the newest belief.
  history: VecDeque<(u64, Cell)>,
  /// The next tick the prediction has yet to simulate.
  ///
  /// The prediction is a function of this and the inputs, never of how often
  /// the frame loop polled: see [`Client::tick`].
  next_tick: u64,
  /// Bombs drawn optimistically, waiting for a frame to confirm them.
  phantoms: Vec<Phantom>,
  /// The direction currently held, which the prediction keeps walking under.
  held: Dir,
  /// Whether the server is holding the world still.
  ///
  /// It does exactly that between a round being settled and the next board
  /// arriving, so the last explosion stays on screen long enough to read. There
  /// is nothing in a frame that says so: the players simply stop moving, which
  /// is indistinguishable from everybody standing still.
  ///
  /// A client that does not know keeps walking a player the server is
  /// deliberately freezing, and every frame becomes a correction invented out
  /// of a rule the client was never told. The black hole example hit the same
  /// thing through its respawn delay, which is why `PredictedPlayer` grew
  /// `set_active`: nothing else expressed "the server has stopped simulating
  /// this".
  paused: bool,
  /// This client's estimate of the server clock, mirrored in every tick.
  server_now_ms: u64,
  /// How far behind the server clock remote state is drawn.
  render_delay_ms: u64,

  /// Snaps: a correction the client could not ease, only jump.
  pub snaps: u64,
  /// Total cells jumped, so one four-cell snap is distinguishable from four
  /// one-cell ones. A count alone hides the difference and the difference is
  /// exactly what a player feels.
  pub snapped_cells: u64,
  /// Server time of the newest snap, for a fading marker on screen.
  pub last_snap_ms: u64,
  /// Bombs this client drew and the server never confirmed.
  pub phantom_bombs: u64,
  /// Bombs predicted in total: the denominator, without which the count above
  /// says nothing.
  pub predicted_bombs: u64,
  /// Frames describing a tick this client had not simulated yet.
  ///
  /// The measurement the offline harness cannot make, because it hands its
  /// clients the server's own clock. A real client *fits* one, and a fit that
  /// sits behind the stream makes every cell boundary look like a
  /// disagreement: the newest tick it has run is still the previous cell. This
  /// counts how often that happened, so the residual snap rate has somewhere to
  /// be attributed instead of being guessed at.
  pub unreached_frames: u64,
  /// The newest tick any frame has described, for the lead readout.
  newest_frame_tick: u64,
  /// Deaths the client drew from a blast before the frame confirming them.
  pub frames_seen: u64,
  pub rounds_seen: u32,
}

impl Client {
  pub fn new(me: PlayerId) -> Self {
    Self {
      me,
      grid: Grid::default(),
      players: Vec::new(),
      bombs: Vec::new(),
      powerups: Vec::new(),
      fire: Vec::new(),
      predicted: PlayerState::new(me, Cell::new(1, 1)),
      predict: true,
      pending: VecDeque::new(),
      history: VecDeque::new(),
      next_tick: 0,
      phantoms: Vec::new(),
      held: Dir::None,
      paused: false,
      server_now_ms: 0,
      render_delay_ms: 140,
      snaps: 0,
      snapped_cells: 0,
      last_snap_ms: 0,
      phantom_bombs: 0,
      predicted_bombs: 0,
      unreached_frames: 0,
      newest_frame_tick: 0,
      frames_seen: 0,
      rounds_seen: 0,
    }
  }

  pub fn set_render_delay(&mut self, ms: u64) {
    self.render_delay_ms = ms;
  }

  /// Tells this client the server has stopped simulating, or resumed.
  ///
  /// Set from `Op::RoundOver`, cleared by the next round. See the note on
  /// `paused` for why a client that guesses instead is wrong every frame of the
  /// interval.
  pub fn set_paused(&mut self, paused: bool) {
    self.paused = paused;
  }

  pub fn is_paused(&self) -> bool {
    self.paused
  }

  /// Whether there is a board worth drawing yet.
  pub fn ready(&self) -> bool {
    !self.players.is_empty()
  }

  /// The instant remote state is drawn at: the server clock, less the declared
  /// delay. Declared rather than measured, so every client shows the same
  /// moment and the server can reason about what a client has yet to play.
  pub fn render_at_ms(&self) -> u64 {
    self.server_now_ms.saturating_sub(self.render_delay_ms)
  }

  /// The local player as it should be drawn: predicted, or authoritative when
  /// prediction is switched off.
  pub fn my_player(&self) -> &PlayerState {
    if self.predict {
      &self.predicted
    } else {
      self.players.iter().find(|p| p.id == self.me).unwrap_or(&self.predicted)
    }
  }

  /// Every bomb to draw: confirmed, plus this client's own optimistic ones.
  pub fn drawn_bombs(&self) -> Vec<BombState> {
    let mut all = self.bombs.clone();
    for phantom in &self.phantoms {
      if !all.iter().any(|b| b.cell == phantom.bomb.cell) {
        all.push(phantom.bomb);
      }
    }
    all
  }

  /// Fire still burning at the render instant.
  pub fn drawn_fire(&self) -> Vec<Cell> {
    let at = self.render_at_ms();
    self.fire.iter().filter(|(_, until)| *until > at).map(|(cell, _)| *cell).collect()
  }

  /// A fresh board.
  pub fn on_round(&mut self, round: &RoundStart) {
    self.grid = round.grid.clone();
    self.players = round.players.clone();
    self.bombs.clear();
    self.powerups.clear();
    self.fire.clear();
    self.phantoms.clear();
    self.pending.clear();
    self.history.clear();
    self.held = Dir::None;
    // A new board is the server simulating again, whatever it said before.
    self.paused = false;
    self.server_now_ms = round.server_time_ms;
    self.next_tick = round.tick;
    if let Some(mine) = round.players.iter().find(|p| p.id == self.me) {
      self.predicted = mine.clone();
    }
    self.rounds_seen += 1;
  }

  /// Records an input this client is sending, scheduled for the tick it names.
  ///
  /// **Not applied immediately, and that is the whole subtlety.** The server
  /// runs a tick-addressed input on the tick it named, which is now plus the
  /// playout depth. A client that predicted it the instant the key went down
  /// would be running that input a playout depth *earlier* than the server,
  /// and on a lattice that is not a small error that eases away: it is a whole
  /// cell of disagreement on every single input, and the snap counter would
  /// read as though prediction did not work at all.
  ///
  /// So prediction here hides the **round trip** and nothing else. The playout
  /// delay is still paid, by everybody, which is exactly what makes a contested
  /// cell independent of ping. Turning the playout buffer off in the panel
  /// removes both the fairness and this delay together.
  pub fn schedule_input(&mut self, seq: u64, tick: u64, intent: Intent, now_server_ms: u64) {
    self.pending.push_back(Pending {
      seq,
      tick,
      at_ms: now_server_ms,
      intent,
      applied: false,
    });
    while self.pending.len() > 256 {
      self.pending.pop_front();
    }
  }

  /// One tick of prediction: this tick's inputs, then one step of the rule.
  ///
  /// The same order the server uses, because the order matters: an input that
  /// lands on tick N takes effect *for* tick N, not after it.
  fn step_once(&mut self, tick: u64, controls: &Controls) {
    let mut due: Vec<Pending> = Vec::new();
    for input in self.pending.iter_mut() {
      if !input.applied && input.tick <= tick {
        input.applied = true;
        due.push(*input);
      }
    }
    due.sort_by_key(|input| input.tick);
    for input in due {
      self.apply_intent(input.intent, input.at_ms, controls);
    }
    let bombs = self.drawn_bombs();
    rules::advance_player(&mut self.predicted, self.held, &self.grid, &bombs, SIM_STEP_MS);
  }

  /// Runs one intent against the predicted state.
  ///
  /// Returns whether it was acted on, which for a bomb is the difference
  /// between drawing one and not.
  fn apply_intent(&mut self, intent: Intent, at_ms: u64, controls: &Controls) -> bool {
    match intent {
      Intent::Walk(dir) => {
        self.held = dir;
        true
      }
      Intent::Bomb => {
        if !controls.predict_bombs {
          return false;
        }
        // Refused for exactly the reasons the server will refuse it, through
        // the shared rule. A client that guessed differently would produce a
        // phantom on every carry-limit mistake.
        let all = self.drawn_bombs();
        let Some(cell) = rules::bomb_placement(&self.predicted, &all) else {
          return false;
        };
        self.phantoms.push(Phantom {
          bomb: BombState {
            cell,
            owner: self.me,
            fires_at_ms: at_ms + FUSE_MS,
            radius: self.predicted.blast_radius,
          },
          at_ms,
        });
        self.predicted_bombs += 1;
        true
      }
    }
  }

  /// Advances the prediction to whatever tick the clock says it is.
  ///
  /// **Driven by the clock, not by the caller's frame delta**, and that is the
  /// whole of it. The server advances its players exactly once per tick, on a
  /// fixed grid derived from its own clock. A client that advanced once per
  /// frame would be stepping at whatever rate the renderer happened to hit, on
  /// a grid not aligned with the server's, and the two drift apart *within a
  /// single cell*: they cross the boundary at different moments, and any frame
  /// that lands in the gap between those moments is a disagreement about which
  /// cell the player is in.
  ///
  /// In a continuous game that is a sub-pixel wobble nobody sees. On a lattice
  /// it is a whole cell, so it snaps, and it gets worse the more cells you
  /// cross: open ground is where it becomes obvious. Measured on a 120 Hz
  /// renderer against a 62 Hz server, it was eight snaps per hundred frames
  /// with no packet loss and an eight millisecond link.
  ///
  /// So the prediction runs the same tick loop the server does: catch up to
  /// `now / SIM_STEP_MS`, one step at a time, running each tick's inputs on
  /// that tick. There is deliberately no `dt` parameter: a caller must not be
  /// able to influence how fast the prediction runs.
  pub fn tick(&mut self, server_now_ms: u64, controls: &Controls) {
    self.server_now_ms = server_now_ms;
    self.predict = controls.predict_local;
    self.render_delay_ms = controls.render_delay_ms;

    let target = server_now_ms / SIM_STEP_MS;
    // A clock that jumps forward (a resumed tab, a fit that lurched) must not
    // turn into thousands of steps inside one frame. Past this the prediction
    // skips ahead and lets the next authoritative frame settle the difference:
    // one snap rather than a freeze.
    if target.saturating_sub(self.next_tick) > CATCH_UP_TICKS {
      self.next_tick = target.saturating_sub(CATCH_UP_TICKS);
    }

    // While the server is holding the world still, so is this client. Not a
    // special case inside the movement rule: the server is not *running* the
    // rule, so neither is this.
    if self.predict && !self.paused {
      // Inclusive of the target: tick N's inputs take effect *for* tick N, so
      // reaching tick N means N has been simulated, not that it is next.
      while self.next_tick <= target {
        let tick = self.next_tick;
        self.next_tick += 1;
        self.step_once(tick, controls);
      }
    } else {
      self.next_tick = target + 1;
    }

    // One entry per tick, not per call. A 120 Hz renderer against a 62 Hz
    // simulation would otherwise fill the buffer with duplicates and halve the
    // window this history covers, so how far back a frame may reach would
    // depend on the frame rate.
    match self.history.back_mut() {
      Some((t, cell)) if *t == target => *cell = self.predicted.cell,
      _ => self.history.push_back((target, self.predicted.cell)),
    }
    while self.history.len() > HISTORY {
      self.history.pop_front();
    }

    // A phantom that no frame has confirmed within a generous round trip was
    // refused. Retiring it here rather than on the next frame means the bomb
    // vanishes at a predictable moment instead of whenever traffic resumes.
    // Compared as an age rather than against a deadline computed by
    // subtraction: a saturating subtraction floors at zero, so early in a
    // round every deadline is zero and every phantom is retired the instant it
    // is created.
    let window = controls.latency_ms * 2 + controls.sync_interval_ms() + 250;
    let before = self.phantoms.len();
    self.phantoms.retain(|p| server_now_ms.saturating_sub(p.at_ms) <= window);
    self.phantom_bombs += (before - self.phantoms.len()) as u64;

    let at = self.render_at_ms();
    self.fire.retain(|(_, until)| *until > at.saturating_sub(BLAST_MS));
  }

  /// Folds in one authoritative frame.
  ///
  /// This is where a disagreement is found, and it is compared **at the frame's
  /// own instant** rather than against the newest belief: a client running a
  /// latency ahead of the server is not wrong for being ahead, and comparing
  /// the two directly would report a snap on every frame.
  pub fn on_frame(&mut self, frame: &Frame, controls: &Controls) {
    self.frames_seen += 1;
    self.players = frame.players.clone();
    self.bombs = frame.bombs.clone();
    self.powerups = frame.powerups.clone();

    // Any phantom the server has now confirmed stops being a phantom.
    self.phantoms.retain(|p| !frame.bombs.iter().any(|b| b.cell == p.bomb.cell && b.owner == p.bomb.owner));

    let Some(authoritative) = frame.players.iter().find(|p| p.id == self.me).cloned() else {
      return;
    };
    if !controls.predict_local {
      self.predicted = authoritative;
      return;
    }

    // A frame describing a tick this client has not reached yet is not
    // evidence of anything. That happens whenever the clock estimate sits
    // behind the stream, which is ordinary for a fitted clock, and comparing
    // anyway reports the client's own lag as a misprediction: at a cell
    // boundary the newest tick it *has* simulated is the previous cell, so
    // every crossing looks like a disagreement. It is the same rule as the
    // start of history, at the other end.
    let reached = self.next_tick.saturating_sub(1);
    self.newest_frame_tick = self.newest_frame_tick.max(frame.tick);
    if frame.tick > reached {
      self.unreached_frames += 1;
    }
    let believed = (frame.tick <= reached).then(|| self.believed_at_tick(frame.tick)).flatten();
    let disagrees = believed.is_some_and(|cell| cell != authoritative.cell);

    // A death is not a misprediction: the client is not predicting whether it
    // is on fire, so adopting it is an update rather than a correction.
    let died = self.predicted.alive && !authoritative.alive;

    if disagrees && !died {
      // Measured at the frame's instant, not against where the prediction has
      // since got to. The second is a different quantity: a client that has
      // already walked on could be back in agreement by now, and reporting a
      // zero-cell snap would say the correction was free when a whole cell of
      // it was drawn.
      let jump = believed.map_or(1, |cell| cell.distance(authoritative.cell)).max(1);
      self.snaps += 1;
      self.snapped_cells += jump as u64;
      self.last_snap_ms = frame.server_time_ms;
      self.resync(authoritative, frame.tick);
    } else {
      // Agreeing about the cell does not mean agreeing about everything: the
      // upgrades and the alive flag are the server's alone, and a client never
      // predicts them.
      self.predicted.alive = authoritative.alive;
      self.predicted.bombs_max = authoritative.bombs_max;
      self.predicted.blast_radius = authoritative.blast_radius;
      self.predicted.speed_level = authoritative.speed_level;
      self.predicted.wins = authoritative.wins;
      if died {
        self.predicted.cell = authoritative.cell;
        self.predicted.step = None;
      }
    }
  }

  /// Adopts an authoritative state and replays everything sent since.
  ///
  /// The replay is what stops a snap from throwing away the inputs the player
  /// has already made: without it, a correction from 200 ms ago would rewind
  /// the player to 200 ms ago and they would have to walk it again.
  fn resync(&mut self, authoritative: PlayerState, at_tick: u64) {
    self.predicted = authoritative;

    // Re-run the same tick loop from the frame's tick to now, so the replayed
    // state is produced by exactly the process that produces the live one.
    // Replaying with one big `dt` instead would land somewhere the tick loop
    // never visits, which is a second implementation of the rule wearing the
    // first one's name.
    //
    // Inputs are *not* cleared of their `applied` flag: they are replayed by
    // being re-run through `step_once`, which reapplies any whose tick falls in
    // the window because the flag is only consulted for inputs that have not
    // been seen at all. What carries the replay is `held`, which the earlier
    // application already set.
    let last = self.next_tick.saturating_sub(1);
    let mut replay: Vec<Pending> = self.pending.iter().copied().filter(|p| p.tick > at_tick).collect();
    replay.sort_by_key(|p| p.tick);

    let bombs = self.drawn_bombs();
    let mut next = 0usize;
    let mut tick = at_tick;
    while tick < last {
      tick += 1;
      while next < replay.len() && replay[next].tick <= tick {
        if let Intent::Walk(dir) = replay[next].intent {
          self.held = dir;
        }
        next += 1;
      }
      rules::advance_player(&mut self.predicted, self.held, &self.grid, &bombs, SIM_STEP_MS);
    }
  }

  /// What this client believed its cell was on `tick`.
  ///
  /// Keyed by tick rather than by milliseconds because that is the unit both
  /// sides actually step in: comparing at a millisecond that falls inside a
  /// tick asks what the client believed halfway through a step the server
  /// takes atomically, and the answer is a disagreement that does not exist.
  ///
  /// The newest entry at or before the tick asked about. Before the history
  /// covers it (a joiner's first frames), `None` means "no opinion", which is
  /// not the same as "disagreed" and must not count as a snap.
  fn believed_at_tick(&self, tick: u64) -> Option<Cell> {
    self.history.iter().rev().find(|(t, _)| *t <= tick).map(|(_, cell)| *cell)
  }

  /// Retires acknowledged inputs, **except any this client has not run yet**.
  ///
  /// The server acknowledges an input on **arrival**, which is well before the
  /// tick it was named for: an input aimed at `now + playout` is acknowledged
  /// within a round trip and executed a playout depth from now, and on a fast
  /// link the acknowledgement wins that race every time.
  ///
  /// So an acknowledgement is not permission to forget the input. This list is
  /// two things at once: the replay buffer for a correction, and this client's
  /// own schedule of inputs whose tick has not come. Trimming on the sequence
  /// alone empties the second one from under the first, and the local
  /// prediction never runs what it dropped. That is invisible while you hold a
  /// direction and obvious the moment you release one: the release is discarded
  /// in flight, `held` keeps the old direction for ever, and every frame
  /// becomes a snap back followed by another step the wrong way.
  pub fn on_input_ack(&mut self, seq: u64) {
    self.pending.retain(|p| p.seq > seq || !p.applied);
  }

  /// Folds in one explosion.
  ///
  /// Applied to the board immediately rather than at the render instant,
  /// because the tiles it clears are an *input* to the movement rule this
  /// client is predicting against: holding a wall the server has destroyed
  /// would refuse a step the server allows, which is a snap manufactured out of
  /// stale state. The fire is drawn on the timeline; the board is not.
  pub fn on_blast(&mut self, blast: &BlastEvent) {
    for cell in &blast.cleared {
      self.grid.set(*cell, Tile::Empty);
    }
    for cell in &blast.bombs {
      self.bombs.retain(|b| b.cell != *cell);
      self.phantoms.retain(|p| p.bomb.cell != *cell);
    }
    for cell in &blast.burned {
      self.powerups.retain(|p| p.cell != *cell);
    }
    for drop in &blast.revealed {
      if !self.powerups.iter().any(|p| p.cell == drop.cell) {
        self.powerups.push(*drop);
      }
    }
    let until = blast.at_ms + BLAST_MS;
    for cell in &blast.cells {
      self.fire.push((*cell, until));
    }
    for id in &blast.killed {
      if let Some(player) = self.players.iter_mut().find(|p| p.id == *id) {
        player.alive = false;
      }
      if *id == self.me {
        self.predicted.alive = false;
        self.predicted.step = None;
      }
    }
  }

  /// How far this client's simulation is ahead of the newest frame, in ticks.
  ///
  /// Positive is healthy and expected: the client runs at the clock's estimate
  /// of *now* while a frame describes a moment one delivery ago. At or below
  /// zero the clock estimate is trailing the stream, and every cell boundary
  /// then reads as a disagreement it is not.
  pub fn tick_lead(&self) -> i64 {
    self.next_tick.saturating_sub(1) as i64 - self.newest_frame_tick as i64
  }

  /// Snaps per hundred frames: the rate, which is comparable across sessions of
  /// different lengths where a raw count is not.
  pub fn snap_rate(&self) -> f64 {
    if self.frames_seen == 0 {
      0.0
    } else {
      self.snaps as f64 * 100.0 / self.frames_seen as f64
    }
  }

  /// Share of predicted bombs the server refused.
  pub fn phantom_rate(&self) -> f64 {
    if self.predicted_bombs == 0 {
      0.0
    } else {
      self.phantom_bombs as f64 * 100.0 / self.predicted_bombs as f64
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

  fn joined(server: &Server, me: PlayerId) -> Client {
    let mut client = Client::new(me);
    client.on_round(&server.round_start());
    client
  }

  #[test]
  fn a_client_that_agrees_never_snaps() {
    // The baseline the snap counter is only meaningful against: run the same
    // inputs on both sides at zero latency and nothing should ever jump.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    server.submit(0, 0, Intent::Walk(Dir::Right), &c);
    for _ in 0..60 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      if let Some(frame) = out.frame {
        client.on_frame(&frame, &c);
      }
    }
    assert_eq!(client.snaps, 0, "identical rules, identical inputs, no correction");
    assert_eq!(client.my_player().cell, server.players[0].cell);
  }

  #[test]
  fn a_disagreement_snaps_a_whole_cell_and_is_counted() {
    // The thing that cannot be eased. The server is told nothing, so the client
    // walks on its own and is corrected back.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    for _ in 0..60 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      if let Some(frame) = out.frame {
        client.on_frame(&frame, &c);
      }
    }
    assert!(client.snaps > 0, "the client walked somewhere the server did not");
    assert!(client.snapped_cells >= client.snaps, "and each snap moved it at least one cell");
    assert_eq!(client.my_player().cell, server.players[0].cell, "it ends up where the server says");
  }

  #[test]
  fn being_ahead_of_the_server_is_not_a_misprediction() {
    // The trap this history buffer exists for. A client a latency ahead is
    // *right*, and comparing its newest belief against an old frame reports a
    // snap on every single one.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    server.submit(0, 0, Intent::Walk(Dir::Right), &c);

    // Frames are held back a few ticks before being applied, so the client is
    // always running ahead of the state it is being shown.
    let mut delayed: Vec<(u64, Frame)> = Vec::new();
    for _ in 0..120 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      if let Some(frame) = out.frame {
        delayed.push((server.now_ms() + 150, frame));
      }
      let now = server.now_ms();
      let due: Vec<Frame> = delayed.iter().filter(|(at, _)| *at <= now).map(|(_, f)| f.clone()).collect();
      delayed.retain(|(at, _)| *at > now);
      for frame in due {
        client.on_frame(&frame, &c);
      }
    }
    assert_eq!(client.snaps, 0, "a client ahead of the frame it is reading is not wrong");
  }

  #[test]
  fn a_refused_bomb_is_retired_as_a_phantom() {
    // The discrete refusal with no way to ease it: the player saw a bomb and
    // then did not.
    let c = controls();
    let server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Bomb, 0);
    client.tick(0, &c);
    assert_eq!(client.drawn_bombs().len(), 1);
    assert_eq!(client.predicted_bombs, 1);

    // No frame ever confirms it, so it is retired once the deadline passes.
    for i in 0..400u64 {
      client.tick(i * SIM_STEP_MS, &c);
    }
    assert_eq!(client.phantom_bombs, 1, "the unconfirmed bomb was withdrawn");
    assert!(client.drawn_bombs().is_empty());
    assert!(client.phantom_rate() > 0.0);
  }

  #[test]
  fn the_carry_limit_is_refused_locally_rather_than_predicted_and_withdrawn() {
    // Predicting a bomb the server is certain to refuse would be a phantom
    // manufactured by the client. The shared rule is what prevents it.
    let c = controls();
    let server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Bomb, 0);
    client.tick(0, &c);
    client.schedule_input(2, 1, Intent::Bomb, 16);
    client.tick(32, &c);
    assert_eq!(client.predicted_bombs, 1, "and the refusal is not counted as a prediction");
  }

  #[test]
  fn a_blast_clears_the_board_immediately_rather_than_on_the_timeline() {
    // The board feeds the movement rule this client predicts against, so
    // holding a destroyed wall would refuse a step the server allows and
    // manufacture a snap.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    server.grid.set(Cell::new(2, 1), Tile::Soft);
    let mut client = joined(&server, 0);
    assert_eq!(client.grid.get(Cell::new(2, 1)), Tile::Soft);

    client.on_blast(&BlastEvent {
      at_ms: 1_000,
      cells: vec![Cell::new(1, 1), Cell::new(2, 1)],
      cleared: vec![Cell::new(2, 1)],
      ..Default::default()
    });
    assert_eq!(client.grid.get(Cell::new(2, 1)), Tile::Empty, "the wall is gone at once");
    assert!(!client.drawn_fire().is_empty() || client.render_at_ms() > 1_000 + BLAST_MS);
  }

  #[test]
  fn a_death_is_adopted_rather_than_counted_as_a_misprediction() {
    // Nothing here predicts whether you are on fire, so learning that you died
    // is an update, not a correction. Counting it would bury the real snaps.
    let c = controls();
    let mut server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);
    client.tick(0, &c);

    server.players[0].alive = false;
    let out = server.advance(SIM_STEP_MS, &c);
    let frame = out.frame.unwrap_or_else(|| server.frame());
    client.on_frame(&frame, &c);

    assert!(!client.my_player().alive);
    assert_eq!(client.snaps, 0, "a death is not a snap");
  }

  #[test]
  fn switching_prediction_off_hands_the_authoritative_state_straight_through() {
    let c = Controls {
      predict_local: false,
      ..controls()
    };
    let mut server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);

    client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    for _ in 0..30 {
      let out = server.advance(SIM_STEP_MS, &c);
      client.tick(server.now_ms(), &c);
      if let Some(frame) = out.frame {
        client.on_frame(&frame, &c);
      }
    }
    assert_eq!(client.snaps, 0, "there is no prediction to be wrong");
    assert_eq!(client.my_player().cell, server.players[0].cell);
  }

  #[test]
  fn an_acknowledgement_does_not_discard_an_input_this_client_has_not_run_yet() {
    // The bug a player reports as "it kept walking after I let go", and the
    // reason it survives a correct clock.
    //
    // An input is named for `now + playout`, and the server acknowledges it on
    // **arrival**, which is well before that tick. If the acknowledgement
    // retires it from the pending list, the local prediction never runs it: the
    // release is discarded in flight, `held` keeps its old direction for ever,
    // and every frame is a snap back followed by another step the wrong way.
    let c = controls();
    let server = Server::new(2, B0MB_SEED);
    let mut client = joined(&server, 0);
    // A clear lane, so a player that never stops is not stopped by a wall
    // instead and the assertion below can actually fail.
    for x in 1..GRID_W - 1 {
      client.grid.set(Cell::new(x, 1), Tile::Empty);
    }

    client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    client.tick(0, &c);
    assert!(client.my_player().step.is_some(), "walking");

    // The release, named for a tick in the future, acknowledged immediately.
    client.schedule_input(2, 20, Intent::Walk(Dir::None), 16);
    client.on_input_ack(2);

    for i in 1..80u64 {
      client.tick(i * SIM_STEP_MS, &c);
    }
    let settled = client.my_player().cell;
    for i in 80..200u64 {
      client.tick(i * SIM_STEP_MS, &c);
    }
    assert_eq!(client.my_player().cell, settled, "the release ran even though it was acknowledged first");
  }

  #[test]
  fn the_prediction_is_driven_by_the_clock_not_by_how_often_it_is_polled() {
    // The bug a player reports as "it snaps back a lot when I can run freely".
    //
    // The server advances its players exactly once per tick. A client that
    // advances once per *frame* is running a different clock: it steps at
    // whatever rate the renderer happens to hit, on a grid that is not aligned
    // with the server's, and the two drift apart within a single cell. On a
    // lattice that lands as a real disagreement about which cell you are in
    // every time a step completes, and a frame arriving in that window is a
    // snap. It gets worse the more cells you cross, which is exactly what open
    // ground means.
    //
    // The property that fixes it: predicted state is a function of the tick,
    // never of the poll count. Two clients handed the same clock must agree
    // however many times each was called.
    let c = controls();
    let server = Server::new(2, B0MB_SEED);
    let mut once = joined(&server, 0);
    let mut twice = joined(&server, 0);
    for client in [&mut once, &mut twice] {
      for x in 1..GRID_W - 1 {
        client.grid.set(Cell::new(x, 1), Tile::Empty);
      }
      client.schedule_input(1, 0, Intent::Walk(Dir::Right), 0);
    }

    // Stopped well short of the far wall: a player that walks twice as fast
    // still ends up against the same wall, and the assertion would pass for a
    // reason that has nothing to do with the code.
    for i in 0..50u64 {
      let now = i * SIM_STEP_MS;
      once.tick(now, &c);
      // The same instant, polled twice: a renderer running faster than the
      // simulation.
      twice.tick(now, &c);
      twice.tick(now, &c);
    }

    assert_eq!(
      twice.my_player().cell,
      once.my_player().cell,
      "polling the prediction twice per tick must not walk the player twice as far"
    );
  }
}
