//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline playground uses, so the relevance
//! mirror, the correction smoothing, the coins and the drawing are unchanged. What
//! it adds is everything a shared clock and a function argument were standing in
//! for:
//!
//! - **Your own movement is predicted.** Offline, the local player's input went
//!   straight into authoritative state at 60 Hz. Over a wire it costs a round
//!   trip, so the player is predicted locally and reconciled against the server,
//!   through [`PredictedPlayer`]. The predicted position is also fed back into the
//!   wrapped sim, because the coin and repulsor rules read the local player and
//!   should read where you actually are, not where a packet last put you.
//! - **The entity stream is acknowledged over the real wire.** The sim already
//!   tracks which relevance packets it holds; this sends that acknowledgement up
//!   so the server's loss recovery has something to diff against.
//! - **The area pulse is inferred from the death burst it causes**, rather than
//!   read from server state a joiner does not have.

use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::{CorrectionMonitor, HeldInputConfig, HeldInputPredictor, InputCoalescer, RttEstimator};
use plaza_ws::{CloseReason, Event, SendJson, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{step_player, Controls, LeaveReason, PlayerId, Vec2, ARENA_H, ARENA_W, SIM_DT};

/// The server's simulation step, which is the unit its ticks are counted in.
const SIM_STEP_MS: u64 = (SIM_DT * 1000.0) as u64;

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Waiting,
  Playing,
  NoSeat,
  Gone(String),
}

/// The prediction rule, which is literally the server's own [`step_player`]
/// rather than a client copy of it. The context is `()` because a player is
/// unforced: nothing but its own input moves it.
fn integrate(pos: &mut Vec2, dir: &Vec2, dt: f32, _ctx: &()) {
  step_player(pos, *dir, dt);
}

