//! One pilot's belief about the field.
//!
//! The unusual part: **the client does not receive the curtain, it computes
//! it.** Everything the server knows about the enemy bullets, this knows too,
//! from the same closed form and the same handful of wave announcements. The
//! two ends never disagree about a bullet. They disagree about where the ship
//! was, which is why the death question has three answers and why only one of
//! them is both fair and checkable.

use std::collections::{BTreeMap, VecDeque};

use plaza_client_utils::{InterpolationClock, RemoteView, RenderOpts};
use plaza_server_utils::{InputSchedule, InputWindow};

use crate::sim::curtain::{Bullet, Downed, Wave, curtain_at};
use crate::sim::protocol::{DeathEvent, Frame, Op, ServerPolicy};
use crate::sim::types::{
  Controls, DeathRule, Dir8, ENEMY_BULLET_R, FIELD_H, FIELD_W, PlayerBullet, PlayerId, SHIP_R, SHIP_SPEED, SIM_STEP_MS, Ship, V2,
};

const SNAP_PX: f32 = 1.5;
const REPLAY_TICKS: usize = 240;

#[derive(Clone, Copy, Debug)]
struct ShipSnap {
  pos: V2,
  alive: bool,
}

impl plaza_client_utils::interpolation::Interpolatable<u64> for ShipSnap {
  fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
    Self {
      pos: self.pos.lerp(other.pos, t),
      alive: self.alive,
    }
  }
}

impl plaza_client_utils::extrapolation::Extrapolatable<V2, f32> for ShipSnap {
  fn extrapolate_with_velocity(&self, velocity: &V2, dt: f32) -> Self {
    Self {
      pos: self.pos.add(velocity.scale(dt)),
      alive: self.alive,
    }
  }
}

#[derive(Clone, Debug, Default)]
pub struct ClientStats {
  pub snaps: u64,
  pub snap_px: f32,
  pub frames_seen: u64,
  /// Contacts this client saw against its own predicted position.
  pub contacts_seen: u64,
  pub declared: u64,
  /// Deaths that arrived for a tick where this client's own view had no
  /// contact at all. Only latency that costs an *input* can produce one.
  pub deaths_i_had_dodged: u64,

  pub deaths_felt: u64,
  /// Ticks spent flying a ship this client already knew was hit.
  ///
  /// The number that condemns `ServerOnly`, and it is not the one this example
  /// was planned around. A derivable curtain means the client is never the last
  /// to know: it computed the same field the server did and saw the contact at
  /// the same tick. So the rule does not decide *who finds out*, it decides
  /// **who is allowed to act on it**, and under `ServerOnly` the player watches
  /// themself keep flying for a round trip after they know they are dead.
  /// Which is worse than not knowing.
  pub flown_while_dead_ticks: u64,
  /// Bullets drawn from a wave whose emitter had already been shot down,
  /// before the op saying so arrived.
  pub phantom_bullets: u64,
}

pub struct Client {
  pub me: PlayerId,
  pub policy: Option<ServerPolicy>,
  pub stats: ClientStats,

  pub ships: Vec<Ship>,
  pub bullets: Vec<PlayerBullet>,
  pub waves: Vec<Wave>,
  pub downed: Vec<Downed>,
  pub deaths: VecDeque<DeathEvent>,

  clock: InterpolationClock<u64>,
  peers: BTreeMap<PlayerId, RemoteView<ShipSnap, V2>>,

  predicted: V2,
  predicted_alive: bool,
  schedule: InputSchedule<Dir8>,
  history: VecDeque<(u64, Dir8)>,
  /// Where this client *drew itself* on each recent tick.
  ///
  /// Needed to answer the only question that condemns `ServerOnly`, and it has
  /// to be a record rather than the current position: "was there anything to
  /// dodge at the tick I was killed on" is a question about the past, and
  /// asking it of the present answers a different question that happens to
  /// compile.
  pos_history: VecDeque<(u64, V2)>,
  sim_tick: u64,
  held: Dir8,
  last_pressed: Dir8,

  input_seq: u64,
  acked_seq: u64,
  last_input_tick: u64,

