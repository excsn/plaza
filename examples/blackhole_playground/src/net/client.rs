//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline playground uses, so the local
//! integration, the corrections and the rendering are unchanged. What it adds is
//! everything a shared clock and a function argument were standing in for:
//!
//! - **Your own movement is predicted.** Offline, the local player's input went
//!   straight into authoritative state at 60 Hz. Over a wire it costs a round
//!   trip, so the hole is predicted locally and reconciled against the server,
//!   through [`PredictedPlayer`]. Without it the camera follows a position a
//!   round trip old and the game feels broken in a way no readout would show.
//! - **The clock is estimated, not shared.** `age_ms = recv - packet.server_time_ms`
//!   was exact offline because both halves read one clock. Here it needs
//!   [`ClockSyncEstimator`] and [`RttEstimator`] over ping and pong.
//! - **The connection is a state, not an assumption.** Connecting, refused, and
//!   dropped are things a player has to be told about.

use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::{PlayerConfig, PredictedPlayer, RttEstimator};
use plaza_ws::{CloseReason, Event, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::types::{Controls, PlayerId, Vec2, ARENA_H, ARENA_W, DASH_COOLDOWN_MS, DASH_DURATION_MS, HOLE_SPEED};

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  /// Connected, but the server has not seated us yet.
  Waiting,
  Playing,
  /// Connected and the arena was full. A real outcome, not an error.
  NoSeat,
  Gone(String),
}

/// One tick of intent, replayed over authoritative state during reconciliation.
#[derive(Clone, Debug)]
pub struct MoveInput {
  pub dir: Vec2,
  pub dt: f32,
}

/// The client half of the movement rule.
///
/// Only the base speed. Dash is deliberately **not** predicted: it is a burst
/// the server grants subject to a cooldown the client cannot see, so predicting
/// it would mean guessing at a permission and snapping back whenever the guess
/// was wrong. Mispredicting continuous movement is invisible once eased;
/// mispredicting a discrete grant is not.
fn apply_move(pos: &mut Vec2, input: &MoveInput) {
  pos.x = (pos.x + input.dir.x * HOLE_SPEED * input.dt).clamp(0.0, ARENA_W);
  pos.y = (pos.y + input.dir.y * HOLE_SPEED * input.dt).clamp(0.0, ARENA_H);
}

