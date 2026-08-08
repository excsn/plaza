//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! The heart is a [`RollbackSession`] running the same fixed-point `sim::step`
//! the server runs. Every server frame echoes the inputs it applied, so the
//! session confirms remote inputs against a single ordered truth, rolls back
//! when a guess is disproved, and re-simulates to the present. The digest on
//! every frame is checked against this client's own re-simulation of the same
//! frame: the machines proving, continuously, that they still agree.
//!
//! The panel's comparison lives here too: **Interpolate** renders the puck
//! from delayed server frames (the standard remote-entity treatment), and
//! **Rollback** renders the session's present. Both record what they showed
//! per frame, and when the authoritative world for that frame arrives, the
//! gap between shown and true is the number the example exists to produce.

use std::collections::VecDeque;

use plaza_client_utils::rollback::{RollbackConfig, RollbackSession};
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{ms_to_frame, FrameUpdate, RinkOp, PROTOCOL};
use crate::sim::{self, PaddleInput, World, SEATS};

const WIRE: MsgPackCodec = MsgPackCodec;

/// How far ahead of the estimated server present this client runs, so an
/// input's frame is still open when it lands.
const AHEAD_MS: u64 = 50;
/// The standard remote-entity render delay the Interpolate mode pays.
const RENDER_DELAY_MS: u64 = 100;
const KEEP_FRAMES: usize = 64;
const KEEP_SHOWN: usize = 360;
/// Frames the session can roll back across; must exceed the worst one-way in
/// frames, or a correction finds nothing to restore.
const ROLLBACK_WINDOW: usize = 240;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// How the puck reaches the screen. Everything else is always the predicted
/// present with corrections eased, so the screen holds one timeline and a
/// bounce lands where the paddles are drawn; splitting the paddles onto
/// delayed frames made the puck carom off empty ice their drawn past had not
/// reached yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
  /// The session's present: predicted inputs, rolled back on every disproof.
  Rollback,
  /// Delayed server frames, blended: the treatment every *owned-by-someone-
  /// else* entity gets, worn by a body nobody owns.
  Interpolate,
}

/// What a rollback rewrote, kept on screen and bled off over ~100ms so a
/// correction reads as a nudge rather than a teleport. Presentation only:
/// nothing here feeds back into the session or an input.
#[derive(Default)]
pub struct VisualEase {
  pub paddles: [(f32, f32); SEATS],
  pub puck: (f32, f32),
}

impl VisualEase {
  fn absorb(&mut self, before: &World, after: &World) {
    for seat in 0..SEATS {
      self.paddles[seat].0 += before.paddles[seat].x.to_f32() - after.paddles[seat].x.to_f32();
      self.paddles[seat].1 += before.paddles[seat].y.to_f32() - after.paddles[seat].y.to_f32();
    }
    self.puck.0 += before.puck.x.to_f32() - after.puck.x.to_f32();
    self.puck.1 += before.puck.y.to_f32() - after.puck.y.to_f32();
  }

  /// Call once per render frame.
  pub fn decay(&mut self, dt: f32) {
    let keep = (-dt * 10.0).exp();
    for p in &mut self.paddles {
      p.0 *= keep;
      p.1 *= keep;
    }
    self.puck.0 *= keep;
    self.puck.1 *= keep;
  }
}

/// A running mean, hand-rolled: two words and no divide-by-zero to forget.
#[derive(Default)]
pub struct Meter {
  sum: f64,
  n: u64,
}

impl Meter {
  fn add(&mut self, v: f32) {
    self.sum += v as f64;
    self.n += 1;
  }

  pub fn mean(&self) -> f32 {
    if self.n == 0 { 0.0 } else { (self.sum / self.n as f64) as f32 }
  }

