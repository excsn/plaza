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

use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::RttEstimator;
use plaza_wire::frame;
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::{CloseReason, Event, Socket};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, PROTOCOL};
use crate::sim::types::{Controls, Input, PlayerId, Track};

const WIRE: MsgPackCodec = MsgPackCodec;
const PING_INTERVAL_MS: u64 = 1000;
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
  socket: Box<dyn Socket>,
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,

  rtt: RttEstimator,
  clock: ClockSyncEstimator,
  events: Vec<Event>,
  last_ping_ms: u64,
  now_ms: u64,
  /// Wall time carried between frames, so the simulation runs in whole ticks
  /// however long a frame took.
  spare_ms: u64,
  pub resume_drops: u64,
  pub bytes_sent: u64,
  pub bytes_received: u64,
}

fn send_framed(socket: &dyn Socket, op: &Op) -> usize {
  let mut buf = Vec::with_capacity(256);
  frame::begin(frame::Kind::Ops, &mut buf);
  if WIRE.encode_into(&std::slice::from_ref(op), &mut buf).is_err() {
    debug_assert!(false, "an op failed to serialise");
    return 0;
  }
  let n = buf.len();
  let _ = socket.send(&buf);
  n
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let socket = open(url)?;
    Ok(Self::from_socket(socket))
  }

  pub fn from_socket(socket: Box<dyn Socket>) -> Self {
    Self {
      socket,
      sim: SimClient::new(0, Track::circuit(), PROTOCOL),
      status: Status::Connecting,
      me: None,
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      events: Vec::new(),
      last_ping_ms: 0,
      now_ms: 0,
      spare_ms: 0,
      resume_drops: 0,
      bytes_sent: 0,
      bytes_received: 0,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt_ms()
  }

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    (
      self.clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      self.clock.sample_count(),
    )
  }

  pub fn restart(&mut self) {
    self.spare_ms = 0;
    self.sim.restart();
  }

  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      self.bytes_sent += send_framed(self.socket.as_ref(), &Op::Ping { origin_ms: now_ms }) as u64;
    }

    self.socket.poll(&mut self.events);
    let mut events = std::mem::take(&mut self.events);
    if self.me.is_some() && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
    }

    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          self.bytes_sent += send_framed(self.socket.as_ref(), &Op::Hello { protocol: PROTOCOL }) as u64;
        }
        Event::Text(text) => self.on_message(text.as_bytes()),
        Event::Message(bytes) => self.on_message(&bytes),
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
      self.bytes_sent += send_framed(
        self.socket.as_ref(),
        &Op::Submit {
          log: Box::new(log),
          claimed_ms,
        },
      ) as u64;
    }
  }

  fn on_message(&mut self, bytes: &[u8]) {
    self.bytes_received += bytes.len() as u64;
    let Some((tag, body)) = frame::split(bytes) else {
      return;
    };
    if frame::Kind::from_byte(tag) != Some(frame::Kind::Ops) {
      return;
    }
    let Ok(ops) = WIRE.decode::<Vec<Op>>(body) else {
      return;
    };
    for op in ops {
      match op {
        Op::Welcome {
          player,
          protocol,
          track,
          ghosts,
          server_time_ms: _,
        } => {
          self.me = Some(player);
          self.sim = SimClient::new(player, *track, protocol);
          self.sim.on_ghosts(ghosts);
          self.status = Status::Playing;
          self.sim.restart();
        }
        Op::Accepted { ghost, place } => self.sim.on_accepted(*ghost, place),
        Op::Refused { why } => self.sim.on_refused(why),
        Op::NoSeat { seats } => self.status = Status::NoSeat { seats },
        Op::Pong { origin_ms, server_ms } => {
          self.rtt.observe_pong(origin_ms, self.now_ms);
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (server_ms as f64 + one_way) - self.now_ms as f64;
          self.clock.observe(self.now_ms as f64, offset);
        }
        Op::Outdated { server, client } => {
          self.status = Status::Gone(format!(
            "this page was built for wire format {client} and the server speaks {server}: reload to get the current client"
          ));
        }
        Op::Hello { .. } | Op::Ping { .. } | Op::Submit { .. } => {}
      }
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

#[cfg(not(any(all(feature = "native", not(target_arch = "wasm32")), all(feature = "web", target_arch = "wasm32"))))]
fn open(_url: &str) -> Result<Box<dyn Socket>, String> {
  Err("this build has no socket backend compiled in".to_owned())
}

#[cfg(test)]
mod tests {
  use std::collections::VecDeque;
  use std::sync::Arc;

  use parking_lot::Mutex;
  use plaza_ws::State;

  use super::*;
  use crate::sim::server::Server;
  use crate::sim::types::SIM_STEP_MS;
  use crate::sim::world::autopilot;

  #[derive(Clone)]
  struct ScriptedSocket {
    inbox: Arc<Mutex<VecDeque<Event>>>,
    sent: Arc<Mutex<Vec<Op>>>,
  }

  impl ScriptedSocket {
    fn new() -> Self {
      Self {
        inbox: Arc::new(Mutex::new(VecDeque::new())),
        sent: Arc::new(Mutex::new(Vec::new())),
      }
    }

    fn feed(&self, ops: Vec<Op>) {
      let mut buf = Vec::new();
      frame::begin(frame::Kind::Ops, &mut buf);
      WIRE.encode_into(&ops, &mut buf).expect("encode");
      self.inbox.lock().push_back(Event::Message(buf));
    }
  }

  impl Socket for ScriptedSocket {
    fn send(&self, bytes: &[u8]) -> Result<(), plaza_ws::WsError> {
      if let Some((tag, body)) = frame::split(bytes)
        && frame::Kind::from_byte(tag) == Some(frame::Kind::Ops)
        && let Ok(ops) = WIRE.decode::<Vec<Op>>(body)
      {
        self.sent.lock().extend(ops);
      }
      Ok(())
    }
    fn send_text(&self, _text: &str) -> Result<(), plaza_ws::WsError> {
      Ok(())
    }
    fn poll(&mut self, out: &mut Vec<Event>) {
      out.extend(self.inbox.lock().drain(..));
    }
    fn state(&self) -> State {
      State::Open
    }
    fn close(&mut self) {}
  }

  fn controls() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      ..Controls::default()
    }
  }

  fn welcomed(feed: &ScriptedSocket) -> NetClient {
    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    let mut server = Server::new(1);
    server.take_seat(0);
    feed.feed(vec![server.welcome(0)]);
    client.poll(0, &controls());
    client
  }

  #[test]
  fn a_welcome_hands_over_the_track_and_starts_a_run() {
    let feed = ScriptedSocket::new();
    let client = welcomed(&feed);
    assert_eq!(client.status, Status::Playing);
    assert!(client.sim.running, "the trial is under way");
    assert_eq!(client.sim.track, Track::circuit());
  }

  #[test]
  fn the_simulation_runs_in_whole_ticks_however_long_a_frame_took() {
    // The property that makes a lap time comparable across machines. Fed the
    // same total time in different sized pieces, the tick count must match.
    let c = controls();
    let feed = ScriptedSocket::new();
    let mut smooth = welcomed(&feed);
    let feed2 = ScriptedSocket::new();
    let mut lumpy = welcomed(&feed2);

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
    assert_eq!(smooth.sim.racer, lumpy.sim.racer);
  }

  #[test]
  fn a_finished_run_puts_its_log_on_the_wire_once() {
    let c = controls();
    let feed = ScriptedSocket::new();
    let mut client = welcomed(&feed);
    for _ in 0..crate::sim::log::MAX_TICKS {
      let input = autopilot(&client.sim.racer, &client.sim.track, client.sim.tick);
      client.tick(SIM_STEP_MS, input, &c);
      if !client.sim.running {
        break;
      }
    }
    assert!(client.sim.finished_ms.is_some(), "it finished");

    let sent = feed.sent.lock().clone();
    let count = sent.iter().filter(|op| matches!(op, Op::Submit { .. })).count();
    assert_eq!(count, 1, "one submission, carrying the inputs");
  }
}