fn lerp_pos(a: &Vec2, b: &Vec2, t: f32) -> Vec2 {
  Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

const PING_INTERVAL_MS: u64 = 1000;
/// When coalescing, resend the held input at least this often, so a dropped
/// direction change cannot strand the player gliding until the next keypress.
/// See [`InputCoalescer`], which is where that reasoning now lives.
const INPUT_KEEPALIVE_MS: u64 = 120;
/// A disagreement past this much, in a single packet, is a real discontinuity
/// (the first spawn, a respawn, a teleport) rather than drift, and is snapped
/// outright. Ordinary drift is eased continuously instead, see `CORRECT_BLEND`.
const LOCAL_CORRECT_PX: f32 = 24.0;
/// Fraction of the remaining gap to the server the local prediction closes each
/// packet. The prediction is close to exact for an unforced player, so what is
/// left is a slow drift from clock and one-way-estimate noise; easing a fixed
/// share of it every packet keeps the marker within a pixel or two of the server
/// without ever accumulating into the visible snap a hard threshold produced.
const CORRECT_BLEND: f32 = 0.25;
/// A gap larger than this is not drift to ease but a discontinuity to snap.
const TELEPORT_PX: f32 = 200.0;
/// How many `Died` retractions in one packet read as an area pulse rather than
/// ordinary attrition. A nova clears a whole radius at once; single shots do not.
const NOVA_BURST: usize = 10;
/// How long the pulse ring is drawn for after the burst.
const NOVA_FLASH_SECS: f32 = 0.45;
/// How long the red damage flash is drawn for after a hit.
const HIT_FLASH_SECS: f32 = 0.35;

pub struct NetClient {
  socket: Box<dyn Socket>,
  /// The same client the offline build runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  /// Your own player.
  ///
  /// The server holds your direction and integrates it every tick rather than
  /// consuming one input per step, which is what makes this the continuous model
  /// and why replaying inputs would double count. It is also what lets the input
  /// be coalesced: what is transmitted is a bandwidth decision, what is
  /// integrated is a simulation one.
  local: HeldInputPredictor<Vec2, Vec2>,
  input_seq: u64,
  /// When to actually transmit, as opposed to when to integrate. The two are
  /// deliberately different: the prediction advances every tick whatever the wire
  /// is doing, so a quiet wire is not a stuttering player.
  send_policy: InputCoalescer<Vec2>,
  rtt: RttEstimator,
  clock: ClockSyncEstimator,

  events: Vec<Event>,
  last_ping_ms: u64,
  pub frames_seen: u64,
  now_ms: u64,
  /// Seconds of area-pulse flash left to draw, refreshed on a death burst.
  nova_flash_secs: f32,
  /// Your own health last frame, to catch the moment it drops.
  prev_health: u8,
  /// Seconds of red damage flash left to draw, refreshed when you take a hit.
  hit_flash_secs: f32,
  /// What the local prediction is costing, measured against a running norm
  /// rather than a fixed pixel count so it stays meaningful as the send rate and
  /// latency change.
  pub monitor: CorrectionMonitor,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let socket = open(url)?;
    Ok(Self {
      socket,
      sim: SimClient::new(0, 1),
      status: Status::Connecting,
      me: None,
      policy: None,
      local: HeldInputPredictor::new(
        Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
        HeldInputConfig { blend: CORRECT_BLEND },
        integrate,
        lerp_pos,
      )
      // Beyond this the player did not travel there: a spawn or a respawn. Snap
      // it, because easing would slide the marker across the whole arena.
      .with_teleport(|a: &Vec2, b: &Vec2| a.dist(*b), TELEPORT_PX),
      input_seq: 0,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      events: Vec::new(),
      last_ping_ms: 0,
      frames_seen: 0,
      now_ms: 0,
      nova_flash_secs: 0.0,
      prev_health: crate::sim::types::PLAYER_MAX_HEALTH as u8,
      hit_flash_secs: 0.0,
      monitor: CorrectionMonitor::new().with_floor(LOCAL_CORRECT_PX),
    })
  }

  /// Your own health, `0..=PLAYER_MAX_HEALTH`, or full before a seat is known.
  pub fn my_health(&self) -> u8 {
    self.me.and_then(|m| self.sim.player_health.get(m as usize)).copied().unwrap_or(crate::sim::types::PLAYER_MAX_HEALTH as u8)
  }

  /// How long ago you last took a hit, while the red flash is worth drawing.
  pub fn hit_flash_age(&self) -> Option<f32> {
    (self.hit_flash_secs > 0.0).then_some(HIT_FLASH_SECS - self.hit_flash_secs)
  }

  /// Where to draw your own player: the prediction, eased through recent
  /// corrections.
  pub fn my_position(&self) -> Vec2 {
    self.local.render()
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt_ms()
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  /// How long ago the last area pulse fired, while still worth drawing.
  pub fn nova_flash_age(&self) -> Option<f32> {
    (self.nova_flash_secs > 0.0).then_some(NOVA_FLASH_SECS - self.nova_flash_secs)
  }

  /// Advances the local prediction and transmits the intent, either every tick or
  /// only on change, per `controls.coalesce_input`.
  pub fn send_input(&mut self, dir: Vec2, dt: f32, controls: &Controls) {
    if !self.is_playing() {
      return;
    }
    // Predict locally every tick regardless of what is transmitted, so movement
    // is smooth even when the wire stays quiet.
    self.local.hold(dir);
    self.local.advance(dt);
    self.input_seq += 1;

    self.send_policy.set_enabled(controls.coalesce_input);
    if self.send_policy.should_send(&dir, self.now_ms) {
      // The tick this input is *for*, computed the same way by every client, so
      // two players pressing at the same instant name the same one however far
      // apart their pings are. Its own estimate of the server clock, plus the
      // playout depth the server advertised, in the server's step units.
      //
      // The server decides whether that tick is still open. This is an intention,
      // not a claim.
      let server_now = self.clock.server_time_at(self.now_ms as f64).unwrap_or(self.now_ms as f64).max(0.0) as u64;
      let depth = self.policy.map(|p| p.playout_delay_ms).unwrap_or(0);
      let tick = (server_now + depth) / SIM_STEP_MS;
      let _ = self.socket.send_json(&Op::Input { seq: self.input_seq, dx: dir.x, dy: dir.y, tick });
    }
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      let _ = self.socket.send_json(&Op::Ping { origin_ms: now_ms });
    }

    self.socket.poll(&mut self.events);
    let events = std::mem::take(&mut self.events);
    let mut applied_a_frame = false;
    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          // Before anything else: say which wire format this build speaks, so a
          // stale page is told to reload rather than half-working.
          let _ = self.socket.send_json(&Op::Hello { protocol: PROTOCOL });
        }
        Event::Text(text) => applied_a_frame |= self.on_frame(text.as_bytes(), now_ms, controls),
        Event::Message(bytes) => applied_a_frame |= self.on_frame(&bytes, now_ms, controls),
        Event::Closed(reason) => {
          self.status = Status::Gone(match reason {
            CloseReason::Local => "you disconnected".to_owned(),
            CloseReason::Remote { code, reason } if reason.is_empty() => format!("host closed the connection ({code})"),
            CloseReason::Remote { reason, .. } => reason,
            CloseReason::Error(e) => e,
          });
        }
      }
    }
    self.events.clear();

    // Acknowledge what we now hold, so the server's loss recovery can diff
    // against a state we provably reached. On receipt, as the offline world does,
    // so the baseline advances as fast as the link allows.
    if applied_a_frame
      && let Some((newest, mask)) = self.sim.acks().encode()
    {
      let _ = self.socket.send_json(&Op::Ack { newest, mask, digest: self.sim.last_digest() });
    }

    // A purchase is a request on the same wire as anything else. Ask once; the
    // sim tracks the pending set so it does not spam until the answer lands.
    if controls.coins && controls.auto_buy && self.is_playing()
      && let Some(upgrade) = self.sim.wants_to_buy()
    {
      let _ = self.socket.send_json(&Op::Buy(upgrade));
    }
  }

  fn on_frame(&mut self, bytes: &[u8], now_ms: u64, controls: &Controls) -> bool {
    let Ok(message) = serde_json::from_slice::<plaza_wire::SessionMessage<Op, u64, ()>>(bytes) else {
      return false;
    };
    let plaza_wire::SessionMessage::Ops { ops, .. } = message else {
      return false;
    };
    let mut applied_frame = false;
    for op in ops {
      match op {
        Op::Welcome { player, policy } => {
          self.me = Some(player);
          self.policy = Some(policy);
          self.sim = SimClient::new(player, policy.player_count);
          self.sim.set_render_delay(policy.render_delay_ms);
          self.status = Status::Playing;
        }
        Op::Policy(policy) => {
          self.policy = Some(policy);
          self.sim.set_render_delay(policy.render_delay_ms);
        }
        // The player stream. It arrives far more often than a frame and carries
        // no deltas, so it is deliberately outside the sequence, acknowledgement
        // and digest machinery the entity stream runs on.
        Op::Players(frame) => {
          let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
          self.sim.on_player_frame(&frame, server_now);
        }
        Op::Frame(packet) => {
          let _ = controls;
          let deaths = packet.left.iter().filter(|(_, r)| *r == LeaveReason::Died).count();
          if deaths >= NOVA_BURST {
            self.nova_flash_secs = NOVA_FLASH_SECS;
          }
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          // Clock sync is driven by pongs alone, not by frames. A frame's offset
          // is only right if the one-way estimate matches the delay the frame
          // actually took, and on a *host* those disagree: pongs come straight
          // back (RTT ~0) while the impairment link holds frames, so a frame
          // sample claims an offset 80 ms off. Mixing the two made the estimate
          // wobble every ping interval and jerk the enemy projection backward.
          // Pongs are correct on a host (direct) and a remote (delayed like the
          // frames), so they are the honest source.
          // Reconcile the local player. Its authoritative position is a one-way
          // delay old, so advance it by that with the held direction to estimate
          // where the server has it *now*, and only pull the prediction toward it
          // when they genuinely disagree, which for an unforced player means a lost
          // input under the coalesced policy rather than routine noise.
          if let Some(me) = self.me
            && let Some((_, pos)) = packet.players.iter().find(|(id, _)| *id == me)
          {
            // The predictor projects the authoritative position forward by its own
            // age under the held direction, then eases toward it: drift is
            // absorbed continuously rather than accumulating into the visible
            // snap a hard threshold used to produce. A gap past `TELEPORT_PX` is
            // a discontinuity and snaps instead, which the predictor decides.
            let correction = self.local.reconcile(*pos, (one_way / 1000.0) as f32);
            let moved = correction.seen.dist(correction.settled);
            if self.monitor.record(moved) && controls.debug_digest {
              // Which way the correction went, relative to where the player is
              // steering. A forward correction is the surprising one: it means
              // the server got further than the prediction did.
              let delta = Vec2::new(correction.settled.x - correction.seen.x, correction.settled.y - correction.seen.y);
              let held = *self.local.held();
              let forward = delta.x * held.x + delta.y * held.y;
              eprintln!(
                "player correction seq={} moved={:.1}px (norm {:.1}px) {} one_way={:.1}ms held=({:.2},{:.2})",
                packet.seq,
                moved,
                self.monitor.norm(),
                if forward > 0.0 { "FORWARD" } else if forward < 0.0 { "back" } else { "lateral" },
                one_way,
                held.x,
                held.y,
              );
            }
          }
          // Hand the sim server time, not local time. It computes a packet's age
          // as `recv - server_time` to project a sample into the present, and that
          // is only meaningful if the two clocks agree; over a real wire they do
          // not, so give it this client's *estimate* of server-time-now. Before
          // sync converges this falls back to local time, which yields an age near
          // zero rather than a wild one.
          let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
          self.sim.receive_packet(*packet, server_now);
          // Deliberately do NOT overwrite the local player with the prediction
          // here. The sim aims enemies at the player position, and the server aims
          // them at its *authoritative* one; feeding the prediction in instead made
          // the client chase a position the server was not, so every sample
          // snapped the enemies between the two and they appeared to lunge at the
          // player whenever it moved. The prediction drives only the camera and
          // your own marker; the shared rules use the authoritative position, the
          // same as the offline build, which is smooth.
          // A drop in your own health is a hit worth flashing.
          let health = self.my_health();
          if health < self.prev_health {
            self.hit_flash_secs = HIT_FLASH_SECS;
          }
          self.prev_health = health;
          self.frames_seen += 1;
          applied_frame = true;
        }
        // The server still acknowledges movement, but a velocity-predicted local
        // player has no per-input replay to retire, so nothing needs it.
        Op::InputAck { .. } => {}
        Op::Pong { origin_ms, server_ms } => {
          self.rtt.observe_pong(origin_ms, now_ms);
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (server_ms as f64 + one_way) - now_ms as f64;
          self.clock.observe(now_ms as f64, offset);
        }
        Op::Outdated { server, client } => {
          self.status = Status::Gone(format!(
            "this page was built for wire format {client} and the server speaks {server}: reload to get the current client"
          ));
        }
        Op::Input { .. } | Op::Ack { .. } | Op::Buy(_) | Op::Ping { .. } | Op::Hello { .. } => {}
      }
    }
    applied_frame
  }

  /// Advances the sim and the correction ease.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.sim.tick(dt_ms, controls);
    if self.nova_flash_secs > 0.0 {
      self.nova_flash_secs = (self.nova_flash_secs - dt_ms as f32 / 1000.0).max(0.0);
    }
    if self.hit_flash_secs > 0.0 {
      self.hit_flash_secs = (self.hit_flash_secs - dt_ms as f32 / 1000.0).max(0.0);
    }
    if self.socket.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::native::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::miniquad::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}
