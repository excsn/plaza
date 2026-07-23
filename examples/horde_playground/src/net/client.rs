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
use plaza_client_utils::{ErrorSmoother, RttEstimator};
use plaza_ws::{CloseReason, Event, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::types::{Controls, LeaveReason, PlayerId, Vec2, ARENA_H, ARENA_W, PLAYER_SPEED};

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Waiting,
  Playing,
  NoSeat,
  Gone(String),
}

/// Integrates a held direction for `dt` seconds, clamped to the arena. The local
/// player is unforced (no gravity, nothing pushes it), so this is exactly what
/// the server does, which is what lets the client predict it without replaying a
/// per-tick input stream.
fn integrate(pos: Vec2, dir: Vec2, dt: f32) -> Vec2 {
  Vec2::new((pos.x + dir.x * PLAYER_SPEED * dt).clamp(0.0, ARENA_W), (pos.y + dir.y * PLAYER_SPEED * dt).clamp(0.0, ARENA_H))
}

fn lerp_pos(a: &Vec2, b: &Vec2, t: f32) -> Vec2 {
  Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

const SMOOTHING_SECS: f32 = 0.12;
const PING_INTERVAL_MS: u64 = 1000;
/// When coalescing, resend the held input at least this often, so a dropped
/// direction change cannot strand the player gliding until the next keypress.
const INPUT_KEEPALIVE_MS: u64 = 120;
/// Only pull the local prediction toward the server past this much drift. The
/// prediction is exact for an unforced player, so a small disagreement is clock
/// or one-way-estimate noise, not a real error; a large one means a lost input.
const LOCAL_CORRECT_PX: f32 = 24.0;
/// How many `Died` retractions in one packet read as an area pulse rather than
/// ordinary attrition. A nova clears a whole radius at once; single shots do not.
const NOVA_BURST: usize = 10;
/// How long the pulse ring is drawn for after the burst.
const NOVA_FLASH_SECS: f32 = 0.45;

pub struct NetClient {
  socket: Box<dyn Socket>,
  /// The same client the offline build runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  /// Your own player, integrated locally from the held direction and eased toward
  /// the server only when it drifts.
  pred: Vec2,
  smoother: ErrorSmoother<Vec2>,
  held_dir: Vec2,
  input_seq: u64,
  /// What was last transmitted, and when, for the coalesced send policy.
  last_sent_dir: Vec2,
  last_input_sent_ms: u64,
  rtt: RttEstimator,
  clock: ClockSyncEstimator,

  events: Vec<Event>,
  last_ping_ms: u64,
  pub frames_seen: u64,
  now_ms: u64,
  /// Seconds of area-pulse flash left to draw, refreshed on a death burst.
  nova_flash_secs: f32,
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
      pred: Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
      smoother: ErrorSmoother::new(SMOOTHING_SECS),
      held_dir: Vec2::default(),
      input_seq: 0,
      // An impossible first "last sent", so the very first input always transmits.
      last_sent_dir: Vec2::new(f32::NAN, f32::NAN),
      last_input_sent_ms: 0,
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      events: Vec::new(),
      last_ping_ms: 0,
      frames_seen: 0,
      now_ms: 0,
      nova_flash_secs: 0.0,
    })
  }

  /// Where to draw your own player: the prediction, eased through recent
  /// corrections.
  pub fn my_position(&self) -> Vec2 {
    self.smoother.sample(&self.pred, lerp_pos)
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
    self.held_dir = dir;
    self.pred = integrate(self.pred, dir, dt);
    self.input_seq += 1;

    let changed = (dir.x, dir.y) != (self.last_sent_dir.x, self.last_sent_dir.y);
    let keepalive_due = self.now_ms.saturating_sub(self.last_input_sent_ms) >= INPUT_KEEPALIVE_MS;
    if !controls.coalesce_input || changed || keepalive_due {
      let _ = self.socket.send_json(&Op::Input { seq: self.input_seq, dx: dir.x, dy: dir.y });
      self.last_sent_dir = dir;
      self.last_input_sent_ms = self.now_ms;
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
      && let Some((newest, mask)) = self.sim.acks.encode()
    {
      let _ = self.socket.send_json(&Op::Ack { newest, mask });
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
          self.status = Status::Playing;
        }
        Op::Policy(policy) => self.policy = Some(policy),
        Op::Frame(packet) => {
          let _ = controls;
          let deaths = packet.left.iter().filter(|(_, r)| *r == LeaveReason::Died).count();
          if deaths >= NOVA_BURST {
            self.nova_flash_secs = NOVA_FLASH_SECS;
          }
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (packet.server_time_ms as f64 + one_way) - now_ms as f64;
          self.clock.observe(now_ms as f64, offset);
          // Reconcile the local player. Its authoritative position is a one-way
          // delay old, so advance it by that with the held direction to estimate
          // where the server has it *now*, and only pull the prediction toward it
          // when they genuinely disagree, which for an unforced player means a lost
          // input under the coalesced policy rather than routine noise.
          if let Some(me) = self.me
            && let Some((_, pos)) = packet.players.iter().find(|(id, _)| *id == me)
          {
            let projected = integrate(*pos, self.held_dir, (one_way / 1000.0) as f32);
            if self.pred.dist(projected) > LOCAL_CORRECT_PX {
              let seen = self.my_position();
              self.pred = projected;
              self.smoother.begin_from(seen);
            }
          }
          // Hand the sim server time, not local time. It computes a packet's age
          // as `recv - server_time` to project a sample into the present, and that
          // is only meaningful if the two clocks agree; over a real wire they do
          // not, so give it this client's *estimate* of server-time-now. Before
          // sync converges this falls back to local time, which yields an age near
          // zero rather than a wild one.
          let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
          self.sim.on_packet(&packet, server_now, controls);
          // Feed the predicted local position back in, so the coin and repulsor
          // rules the sim runs read where you are, not where the packet put you.
          self.sim.set_local_pos(self.my_position());
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
        Op::Input { .. } | Op::Ack { .. } | Op::Buy(_) | Op::Ping { .. } => {}
      }
    }
    applied_frame
  }

  /// Advances the sim and the correction ease.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.sim.tick(dt_ms, controls);
    self.smoother.advance(dt_ms as f32 / 1000.0);
    self.sim.set_local_pos(self.my_position());
    if self.nova_flash_secs > 0.0 {
      self.nova_flash_secs = (self.nova_flash_secs - dt_ms as f32 / 1000.0).max(0.0);
    }
    if self.socket.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }
}

/// A little sugar so call sites are not full of `serde_json::to_vec`.
trait SendJson {
  fn send_json<T: serde::Serialize>(&self, value: &T) -> Result<(), plaza_ws::WsError>;
}

impl<S: Socket + ?Sized> SendJson for S {
  fn send_json<T: serde::Serialize>(&self, value: &T) -> Result<(), plaza_ws::WsError> {
    // A bare op, never an envelope: the server attaches who it came from, and a
    // client that could name itself could name somebody else.
    match serde_json::to_string(value) {
      Ok(text) => self.send_text(&text),
      Err(e) => Err(plaza_ws::WsError::Send(e.to_string())),
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