  pub fn samples(&self) -> u64 {
    self.n
  }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Moment {
  Seated(usize),
  Goal { scores: [u16; 2] },
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub seat: Option<usize>,
  pub mode: Mode,

  session: Option<RollbackSession<World, PaddleInput>>,
  base: u64,
  pub latest: Option<FrameUpdate>,
  frames: VecDeque<FrameUpdate>,
  pending_checks: VecDeque<(u64, u64)>,
  shown: VecDeque<(u64, (f32, f32), Mode)>,
  prev_scores: [u16; 2],

  pub digest_ok: u64,
  pub digest_bad: u64,
  /// Shown-versus-truth puck error, by the mode that showed it.
  pub err_rollback: Meter,
  pub err_interp: Meter,
  /// Correction size when a rollback rewrote the present.
  pub snap_px: Meter,
  pub corrections: u64,
  pub resim_frames: u64,
  pub ease: VisualEase,
  pub moments: Vec<Moment>,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?))
  }

  pub fn from_socket(socket: Box<dyn plaza_ws::Socket>) -> Self {
    Self::from_pump(FramePump::new(socket, WIRE, PROTOCOL))
  }

  fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      status: Status::Connecting,
      seat: None,
      mode: Mode::Rollback,
      session: None,
      base: 0,
      latest: None,
      frames: VecDeque::new(),
      pending_checks: VecDeque::new(),
      shown: VecDeque::new(),
      prev_scores: [0, 0],
      digest_ok: 0,
      digest_bad: 0,
      err_rollback: Meter::default(),
      err_interp: Meter::default(),
      snap_px: Meter::default(),
      corrections: 0,
      resim_frames: 0,
      ease: VisualEase::default(),
      moments: Vec::new(),
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  pub fn server_time_ms(&self) -> u64 {
    self.pump.server_time_ms(self.now_ms)
  }

  pub fn ready(&self) -> bool {
    self.session.is_some()
  }

  pub fn prediction_horizon(&self) -> u64 {
    self.session.as_ref().map(|s| s.prediction_horizon() as u64).unwrap_or(0)
  }

  pub fn poll(&mut self, now_ms: u64) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Joined;
          }
        }
        Arrival::Ops(frame) => self.on_ops(frame.body()),
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;

    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<RinkOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        RinkOp::Seated { seat } => {
          self.seat = Some(seat as usize);
          self.moments.push(Moment::Seated(seat as usize));
        }
        RinkOp::Frame(update) => self.on_frame(*update),
        RinkOp::Input { .. } => {}
      }
    }
  }

  fn on_frame(&mut self, update: FrameUpdate) {
    self.pump.timeline_mut().note_stamp(update.server_time_ms, self.now_ms);

    let Some(session) = self.session.as_mut() else {
      // The first authoritative world is the session's frame zero.
      self.session = Some(RollbackSession::new(
        update.world.clone(),
        vec![PaddleInput::default(); SEATS],
        RollbackConfig {
          max_rollback_frames: ROLLBACK_WINDOW,
        },
        sim::step,
      ));
      self.base = update.frame;
      self.prev_scores = update.world.scores;
      self.frames.push_back(update.clone());
      self.latest = Some(update);
      return;
    };

    if update.frame <= self.base {
      return;
    }
    // The server labels inputs by the frame they produced, the session by the
    // frame they simulate: `world(f) = step(world(f-1), applied(f))`.
    let simulated = update.frame - self.base - 1;
    for (player, input) in update.applied.iter().enumerate() {
      session.confirm_remote_input(player, simulated, *input);
    }
    self.pending_checks.push_back((update.frame - self.base, update.digest));

    if update.world.scores != self.prev_scores {
      self.prev_scores = update.world.scores;
      self.moments.push(Moment::Goal {
        scores: update.world.scores,
      });
    }

    // What did we show for this frame, back when it was the present?
    let sf = update.frame - self.base;
    if let Some((_, shown, mode)) = self.shown.iter().find(|(f, _, _)| *f == sf) {
      let truth = (update.world.puck.x.to_f32(), update.world.puck.y.to_f32());
      let err = ((shown.0 - truth.0).powi(2) + (shown.1 - truth.1).powi(2)).sqrt();
      match mode {
        Mode::Rollback => self.err_rollback.add(err),
        Mode::Interpolate => self.err_interp.add(err),
      }
    }

    self.frames.push_back(update.clone());
    while self.frames.len() > KEEP_FRAMES {
      self.frames.pop_front();
    }
    self.latest = Some(update);
  }

  /// Advances the session to the aimed present, sending this client's held
  /// input addressed to each frame it simulates. Call once per render frame.
  pub fn advance(&mut self, my_input: PaddleInput) {
    let one_way = self.pump.rtt_ms().map(|r| (r / 2.0) as u64).unwrap_or(0);
    let target_ms = self.server_time_ms() + one_way + AHEAD_MS;
    let Some(session) = self.session.as_mut() else {
      return;
    };
    let target = ms_to_frame(target_ms).saturating_sub(self.base);

    let mut advanced = false;
    let mut steps = 0;
    while session.current_frame() < target && steps < 8 {
      steps += 1;
      advanced = true;

      if let Some(seat) = self.seat {
        session.queue_local_input(seat, my_input);
        // Addressed to the frame the session is about to simulate, which is
        // the server frame it will *produce* minus the base mapping above.
        let frame = self.base + session.current_frame() + 1;
        self.pump.send_op(&RinkOp::Input { frame, input: my_input });
      }

      let before_count = session.rollback_count();
      let cf = session.current_frame();
      let before = session.state().clone();
      session.advance_frame();
      let rolled = session.rollback_count() - before_count;
      if rolled > 0 {
        self.corrections += rolled;
        self.resim_frames += session.last_rollback_frames() as u64;
        if let Some(rewritten) = session.state_at(cf) {
          let dx = rewritten.puck.x.to_f32() - before.puck.x.to_f32();
          let dy = rewritten.puck.y.to_f32() - before.puck.y.to_f32();
          self.snap_px.add((dx * dx + dy * dy).sqrt());
          self.ease.absorb(&before, &rewritten);
        }
      }
    }

    // Digest checks only once a rollback has had an advance to apply it, or
    // the compared state would still be the disproved guess.
    if advanced {
      while let Some((sf, digest)) = self.pending_checks.front().copied() {
        if sf >= session.current_frame() {
          break;
        }
        match session.state_at(sf) {
          Some(world) => {
            if sim::digest(&world) == digest {
              self.digest_ok += 1;
            } else {
              self.digest_bad += 1;
            }
          }
          None => {}
        }
        self.pending_checks.pop_front();
      }
    }
  }

  /// Records what the screen showed for the present frame, so the truth can
  /// be compared against it when it arrives.
  pub fn note_shown(&mut self, puck_px: (f32, f32)) {
    let Some(session) = self.session.as_ref() else {
      return;
    };
    let sf = session.current_frame();
    if let Some(last) = self.shown.back_mut()
      && last.0 == sf
    {
      *last = (sf, puck_px, self.mode);
    } else {
      self.shown.push_back((sf, puck_px, self.mode));
    }
    while self.shown.len() > KEEP_SHOWN {
      self.shown.pop_front();
    }
  }

  /// The world to draw: the session's present. The caller adds [`Self::ease`]
  /// on top, and swaps the puck for the delayed blend when the mode says so.
  pub fn present(&self) -> Option<World> {
    Some(self.session.as_ref()?.state().clone())
  }

  /// The two server frames around `now - delay`, and how far between them the
  /// delayed present sits. `None` until a frame old enough exists.
  fn delayed_frames(&self) -> Option<(&FrameUpdate, Option<&FrameUpdate>, f32)> {
    let t_ms = self.server_time_ms().saturating_sub(RENDER_DELAY_MS);
    let tf = ms_to_frame(t_ms);
    let mut older: Option<&FrameUpdate> = None;
    let mut newer: Option<&FrameUpdate> = None;
    for f in &self.frames {
      if f.frame <= tf {
        older = Some(f);
      } else {
        newer = Some(f);
        break;
      }
    }
    let a = older?;
    let t = match newer {
      Some(b) => (tf.saturating_sub(a.frame)) as f32 / (b.frame - a.frame).max(1) as f32,
      None => 0.0,
    };
    Some((a, newer, t))
  }

  /// The puck as the Interpolate mode shows it: two server frames around
  /// `now - delay`, blended. `None` until a frame old enough exists.
  pub fn interpolated_puck(&self) -> Option<(f32, f32)> {
    let (a, b, t) = self.delayed_frames()?;
    let ax = a.world.puck.x.to_f32();
    let ay = a.world.puck.y.to_f32();
    Some(match b {
      Some(b) => {
        let bx = b.world.puck.x.to_f32();
        let by = b.world.puck.y.to_f32();
        (ax + (bx - ax) * t, ay + (by - ay) * t)
      }
      None => (ax, ay),
    })
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, ProtocolVersion};
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::protocol::{frame_to_ms, Occupant};

  fn feed(socket: &ScriptedSocket, ops: Vec<RinkOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn frame_op(frame: u64, world: &World, applied: [PaddleInput; SEATS]) -> RinkOp {
    RinkOp::Frame(Box::new(FrameUpdate {
      frame,
      server_time_ms: frame_to_ms(frame),
      world: world.clone(),
      applied,
      digest: sim::digest(world),
      occupants: [Occupant::Bot; SEATS],
    }))
  }

  #[test]
  fn the_first_frame_is_the_sessions_ground() {
    let socket = ScriptedSocket::new();
    let world = World::new();
    feed(&socket, vec![RinkOp::Seated { seat: 0 }, frame_op(100, &world, [PaddleInput::default(); SEATS])]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(client.ready());
    assert_eq!(client.seat, Some(0));
  }

  #[test]
  fn a_disproved_guess_rolls_back_and_the_digests_still_agree() {
    let socket = ScriptedSocket::new();
    let world0 = World::new();
    feed(&socket, vec![frame_op(100, &world0, [PaddleInput::default(); SEATS])]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    // The estimate now sits at frame 100's stamp; run the present ahead of it
    // on predicted (neutral) inputs.
    client.advance(PaddleInput::default());
    assert!(client.prediction_horizon() > 0, "the present runs ahead of confirmation");

    // The server disagrees: seat 2 was actually holding east the whole time.
    let mut applied = [PaddleInput::default(); SEATS];
    applied[2] = PaddleInput { dx: 1, dy: 0 };
    let world1 = sim::step(&world0, &applied);
    let world2 = sim::step(&world1, &applied);
    feed(&socket, vec![frame_op(101, &world1, applied), frame_op(102, &world2, applied)]);

    client.poll(50);
    client.advance(PaddleInput::default());

    assert!(client.corrections > 0, "the disproof rolled the session back");
    assert!(client.resim_frames > 0, "and re-simulation was paid for");
    assert!(client.digest_ok >= 2, "the re-simulated frames match the server's digests");
    assert_eq!(client.digest_bad, 0, "fixed point holds: no divergence");
  }

  #[test]
  fn a_goal_is_a_moment() {
    let socket = ScriptedSocket::new();
    let world0 = World::new();
    let mut scored = world0.clone();
    scored.scores = [1, 0];
    feed(&socket, vec![
      frame_op(100, &world0, [PaddleInput::default(); SEATS]),
      frame_op(101, &scored, [PaddleInput::default(); SEATS]),
    ]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(client.moments.iter().any(|m| matches!(m, Moment::Goal { scores: [1, 0] })));
  }

  #[test]
  fn a_correction_is_absorbed_into_the_visual_ease_and_bleeds_off() {
    let socket = ScriptedSocket::new();
    let world0 = World::new();
    feed(&socket, vec![frame_op(100, &world0, [PaddleInput::default(); SEATS])]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    client.advance(PaddleInput::default());

    let mut applied = [PaddleInput::default(); SEATS];
    applied[2] = PaddleInput { dx: 1, dy: 0 };
    let world1 = sim::step(&world0, &applied);
    let world2 = sim::step(&world1, &applied);
    feed(&socket, vec![frame_op(101, &world1, applied), frame_op(102, &world2, applied)]);

    client.poll(50);
    client.advance(PaddleInput::default());

    assert!(client.corrections > 0, "the disproof rolled the session back");
    let absorbed = client.ease.paddles[2].0.abs();
    assert!(absorbed > 0.0, "the rewritten paddle left its snap in the ease");

    client.ease.decay(0.1);
    let after = client.ease.paddles[2].0.abs();
    assert!(after < absorbed && after > 0.0, "the ease bleeds off rather than snapping to zero");
  }

  #[test]
  fn a_server_on_another_wire_format_is_reported_rather_than_ignored() {
    let socket = ScriptedSocket::new();
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Hello, &mut bytes);
    WIRE.encode_into(&ProtocolVersion(PROTOCOL.wrapping_add(1)), &mut bytes).unwrap();
    socket.feed_message(bytes);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(matches!(client.status, Status::Gone(_)));
  }
}