fn lerp_pos(a: &Vec2, b: &Vec2, t: f32) -> Vec2 {
  Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// How long a correction to your own hole is eased over.
const SMOOTHING_SECS: f32 = 0.12;
/// How often to probe the round trip.
const PING_INTERVAL_MS: u64 = 1000;

pub struct NetClient {
  socket: Box<dyn Socket>,
  /// The same client the offline build runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  /// Your own hole, predicted locally and reconciled on every frame.
  local: PredictedPlayer<Vec2, MoveInput>,
  /// The newest sequence the server says it has applied, fed back into
  /// reconciliation. `PredictedPlayer` owns the counter itself, so the number it
  /// hands out is the number that goes on the wire: two counters would be two
  /// things to keep in step.
  acked_seq: u64,
  rtt: RttEstimator,
  clock: ClockSyncEstimator,

  events: Vec<Event>,
  last_ping_ms: u64,
  /// Frames applied, so a joiner can tell "connected but silent" from "playing".
  pub frames_seen: u64,

  /// This client's clock, mirrored from [`poll`](Self::poll) so [`send_input`]
  /// can time the local dash prediction without being handed the time again.
  ///
  /// [`send_input`]: Self::send_input
  now_ms: u64,
  /// Who the server says is mid-dash, from the last frame. Authoritative, and
  /// what makes a rival's dash visible.
  dashing: Vec<PlayerId>,
  /// The local dash prediction. The dash movement is deliberately not predicted
  /// (see [`apply_move`]), but the *effect* is: the burst should flash the
  /// instant you press it, not a round trip later, so a local cooldown mirror of
  /// the server's own rule decides when a press counts and lights it immediately.
  local_dash_until_ms: u64,
  local_dash_ready_ms: u64,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let socket = open(url)?;
    Ok(Self {
      socket,
      sim: SimClient::new(0),
      status: Status::Connecting,
      me: None,
      policy: None,
      local: PredictedPlayer::new(
        Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
        PlayerConfig {
          input_buffer: 256,
          smoothing_secs: SMOOTHING_SECS,
          // Eased in and out: a correction to your own hole should never start
          // or stop abruptly, or the player reads it as the controls stuttering
          // rather than as the network being corrected.
          easing: plaza_client_utils::smoothstep,
        },
        apply_move,
        lerp_pos,
      ),
      acked_seq: 0,
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      events: Vec::new(),
      last_ping_ms: 0,
      frames_seen: 0,
      now_ms: 0,
      dashing: Vec::new(),
      local_dash_until_ms: 0,
      local_dash_ready_ms: 0,
    })
  }

  /// Whether a hole should show the dash burst: the server's word for anyone,
  /// plus your own optimistic flash the moment you press it.
  pub fn is_dashing(&self, id: PlayerId) -> bool {
    if self.me == Some(id) && self.now_ms < self.local_dash_until_ms {
      return true;
    }
    self.dashing.contains(&id)
  }

  /// Where to draw your own hole: the eased prediction, not the last thing the
  /// server said.
  pub fn my_position(&self) -> Vec2 {
    self.local.render()
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt_ms()
  }

  /// The server's clock as this client estimates it, which is what the offline
  /// build got for free by sharing one.
  pub fn server_time_ms(&self, now_ms: u64) -> u64 {
    self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  /// Sends this frame's intent and advances the prediction.
  pub fn send_input(&mut self, dir: Vec2, dash: bool, dt: f32) {
    if !self.is_playing() {
      return;
    }
    // Predicted locally at once, and the sequence it returns is what the server
    // is asked to acknowledge. The buffered copy is replayed over whatever comes
    // back.
    let seq = self.local.input(MoveInput { dir, dt });
    // Light the burst immediately if the press would be granted. The mirror uses
    // the server's own cooldown, so it rarely disagrees; when it does, the cost
    // is a stray flash, never a mispredicted position.
    if dash && self.now_ms >= self.local_dash_ready_ms {
      self.local_dash_until_ms = self.now_ms + DASH_DURATION_MS;
      self.local_dash_ready_ms = self.now_ms + DASH_COOLDOWN_MS;
    }
    let _ = self.socket.send_json(&Op::Input {
      seq,
      dx: dir.x,
      dy: dir.y,
      dash,
    });
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
    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
        }
        Event::Text(text) => self.on_frame(text.as_bytes(), now_ms, controls),
        Event::Message(bytes) => self.on_frame(&bytes, now_ms, controls),
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
  }

  fn on_frame(&mut self, bytes: &[u8], now_ms: u64, controls: &Controls) {
    // The envelope is whatever `plaza_session` sends; only `Ops` matters here,
    // since the arena is built with join snapshots off.
    let Ok(message) = serde_json::from_slice::<plaza_wire::SessionMessage<Op, u64, ()>>(bytes) else {
      return;
    };
    let plaza_wire::SessionMessage::Ops { ops, .. } = message else {
      return;
    };
    for op in ops {
      match op {
        Op::Welcome { player, policy } => {
          self.me = Some(player);
          self.policy = Some(policy);
          self.sim = SimClient::new(player);
          self.status = Status::Playing;
        }
        Op::Frame(packet) => {
          // Clock sync is driven by pongs alone, not by frames. A frame's offset
          // is only right if the one-way estimate matches the delay the frame
          // actually took, and on a *host* (which talks to its own arena) those
          // disagree: pongs come straight back while the impairment link holds
          // frames, so a frame sample claims an offset a whole latency off and the
          // estimate wobbles every ping interval. Pongs are correct on a host
          // (direct) and a remote (delayed like the frames).
          if let Some(me) = self.me
            && let Some(hole) = packet.holes.iter().find(|(id, _)| *id == me)
          {
            // Reconcile before the packet is consumed: the authoritative
            // position is the basis, and everything sent since is replayed over
            // it by `PredictedPlayer`.
            self.local.reconcile(hole.1.pos, self.acked_seq);
          }
          self.dashing = packet.dashing.clone();
          self.sim.on_packet(&packet, now_ms, controls);
          self.frames_seen += 1;
        }
        // Recorded, not acted on: reconciliation needs an authoritative
        // *position* to go with the sequence, and that only arrives with a
        // frame. Acks come far more often than frames, so this keeps the newest.
        Op::Ack { seq } => self.acked_seq = self.acked_seq.max(seq),
        Op::Pong { origin_ms, server_ms } => {
          self.rtt.observe_pong(origin_ms, now_ms);
          // The server stamped `server_ms` when it replied, which is roughly one
          // way back in time from now. Correcting by the estimated one-way delay
          // is what turns a raw sample into an offset worth fitting.
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (server_ms as f64 + one_way) - now_ms as f64;
          self.clock.observe(now_ms as f64, offset);
        }
        // Client-to-server variants coming back would mean a confused server.
        Op::Input { .. } | Op::Ping { .. } => {}
      }
    }
  }

  /// Advances local integration and the correction ease.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.sim.tick(dt_ms, controls);
    self.local.advance(dt_ms as f32 / 1000.0);
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
    // A bare op, never an envelope: the server attaches who it came from, and
    // a client that could name itself could name somebody else.
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
