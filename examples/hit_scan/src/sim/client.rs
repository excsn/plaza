//! One player's belief about the arena, and how wrong it is allowed to be.
//!
//! Three different things are drawn here and they are drawn three different
//! ways, which is the part worth reading. Your own player is *predicted*,
//! because waiting a round trip to turn is unplayable. Everybody else is
//! *interpolated between samples that have already arrived*, because a guess
//! about somebody else's steering is a guess about a human. Rockets are neither
//! and are simply watched, because they are the one body whose future the
//! server has already decided.

use std::collections::{BTreeMap, VecDeque};

use plaza_client_utils::{InterpolationClock, RemoteView, RenderOpts};
use plaza_server_utils::{InputSchedule, InputWindow};

use crate::sim::protocol::{DeathEvent, Frame, Op, ServerPolicy, ShotEvent};
use crate::sim::rules::move_circle;
use crate::sim::types::{
  Controls, Dir8, PLAYER_R, PLAYER_SPEED, PlayerId, PlayerSnap, PlayerState, RocketState, SIM_STEP_MS, V2, Weapon,
};

/// A correction big enough to be a disagreement rather than arithmetic noise.
///
/// Below this the two sides differ by the accumulated error of stepping the
/// same rule at slightly different moments, which is not a netcode event and
/// counting it would bury the ones that are.
const SNAP_PX: f32 = 1.5;

/// How many ticks of our own input history to keep for replay.
const REPLAY_TICKS: usize = 240;

/// An input that has been sent and not yet both acknowledged and run.
///
/// The direction is not kept here: the schedule holds it, and a second copy
/// would be a second answer to the same question.
#[derive(Clone, Copy, Debug)]
struct Named {
  seq: u64,
  tick: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ClientStats {
  /// Corrections large enough to see.
  pub snaps: u64,
  /// Total distance those corrections moved the player, so one big snap and
  /// forty small ones are not the same number.
  pub snap_px: f32,
  pub frames_seen: u64,
  pub shots_seen: u64,
  /// Times a peer's view was asked for a moment past everything it holds.
  pub over_extrapolations: u64,
  /// Times the render clock had no sample new enough to interpolate towards.
  pub underruns: u64,
}

pub struct Client {
  pub me: PlayerId,
  pub policy: Option<ServerPolicy>,
  pub stats: ClientStats,

  /// The newest authoritative frame, kept whole for the parts that are not
  /// predicted or interpolated.
  pub auth: Vec<PlayerState>,
  pub rockets: Vec<RocketState>,
  pub shots: VecDeque<ShotEvent>,
  pub deaths: VecDeque<DeathEvent>,

  clock: InterpolationClock<u64>,
  peers: BTreeMap<PlayerId, RemoteView<PlayerSnap, V2>>,

  predicted: V2,
  predicted_alive: bool,
  schedule: InputSchedule<Dir8>,
  /// What direction was held on each tick we have already simulated, so a
  /// correction can be replayed forward rather than merely applied.
  history: VecDeque<(u64, Dir8)>,
  sim_tick: u64,
  /// The direction being simulated. Written only by the schedule, on the tick
  /// the input named.
  held: Dir8,
  /// The direction last handed to [`Client::press`], which exists only to
  /// avoid resending an unchanged level input.
  ///
  /// Separate from `held` on purpose, and the separation is the whole of the
  /// prediction being correct. Letting a press write `held` runs the input a
  /// playout depth before the server will, so the two sides walk the same route
  /// out of step and every frame arrives as a correction.
  last_pressed: Dir8,

  input_seq: u64,
  acked_seq: u64,
  unrun: Vec<Named>,

