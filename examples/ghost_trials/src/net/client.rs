//! A client on a real wire.
//!
//! Thin, because the wire is not in the driving loop. It wraps the same
//! [`sim::Client`] the harness runs, adds a socket and a clock estimate, and
//! sends one message per finished run.
//!
//! The clock is here for the readouts rather than for the simulation, which is
//! a real difference from every other playground. A run's tick count comes from
//! the ticks it took, not from any wall clock, so a client with a badly fitted
//! clock still records the same lap time. That is not slackness: it is what
//! makes a recorded run comparable with one driven on another machine a week
//! later.
//!
//! [`sim::Client`]: crate::sim::client::Client

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::Event;

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, PROTOCOL};
use crate::sim::types::{Controls, Input, PlayerId, Track};

const WIRE: MsgPackCodec = MsgPackCodec;
const BACKLOG_TRIGGER: usize = 128;
const BACKLOG_KEEP: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Waiting,
  Playing,
  NoSeat { seats: usize },
  Gone(String),
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
  /// Wall time carried between frames, so the simulation runs in whole ticks
  /// however long a frame took.
  spare_ms: u64,
  pub resume_drops: u64,
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
      sim: SimClient::new(0, Track::circuit(), PROTOCOL),
      status: Status::Connecting,
      me: None,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
      spare_ms: 0,
      resume_drops: 0,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// Everything sent, in bytes, probes and answers included.
  pub fn bytes_sent(&self) -> u64 {
    self.pump.bytes_sent()
  }

  /// Everything received, in bytes, before any of it is decoded.
  pub fn bytes_received(&self) -> u64 {
    self.pump.bytes_received()
  }

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    let clock = &self.pump.timeline().clock;
    (
      clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      clock.sample_count(),
    )
  }

  pub fn restart(&mut self) {
    self.spare_ms = 0;
    self.sim.restart();
  }

  pub fn restart_as(&mut self, mode: crate::sim::types::Mode, size: crate::sim::types::TrackSize, field: usize) {
    self.spare_ms = 0;
    self.sim.restart_as(mode, size, field);
  }

  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    if self.me.is_some() && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
      // A probe sent before the freeze and answered after it measures the
      // freeze, not the network, and its origin still matches so the echo
      // check waves it through. `on_resume` is what discards it, along with
      // everything the estimators learned across a gap of unknown length.
      self.pump.on_resume();
    }
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
        }
        Arrival::Ops(frame) => self.on_ops(frame.body()),
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;
    let _ = controls;
  }

  /// Advances the trial by whole ticks, under the input held this frame, and
  /// sends the run if it just ended.
  ///
  /// **Whole ticks, from carried wall time.** A racing example is the easiest
  /// place in the world to advance by "however long the last frame took", and
  /// it would make every recorded lap a function of the frame rate that
  /// recorded it: a ghost from a 144 Hz machine would drive differently on a
  /// 60 Hz one.
  pub fn tick(&mut self, dt_ms: u64, input: Input, controls: &Controls) {
    if !self.is_playing() {
      return;
    }
    self.spare_ms += dt_ms.min(250);
    let step = crate::sim::types::SIM_STEP_MS;
    let mut budget = 16;
    while self.spare_ms >= step && budget > 0 {
      self.spare_ms -= step;
      budget -= 1;
      self.sim.step(input, controls);
    }
    if let Some((log, claimed_ms)) = self.sim.take_submission() {
      self.pump.send_op(&Op::Submit {
        log: Box::new(log),
        claimed_ms,
      });
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<Op>>(body) else {
      return;
    };
    for op in ops {
      match op {
        Op::Welcome {
          player,
          protocol,
          ghosts,
          server_time_ms: _,
        } => {
          self.me = Some(player);
          // The track is built here rather than sent: it is a constant both
          // ends already have, and the log names which one by a single byte.
          self.sim = SimClient::new(player, Track::circuit(), protocol);
          self.sim.on_ghosts(ghosts);
          self.status = Status::Playing;
        }
        Op::Accepted { ghost, place } => self.sim.on_accepted(*ghost, place),
        Op::Refused { why } => self.sim.on_refused(why),
        Op::NoSeat { seats } => self.status = Status::NoSeat { seats },
        Op::Submit { .. } => {}
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame;
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::sim::server::Server;
  use crate::sim::types::SIM_STEP_MS;
  use crate::sim::world::autopilot;

  fn feed(socket: &ScriptedSocket, ops: Vec<Op>) {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ops, &mut buf);
    WIRE.encode_into(&ops, &mut buf).expect("encode");
    socket.feed_message(buf);
  }

  fn sent_ops(socket: &ScriptedSocket) -> Vec<Op> {
    socket
      .sent()
      .iter()
      .filter_map(|bytes| {
        let (tag, body) = frame::split(bytes)?;
        (frame::Kind::from_byte(tag) == Some(frame::Kind::Ops)).then(|| WIRE.decode::<Vec<Op>>(body).ok())?
      })
      .flatten()
      .collect()
  }

  fn controls() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      ..Controls::default()
    }
  }

  fn welcomed(socket: &ScriptedSocket) -> NetClient {
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    let mut server = Server::new(1);
    server.take_seat(0);
    feed(socket, vec![server.welcome(0)]);
    client.poll(0, &controls());
    client
  }

  #[test]
  fn a_welcome_hands_over_the_track_and_starts_a_run() {
    let socket = ScriptedSocket::new();
    let client = welcomed(&socket);
    assert_eq!(client.status, Status::Playing);
    assert_eq!(client.sim.track, Track::circuit());
  }

  #[test]
  fn the_simulation_runs_in_whole_ticks_however_long_a_frame_took() {
    let c = controls();
    let socket = ScriptedSocket::new();
    let mut smooth = welcomed(&socket);
    smooth.restart_as(crate::sim::types::Mode::Trial, crate::sim::types::TrackSize::Medium, 1);
    let socket2 = ScriptedSocket::new();
    let mut lumpy = welcomed(&socket2);
    lumpy.restart_as(crate::sim::types::Mode::Trial, crate::sim::types::TrackSize::Medium, 1);

    for _ in 0..300 {
      smooth.tick(SIM_STEP_MS, Input::new(1, false), &c);
    }
    // The same 6000 ms, in awkward pieces.
    for _ in 0..100 {
      lumpy.tick(17, Input::new(1, false), &c);
      lumpy.tick(23, Input::new(1, false), &c);
      lumpy.tick(20, Input::new(1, false), &c);
    }
    assert_eq!(smooth.sim.tick, lumpy.sim.tick);
    assert_eq!(smooth.sim.racer(), lumpy.sim.racer());
  }

  #[test]
  fn a_finished_run_puts_its_log_on_the_wire_once() {
    let c = controls();
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket);
    client.restart_as(crate::sim::types::Mode::Trial, crate::sim::types::TrackSize::Medium, 1);
    for _ in 0..crate::sim::log::MAX_TICKS {
      let input = autopilot(client.sim.racer(), &client.sim.track, client.sim.tick, 0);
      client.tick(SIM_STEP_MS, input, &c);
      if !client.sim.running {
        break;
      }
    }
    assert!(client.sim.finished_ms.is_some(), "it finished");

    let count = sent_ops(&socket).iter().filter(|op| matches!(op, Op::Submit { .. })).count();
    assert_eq!(count, 1, "one submission, carrying the inputs");
  }
}
