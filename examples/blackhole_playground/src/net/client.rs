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
use plaza_client_utils::{CorrectionMonitor, PlayerConfig, PredictedPlayer, RttEstimator};
use plaza_ws::{CloseReason, Event, SendJson, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Attractor, Controls, PlayerId, Vec2, ARENA_H, ARENA_W, DASH_COOLDOWN_MS, DASH_DURATION_MS, DASH_SPEED_MULT, HOLE_PULL_SCALE, HOLE_SPEED, MAX_HOLE_PULL};

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
  /// Whether this step is a dash, per the local mirror, and only when the dash
  /// switch is on. When set, the base move runs at dash speed, so the dash burst
  /// is predicted rather than corrected for a round trip later.
  pub dash: bool,
}

/// The world the prediction runs in: the other holes pulling on yours.
///
/// The hole is a **forced** entity, moved by more than its own input, so the
/// client cannot predict it from the input alone. This is what
/// [`PredictedPlayer::set_context`] exists for, and refreshing it each packet is
/// what lets the client run the server's rule rather than a lesser copy of it.
///
/// [`PredictedPlayer::set_context`]: plaza_client_utils::PredictedPlayer::set_context
pub type Field = Vec<Attractor>;

/// The movement rule, the same one the server runs.
///
/// Both of the server's continuous passes are here: the steered move, and the
/// gravitational attraction toward every other hole. Leaving the second one out
/// is what made the hole jerk constantly, because the pull is tuned above walking
/// speed at close range, so the unmodelled term was largest exactly when it was
/// most visible.
///
/// What is still not predicted is the *contact separation* between two touching
/// holes, which would need the other holes' motion predicted too, and that means
/// running the whole field forward rather than one entity. It is the residual
/// visible during a close grapple, and it is a deliberate stopping point.
fn apply_move(pos: &mut Vec2, input: &MoveInput, field: &Field) {
  // Base speed, boosted to dash speed when this step is a predicted dash. The
  // server multiplies the same base speed in the same direction, so this is the
  // whole of the dash: no separate vector, just a faster walk.
  let speed = if input.dash { HOLE_SPEED * DASH_SPEED_MULT } else { HOLE_SPEED };
  pos.x = (pos.x + input.dir.x * speed * input.dt).clamp(0.0, ARENA_W);
  pos.y = (pos.y + input.dir.y * speed * input.dt).clamp(0.0, ARENA_H);
  // Then the pull, integrated from this step's own position so the direction
  // stays right as the hole moves, exactly as the server's `attract_holes` does.
  let (mut vx, mut vy) = (0.0f32, 0.0f32);
  for a in field {
    let (dx, dy) = (a.pos.x - pos.x, a.pos.y - pos.y);
    let r2 = (dx * dx + dy * dy).max(1.0);
    let pull = (HOLE_PULL_SCALE * a.pull / r2).min(MAX_HOLE_PULL);
    let inv_r = 1.0 / r2.sqrt();
    vx += dx * inv_r * pull;
    vy += dy * inv_r * pull;
  }
  pos.x = (pos.x + vx * input.dt).clamp(0.0, ARENA_W);
  pos.y = (pos.y + vy * input.dt).clamp(0.0, ARENA_H);
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
  local: PredictedPlayer<Vec2, MoveInput, Field>,

  /// Whether to predict the dash burst, mirrored from the panel each frame. On,
  /// the prediction moves at dash speed and the burst is smooth; off, the dash is
  /// left unpredicted and arrives as a correction. The switch between the two.
  predict_dash: bool,
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
  /// What the prediction is costing, and whether any of it is abnormal.
  ///
  /// The adaptive part matters: a correction of thirty pixels means nothing
  /// without knowing the send rate, the latency, and how much contact the game is
  /// in, so the monitor learns the norm and reports departures from it rather
  /// than tripping a constant tuned once against one configuration.
  pub monitor: CorrectionMonitor,

  /// Two shadow predictions of your hole, fed the same inputs as the real one but
  /// one always predicting the dash and one never, reconciled the same way. Their
  /// running mean corrections are a live, same-gameplay A/B of what predicting the
  /// dash is actually worth, independent of the render switch. Cheap only because
  /// a predictor is pure and holds nothing.
  ab_dash: PredictedPlayer<Vec2, MoveInput, Field>,
  ab_nodash: PredictedPlayer<Vec2, MoveInput, Field>,
  pub ab_dash_monitor: CorrectionMonitor,
  pub ab_nodash_monitor: CorrectionMonitor,
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
      predict_dash: true,
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
      // A floor of a few pixels, so ordinary sub-pixel disagreement never reads
      // as an anomaly however quiet a stretch of play gets.
      monitor: CorrectionMonitor::new().with_floor(8.0),
      // The A/B shadows read `logical`, never `render`, so they need no smoothing.
      ab_dash: PredictedPlayer::new(
        Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
        PlayerConfig { input_buffer: 256, smoothing_secs: 0.0, easing: plaza_client_utils::linear },
        apply_move,
        lerp_pos,
      ),
      ab_nodash: PredictedPlayer::new(
        Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
        PlayerConfig { input_buffer: 256, smoothing_secs: 0.0, easing: plaza_client_utils::linear },
        apply_move,
        lerp_pos,
      ),
      ab_dash_monitor: CorrectionMonitor::new().with_floor(8.0),
      ab_nodash_monitor: CorrectionMonitor::new().with_floor(8.0),
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
    // Grant the local dash mirror *before* predicting this step, so the step
    // itself can move at dash speed. It uses the server's own cooldown, so it
    // rarely disagrees; when it does, the cost is one stray correction, no worse
    // than a single unpredicted dash.
    if dash && self.now_ms >= self.local_dash_ready_ms {
      self.local_dash_until_ms = self.now_ms + DASH_DURATION_MS;
      self.local_dash_ready_ms = self.now_ms + DASH_COOLDOWN_MS;
    }
    // Predict the dash *movement* too, when the switch is on: the hole dashes in
    // whatever direction it is already steering, so a boosted base speed while
    // the mirror says you are dashing is the whole prediction. Off, the dash is
    // left to arrive as a correction, the older and simpler behaviour, so the
    // two can be compared live.
    let dash_now = self.now_ms < self.local_dash_until_ms;
    let dashing = self.predict_dash && dash_now;
    // Predicted locally at once, and the sequence it returns is what the server
    // is asked to acknowledge. The buffered copy is replayed over whatever comes
    // back.
    // A frozen hole (dead, awaiting respawn) still numbers its inputs so the
    // sequence stays in step with the server, but predicts nothing: see
    // `PredictedPlayer::set_active`, driven from the packet handler.
    let seq = self.local.input(MoveInput { dir, dt, dash: dashing });
    // Feed the two shadows the identical step, differing only in the dash flag, so
    // their corrections are a controlled comparison of predicting the dash or not.
    self.ab_dash.input(MoveInput { dir, dt, dash: dash_now });
    self.ab_nodash.input(MoveInput { dir, dt, dash: false });
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
    // Mirror the dash-prediction switch so `send_input`, which is not handed the
    // controls, can honour it.
    self.predict_dash = controls.predict_dash;
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
          // Before anything else: say which wire format this build speaks, so a
          // stale page is told to reload rather than half-working.
          let _ = self.socket.send_json(&Op::Hello { protocol: PROTOCOL });
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
          // The field pulling on your hole: every other live hole as a point
          // source, your own excluded (its pull on itself is zero, but computed
          // from a stale packet position it would read as a spurious self-tug).
          // Refreshed before reconciling, so a replay runs against the newest
          // world rather than the one from the previous packet.
          let field: Field = packet
            .holes
            .iter()
            .filter(|(id, h)| Some(*id) != self.me && h.alive)
            .map(|(_, h)| h.as_attractor())
            .collect();
          self.local.set_context(field.clone());
          self.ab_dash.set_context(field.clone());
          self.ab_nodash.set_context(field);

          if let Some(me) = self.me
            && let Some(hole) = packet.holes.iter().find(|(id, _)| *id == me)
          {
            // Freeze the prediction while eliminated: the server holds a dead hole
            // in place through the respawn delay, so integrating input into it
            // manufactures a correction every packet out of nothing at all.
            let respawned = hole.1.alive && !self.local.is_active();
            self.local.set_active(hole.1.alive);
            self.ab_dash.set_active(hole.1.alive);
            self.ab_nodash.set_active(hole.1.alive);
            if respawned {
              // Not a correction: the hole did not travel from where it died to
              // where it respawned, so easing across that would draw it smoothly
              // over the whole arena.
              self.local.teleport(hole.1.pos);
              self.ab_dash.teleport(hole.1.pos);
              self.ab_nodash.teleport(hole.1.pos);
            }

            // Reconcile: the authoritative position is the basis, and everything
            // sent since is replayed over it under the shared rule.
            let correction = self.local.reconcile(hole.1.pos, self.acked_seq);
            let jump = correction.seen.dist(correction.settled);
            if self.monitor.record(jump) {
              // Distance to the nearest other hole: a small value means a close
              // grapple, where the inverse-square pull and the unpredicted contact
              // separation are what the prediction cannot track, whatever the dash
              // state happens to be. `dashing` is only the server's dash flag,
              // which reads true through a grapple because a dash is how a grapple
              // is fought, so it is a coincidence and not the cause.
              let nearest = packet
                .holes
                .iter()
                .filter(|(id, h)| *id != me && h.alive)
                .map(|(_, h)| h.pos.dist(hole.1.pos))
                .fold(f32::INFINITY, f32::min);
              eprintln!(
                "hole correction t={}ms jump={:.1}px (norm {:.1}px, band {:.1}px) nearest={:.0}px dashing={} authoritative=({:.0},{:.0})",
                packet.server_time_ms,
                jump,
                self.monitor.norm(),
                self.monitor.band(),
                nearest,
                packet.dashing.iter().any(|d| Some(*d) == self.me),
                hole.1.pos.x,
                hole.1.pos.y,
              );
            }

            // The same authoritative state through both shadows. `ab_dash`
            // predicts the dash and `ab_nodash` does not, and they are otherwise
            // identical, so the gap between their norms is the dash prediction's
            // whole worth, measured on the gameplay actually being played.
            let on = self.ab_dash.reconcile(hole.1.pos, self.acked_seq);
            self.ab_dash_monitor.record(on.seen.dist(on.settled));
            let off = self.ab_nodash.reconcile(hole.1.pos, self.acked_seq);
            self.ab_nodash_monitor.record(off.seen.dist(off.settled));
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
        Op::Outdated { server, client } => {
          self.status = Status::Gone(format!(
            "this page was built for wire format {client} and the server speaks {server}: reload to get the current client"
          ));
        }
        Op::Input { .. } | Op::Ping { .. } | Op::Hello { .. } => {}
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

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::native::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::miniquad::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}