  newest_stamp_ms: u64,
  started: bool,
}

impl Client {
  pub fn new(me: PlayerId, render_delay_ms: u64) -> Self {
    Self {
      me,
      policy: None,
      stats: ClientStats::default(),
      auth: Vec::new(),
      rockets: Vec::new(),
      shots: VecDeque::new(),
      deaths: VecDeque::new(),
      clock: InterpolationClock::new(render_delay_ms),
      peers: BTreeMap::new(),
      predicted: V2::ZERO,
      predicted_alive: true,
      schedule: InputSchedule::new(),
      history: VecDeque::new(),
      sim_tick: 0,
      held: Dir8::Still,
      last_pressed: Dir8::Still,
      input_seq: 0,
      acked_seq: 0,
      unrun: Vec::new(),
      newest_stamp_ms: 0,
      started: false,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.started
  }

  pub fn newest_stamp_ms(&self) -> u64 {
    self.newest_stamp_ms
  }

  /// The server instant this client is drawing.
  pub fn render_at(&self) -> Option<u64> {
    self.clock.target()
  }

  pub fn my_state(&self) -> Option<&PlayerState> {
    self.auth.iter().find(|p| p.id == self.me)
  }

  /// The tick a fresh input should name.
  ///
  /// A playout depth ahead of the server's present, because that is the tick
  /// the input will be due on. Naming *now* would name a tick the server has
  /// already run by the time the packet lands, and every input would be
  /// refused for arriving late at a tick it never had a chance to reach.
  pub fn aim_tick(&self, server_time_ms: u64) -> u64 {
    let depth = self.policy.map(|p| p.playout_delay_ms).unwrap_or(0);
    (server_time_ms + depth) / SIM_STEP_MS
  }

  /// Records a held direction and returns the op to send.
  pub fn press(&mut self, dir: Dir8, server_time_ms: u64) -> Option<Op> {
    if !self.started || dir == self.last_pressed {
      return None;
    }
    Some(self.schedule_walk(dir, server_time_ms))
  }

  /// Schedules a direction whether or not it changed, and returns the op.
  ///
  /// The unconditional form, for the keepalive on a real wire: a held
  /// direction is a *level*, so a lost change is not a missing update but a
  /// wrong state that persists until the player presses something else. That
  /// reads as the controls sticking rather than as packet loss.
  pub fn schedule_walk(&mut self, dir: Dir8, server_time_ms: u64) -> Op {
    self.last_pressed = dir;
    self.input_seq += 1;
    let tick = self.aim_tick(server_time_ms);
    // Scheduled for the tick it named rather than applied now. Applying it
    // immediately runs the input a whole playout depth before the server will,
    // and every single one of them then reads as a correction.
    self.schedule.submit(tick, dir, self.sim_tick, self.window());
    self.unrun.push(Named { seq: self.input_seq, tick });
    Op::Move { seq: self.input_seq, tick, dir }
  }

  /// The tick the newest input named, for the panel.
  pub fn last_input_tick(&self) -> u64 {
    self.unrun.last().map(|n| n.tick).unwrap_or(self.sim_tick)
  }

  /// Pulls the trigger. The aim crosses the wire with the shot rather than
  /// being tracked continuously, because it is only ever needed at this
  /// instant, and a shot that named its own aim cannot be resolved against a
  /// stale one.
  pub fn shoot(&mut self, aim: V2, weapon: Weapon, server_time_ms: u64) -> Option<Op> {
    if !self.started {
      return None;
    }
    self.input_seq += 1;
    Some(Op::Shoot {
      seq: self.input_seq,
      tick: self.aim_tick(server_time_ms),
      aim_deg: aim.to_degrees_i16(),
      weapon,
    })
  }

  fn window(&self) -> InputWindow {
    let policy = self.policy;
    InputWindow {
      max_late: policy.map(|p| p.input_max_late_ticks).unwrap_or(4),
      max_early: policy.map(|p| p.input_max_early_ticks).unwrap_or(30),
    }
  }

  pub fn on_op(&mut self, op: Op, _now_ms: u64) {
    match op {
      Op::Welcome { player, policy, start } => {
        self.me = player;
        self.policy = Some(policy);
        self.clock.set_delay(policy.render_delay_ms);
        self.auth = start.players.clone();
        self.sim_tick = start.tick;
        self.newest_stamp_ms = start.server_time_ms;
        self.predicted = start.players.iter().find(|p| p.id == player).map(|p| p.pos).unwrap_or(V2::ZERO);
        self.predicted_alive = true;
        self.started = true;
      }
      Op::Policy(policy) => {
        self.policy = Some(policy);
        self.clock.set_delay(policy.render_delay_ms);
      }
      Op::Frame(frame) => self.on_frame(*frame),
      Op::Shot(shot) => {
        self.stats.shots_seen += 1;
        if self.shots.len() == 64 {
          self.shots.pop_front();
        }
        self.shots.push_back(*shot);
      }
      Op::Died(death) => {
        if self.deaths.len() == 16 {
          self.deaths.pop_front();
        }
        self.deaths.push_back(*death);
      }
      Op::InputAck { seq } => {
        self.acked_seq = self.acked_seq.max(seq);
        // Retained while unrun even once acknowledged. Trimming by sequence
        // alone throws away an input this client has scheduled and not yet
        // reached, which reads to the player as a key release going missing.
        let sim_tick = self.sim_tick;
        self.unrun.retain(|p| p.seq > seq || p.tick > sim_tick);
      }
      Op::NoSeat { .. } | Op::Refused { .. } | Op::Move { .. } | Op::Shoot { .. } => {}
    }
  }

  fn on_frame(&mut self, frame: Frame) {
    self.stats.frames_seen += 1;
    self.newest_stamp_ms = self.newest_stamp_ms.max(frame.server_time_ms);
    self.clock.observe(frame.server_time_ms);
    self.rockets = frame.rockets.clone();

    for p in &frame.players {
      if p.id == self.me {
        continue;
      }
      let view = self.peers.entry(p.id).or_insert_with(|| RemoteView::new(16, 250));
      view.push(frame.server_time_ms, PlayerSnap { pos: p.pos, alive: p.alive }, p.velocity());
    }

    if let Some(mine) = frame.players.iter().find(|p| p.id == self.me) {
      self.reconcile(mine, frame.tick);
    }
    self.auth = frame.players;
  }

  /// Puts the authoritative position back under the prediction, then replays
  /// everything that has happened since.
  ///
  /// The replay is the half that matters. Snapping to the authoritative
  /// position alone would drag the player backwards by exactly one round trip
  /// on every frame, because that position is a round trip old by the time it
  /// arrives.
  fn reconcile(&mut self, authoritative: &PlayerState, at_tick: u64) {
    let before = self.predicted;
    self.predicted = authoritative.pos;
    self.predicted_alive = authoritative.alive;

    if at_tick < self.sim_tick {
      let dt = SIM_STEP_MS as f32 / 1000.0;
      for tick in (at_tick + 1)..=self.sim_tick {
        let dir = self.history.iter().rev().find(|(t, _)| *t == tick).map(|(_, d)| *d).unwrap_or(Dir8::Still);
        if self.predicted_alive {
          self.predicted = move_circle(self.predicted, dir.unit().scale(PLAYER_SPEED * dt), PLAYER_R);
        }
      }
    }

    let moved = before.dist(self.predicted);
    if moved > SNAP_PX {
      self.stats.snaps += 1;
      self.stats.snap_px += moved;
    }
  }

  /// Advances the prediction and the render clock by whole quanta.
  pub fn advance(&mut self, elapsed_ms: u64, server_time_ms: u64, controls: &Controls) {
    self.clock.advance(elapsed_ms);
    if self.clock.target().is_none() {
      self.stats.underruns += 1;
    }

    let target_tick = server_time_ms / SIM_STEP_MS;
    let dt = SIM_STEP_MS as f32 / 1000.0;
    while self.sim_tick < target_tick {
      self.sim_tick += 1;
      // The same schedule the server runs, on the same tick numbers, which is
      // the only reason the two agree at all.
      if let Some(dir) = self.schedule.execute_due(self.sim_tick) {
        self.held = dir;
      }
      if self.history.len() == REPLAY_TICKS {
        self.history.pop_front();
      }
      self.history.push_back((self.sim_tick, self.held));

      if controls.predict_self && self.predicted_alive {
        self.predicted = move_circle(self.predicted, self.held.unit().scale(PLAYER_SPEED * dt), PLAYER_R);
      }
    }
    let sim_tick = self.sim_tick;
    self.unrun.retain(|p| p.tick > sim_tick || p.seq > self.acked_seq);
  }

  /// Where this client would draw everybody, right now.
  ///
  /// The positions a render error is measured against. Returned rather than
  /// drawn so the harness and the panel read exactly what the renderer does.
  pub fn render(&self, controls: &Controls) -> Vec<(PlayerId, V2, bool)> {
    let target = self.render_at();
    let opts = RenderOpts {
      interpolate: controls.interpolate_peers,
      extrapolate: controls.extrapolate_peers,
    };
    let mut out = Vec::with_capacity(self.auth.len());
    for p in &self.auth {
      if p.id == self.me {
        let pos = if controls.predict_self { self.predicted } else { p.pos };
        out.push((p.id, pos, self.predicted_alive));
      } else if let Some(view) = self.peers.get(&p.id) {
        match view.render(target, opts) {
          Some(snap) => out.push((p.id, snap.pos, snap.alive)),
          None => out.push((p.id, p.pos, p.alive)),
        }
      } else {
        out.push((p.id, p.pos, p.alive));
      }
    }
    out
  }

  pub fn predicted_pos(&self) -> V2 {
    self.predicted
  }

  pub fn unacked(&self) -> usize {
    self.unrun.len()
  }

  pub fn input_seq(&self) -> u64 {
    self.input_seq
  }

  pub fn acked_seq(&self) -> u64 {
    self.acked_seq
  }

  pub fn sim_tick(&self) -> u64 {
    self.sim_tick
  }

  /// How far the simulation runs ahead of the newest frame, in ticks.
  ///
  /// Must stay positive. At or below zero the client is naming input ticks the
  /// server has already run, and every one of them is refused.
  pub fn lead_ticks(&self) -> i64 {
    self.sim_tick as i64 - (self.newest_stamp_ms / SIM_STEP_MS) as i64
  }

  pub fn peer_over_extrapolations(&self) -> u64 {
    self.peers.values().map(|v| v.over_extrapolations()).sum()
  }
}