  /// Reused buffer for the derived curtain.
  curtain: Vec<Bullet>,
  /// The tick this client last told the server it was hit on, so one contact
  /// does not become a stream of declarations.
  declared_tick: Option<u64>,
  invuln_until_tick: u64,
  /// Set when this client acted on a contact it found itself, rather than
  /// waiting to be told.
  self_marked: Option<u64>,

  newest_stamp_ms: u64,
  started: bool,
}

impl Client {
  pub fn new(me: PlayerId, render_delay_ms: u64) -> Self {
    Self {
      me,
      policy: None,
      stats: ClientStats::default(),
      ships: Vec::new(),
      bullets: Vec::new(),
      waves: Vec::new(),
      downed: Vec::new(),
      deaths: VecDeque::new(),
      clock: InterpolationClock::new(render_delay_ms),
      peers: BTreeMap::new(),
      predicted: V2::new(FIELD_W * 0.5, FIELD_H - 70.0),
      predicted_alive: true,
      schedule: InputSchedule::new(),
      history: VecDeque::new(),
      pos_history: VecDeque::new(),
      sim_tick: 0,
      held: Dir8::Still,
      last_pressed: Dir8::Still,
      input_seq: 0,
      acked_seq: 0,
      last_input_tick: 0,
      curtain: Vec::new(),
      declared_tick: None,
      invuln_until_tick: 0,
      self_marked: None,
      newest_stamp_ms: 0,
      started: false,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.started
  }

  pub fn render_at(&self) -> Option<u64> {
    self.clock.target()
  }

  pub fn sim_tick(&self) -> u64 {
    self.sim_tick
  }

  pub fn input_seq(&self) -> u64 {
    self.input_seq
  }

  pub fn acked_seq(&self) -> u64 {
    self.acked_seq
  }

  pub fn last_input_tick(&self) -> u64 {
    self.last_input_tick
  }

  pub fn predicted_pos(&self) -> V2 {
    self.predicted
  }

  /// The curtain this client derived. Never received.
  pub fn curtain(&self) -> &[Bullet] {
    &self.curtain
  }

  pub fn lead_ticks(&self) -> i64 {
    self.sim_tick as i64 - (self.newest_stamp_ms / SIM_STEP_MS) as i64
  }

  pub fn aim_tick(&self, server_time_ms: u64) -> u64 {
    let depth = self.policy.map(|p| p.playout_delay_ms).unwrap_or(0);
    (server_time_ms + depth) / SIM_STEP_MS
  }

  fn window(&self) -> InputWindow {
    InputWindow {
      max_late: self.policy.map(|p| p.input_max_late_ticks).unwrap_or(4),
      max_early: self.policy.map(|p| p.input_max_early_ticks).unwrap_or(30),
    }
  }

  pub fn press(&mut self, dir: Dir8, server_time_ms: u64) -> Option<Op> {
    if !self.started || dir == self.last_pressed {
      return None;
    }
    Some(self.schedule_walk(dir, server_time_ms))
  }

  pub fn schedule_walk(&mut self, dir: Dir8, server_time_ms: u64) -> Op {
    self.last_pressed = dir;
    self.input_seq += 1;
    let tick = self.aim_tick(server_time_ms);
    self.last_input_tick = tick;
    // Scheduled for the tick it named, never applied on the press. Applying it
    // now runs the input a playout depth before the server does, and every
    // frame then arrives as a correction.
    self.schedule.submit(tick, dir, self.sim_tick, self.window());
    Op::Move { seq: self.input_seq, tick, dir }
  }

  pub fn fire(&mut self, server_time_ms: u64) -> Option<Op> {
    if !self.started {
      return None;
    }
    self.input_seq += 1;
    let tick = self.aim_tick(server_time_ms);
    self.last_input_tick = tick;
    Some(Op::Fire { seq: self.input_seq, tick })
  }

  pub fn on_op(&mut self, op: Op, _now_ms: u64) {
    match op {
      Op::Welcome { player, policy, start } => {
        self.me = player;
        self.policy = Some(policy);
        self.clock.set_delay(policy.render_delay_ms);
        self.ships = start.ships.clone();
        // Every wave already in flight. Without these a joiner flies through a
        // curtain it cannot see, which is the one failure mode a derived field
        // has that a streamed one does not.
        self.waves = start.waves.clone();
        self.downed = start.downed.clone();
        self.sim_tick = start.tick;
        self.newest_stamp_ms = start.server_time_ms;
        self.predicted = start.ships.iter().find(|s| s.id == player).map(|s| s.pos).unwrap_or(self.predicted);
        self.predicted_alive = true;
        self.started = true;
      }
      Op::Frame(frame) => self.on_frame(*frame),
      Op::WaveUp(wave) => {
        if !self.waves.iter().any(|w| w.id == wave.id) {
          self.waves.push(*wave);
        }
      }
      Op::ArmDown(down) => {
        if !self.downed.iter().any(|d| d.wave == down.wave && d.arm == down.arm) {
          self.downed.push(down);
        }
      }
      Op::Died(death) => {
        if death.victim == self.me {
          // Asked of this client's own record, at the tick named: was there
          // anything to dodge where I was drawing myself?
          if let Some((_, at)) = self.pos_history.iter().find(|(t, _)| *t == death.at_tick) {
            let mut scratch = Vec::new();
            if !crate::sim::curtain::contact(&self.waves, &self.downed, death.at_tick, *at, SHIP_R, &mut scratch) {
              self.stats.deaths_i_had_dodged += 1;
            }
          }
          // Nothing if this client already acted on the contact it found for
          // itself; a whole round trip if it was made to wait to be told.
          self.stats.deaths_felt += 1;
          if self.self_marked.take().is_none() {
            self.stats.flown_while_dead_ticks += self.sim_tick.saturating_sub(death.at_tick);
          }
          self.invuln_until_tick = self.sim_tick + (crate::sim::types::INVULN_MS / SIM_STEP_MS);
          self.declared_tick = None;
        }
        if self.deaths.len() == 16 {
          self.deaths.pop_front();
        }
        self.deaths.push_back(*death);
      }
      Op::InputAck { seq } => self.acked_seq = self.acked_seq.max(seq),
      Op::NoSeat { .. } | Op::Refused { .. } | Op::Move { .. } | Op::Fire { .. } | Op::Struck { .. } => {}
    }
  }

  fn on_frame(&mut self, frame: Frame) {
    self.stats.frames_seen += 1;
    self.newest_stamp_ms = self.newest_stamp_ms.max(frame.server_time_ms);
    self.clock.observe(frame.server_time_ms);
    self.bullets = frame.bullets.clone();

    for ship in &frame.ships {
      if ship.id == self.me {
        continue;
      }
      let view = self.peers.entry(ship.id).or_insert_with(|| RemoteView::new(16, 250));
      view.push(
        frame.server_time_ms,
        ShipSnap { pos: ship.pos, alive: ship.alive },
        ship.dir.unit().scale(SHIP_SPEED),
      );
    }

    if let Some(mine) = frame.ships.iter().find(|s| s.id == self.me) {
      let before = self.predicted;
      self.predicted = mine.pos;
      self.predicted_alive = mine.alive;
      if frame.tick < self.sim_tick {
        let dt = SIM_STEP_MS as f32 / 1000.0;
        for tick in (frame.tick + 1)..=self.sim_tick {
          let dir = self.history.iter().rev().find(|(t, _)| *t == tick).map(|(_, d)| *d).unwrap_or(Dir8::Still);
          self.predicted = clamp_field(self.predicted.add(dir.unit().scale(SHIP_SPEED * dt)));
        }
      }
      let moved = before.dist(self.predicted);
      if moved > SNAP_PX {
        self.stats.snaps += 1;
        self.stats.snap_px += moved;
      }
    }
    self.ships = frame.ships;
  }

  /// Advances the prediction, re-derives the curtain, and decides whether this
  /// client believes it has just been hit.
  ///
  /// Returns a declaration to send, when the rule asks for one. Returning it
  /// rather than sending it keeps this file free of sockets, which is what lets
  /// the offline harness run the identical code.
  pub fn advance(&mut self, elapsed_ms: u64, server_time_ms: u64, controls: &Controls) -> Option<Op> {
    self.clock.advance(elapsed_ms);

    let target_tick = server_time_ms / SIM_STEP_MS;
    let dt = SIM_STEP_MS as f32 / 1000.0;
    while self.sim_tick < target_tick {
      self.sim_tick += 1;
      if let Some(dir) = self.schedule.execute_due(self.sim_tick) {
        self.held = dir;
      }
      if self.history.len() == REPLAY_TICKS {
        self.history.pop_front();
      }
      self.history.push_back((self.sim_tick, self.held));
      if controls.predict_self && self.predicted_alive {
        self.predicted = clamp_field(self.predicted.add(self.held.unit().scale(SHIP_SPEED * dt)));
      }
      if self.pos_history.len() == REPLAY_TICKS {
        self.pos_history.pop_front();
      }
      self.pos_history.push_back((self.sim_tick, self.predicted));
    }

    // The curtain, derived. Nothing about this arrived from anywhere.
    if controls.derive_curtain {
      curtain_at(&self.waves, &self.downed, self.sim_tick, &mut self.curtain);
    } else {
      self.curtain.clear();
    }

    if !self.started || self.sim_tick < self.invuln_until_tick {
      return None;
    }

    let reach = SHIP_R + ENEMY_BULLET_R;
    let touched = self.curtain.iter().any(|b| b.pos.dist(self.predicted) <= reach);
    if !touched {
      return None;
    }
    self.stats.contacts_seen += 1;

    // Only the rules that ask. Under `ServerOnly` a declaration is noise the
    // server would ignore, and sending it anyway would make the panel's
    // "declared" count a lie about which rule is running.
    let rule = self.policy.map(|p| p.death_rule).unwrap_or(controls.death_rule);
    if rule == DeathRule::ServerOnly {
      return None;
    }
    if self.declared_tick.is_some_and(|t| self.sim_tick.saturating_sub(t) < 30) {
      return None;
    }
    // The seat that has stopped owning up. Under `ClientDeclares` this is an
    // immortal ship; the interesting part is what it costs the server to see.
    if controls.silent_seat && self.me == 0 {
      return None;
    }
    // Acted on here rather than when the verdict returns. That is the whole
    // difference between the rules: the contact is not news to this client,
    // only the permission to react to it is.
    self.declared_tick = Some(self.sim_tick);
    self.self_marked = Some(self.sim_tick);
    self.invuln_until_tick = self.sim_tick + (crate::sim::types::INVULN_MS / SIM_STEP_MS);
    self.stats.declared += 1;
    self.input_seq += 1;
    Some(Op::Struck { seq: self.input_seq, tick: self.sim_tick })
  }

  /// Where this client would draw every ship.
  pub fn render(&self, controls: &Controls) -> Vec<(PlayerId, V2, bool)> {
    let target = self.render_at();
    let opts = RenderOpts { interpolate: true, extrapolate: false };
    let mut out = Vec::with_capacity(self.ships.len());
    for ship in &self.ships {
      if ship.id == self.me {
        let pos = if controls.predict_self { self.predicted } else { ship.pos };
        out.push((ship.id, pos, self.predicted_alive));
      } else if let Some(view) = self.peers.get(&ship.id) {
        match view.render(target, opts) {
          Some(snap) => out.push((ship.id, snap.pos, snap.alive)),
          None => out.push((ship.id, ship.pos, ship.alive)),
        }
      } else {
        out.push((ship.id, ship.pos, ship.alive));
      }
    }
    out
  }

  pub fn my_ship(&self) -> Option<&Ship> {
    self.ships.iter().find(|s| s.id == self.me)
  }
}

fn clamp_field(p: V2) -> V2 {
  V2::new(p.x.clamp(SHIP_R, FIELD_W - SHIP_R), p.y.clamp(SHIP_R, FIELD_H - SHIP_R))
}
