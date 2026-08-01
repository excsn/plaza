//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline harness runs, and adds the two
//! things a shared clock was standing in for.
//!
//! **The clock is estimated, not shared**, and here it decides something
//! different from the other examples. Elsewhere the clock names the tick an
//! input is *for*. Here it names the tick this client's simulation should be
//! *at*, which is a stronger requirement: a client whose clock drifts does not
//! aim badly, it runs the world at the wrong speed, and its digest disagrees
//! with the server's for a reason that has nothing to do with arithmetic.
//!
//! **The newest server timestamp is a floor, carried forward at wall rate.**
//! Same mechanism as `pellet_maze` and `bomb_grid`, same reason: a stamp the
//! server wrote is a lower bound that needs no synchronisation to trust, and it
//! has to keep advancing between messages or the simulation stalls between
//! digests and then catches up in a burst.
//!
//! What this client does **not** do is predict its own build. It asks, and the
//! tower appears when the server's op arrives naming a tick everyone applies it
//! on. Predicting it locally would mean simulating a cause the server might
//! refuse, and there is no correction here to undo that: it would be a
//! divergence the digest catches half a second later.
//!
//! [`sim::Client`]: crate::sim::Client

use plaza_client_utils::{Probe, Timeline};
use plaza_wire::frame::{self, ProtocolVersion};
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::{CloseReason, Event, Socket};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Cell, Controls, PlayerId, TowerKind, SIM_STEP_MS};

/// One codec for the whole client, matching the one the host is built with.
const WIRE: MsgPackCodec = MsgPackCodec;

const PING_INTERVAL_MS: u64 = 1000;

/// More payload messages than this in one poll means the frame loop was stopped
/// while the socket kept receiving: a hidden browser tab, a machine that slept.
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
  /// The same client the offline harness runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  timeline: Timeline,
  probe: Option<Probe>,
  newest_stamp_ms: u64,
  stamp_at_local_ms: u64,

  seq: u64,
  last_ack: u64,
  pub refusals: u64,
  /// Digests received, so the panel can say whether checking is happening at
  /// all rather than only whether it has failed.
  pub digests_seen: u64,
  pub snapshots_received: u64,

  events: Vec<Event>,
  last_ping_ms: u64,
  now_ms: u64,
  pub resume_drops: u64,
}

fn send_hello(socket: &dyn Socket) {
  let mut buf = Vec::with_capacity(16);
  frame::begin(frame::Kind::Hello, &mut buf);
  if WIRE.encode_into(&ProtocolVersion(PROTOCOL), &mut buf).is_err() {
    debug_assert!(false, "a protocol version failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

/// Starts a latency probe. The stamp is this client's own clock in
/// milliseconds; the server echoes it back without reading it.
fn send_ping(socket: &dyn Socket, origin: u64) {
  let mut buf = Vec::with_capacity(16);
  frame::begin(frame::Kind::Ping, &mut buf);
  if WIRE.encode_into(&frame::Ping { origin }, &mut buf).is_err() {
    debug_assert!(false, "a ping failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

fn send_framed(socket: &dyn Socket, op: &Op) {
  let mut buf = Vec::with_capacity(64);
  frame::begin(frame::Kind::Ops, &mut buf);
  if WIRE.encode_into(&std::slice::from_ref(op), &mut buf).is_err() {
    debug_assert!(false, "an op failed to serialise");
    return;
  }
  let _ = socket.send(&buf);
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let socket = open(url)?;
    Ok(Self::from_socket(socket))
  }

  pub fn from_socket(socket: Box<dyn Socket>) -> Self {
    Self {
      socket,
      sim: SimClient::new(0),
      status: Status::Connecting,
      me: None,
      policy: None,
      timeline: Timeline::new(),
      probe: None,
      newest_stamp_ms: 0,
      stamp_at_local_ms: 0,
      seq: 0,
      last_ack: 0,
      refusals: 0,
      digests_seen: 0,
      snapshots_received: 0,
      events: Vec::new(),
      last_ping_ms: 0,
      now_ms: 0,
      resume_drops: 0,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.timeline.rtt.rtt()
  }

  /// This client's best estimate of server time now: the fitted clock, floored
  /// by the newest stamp carried forward at wall rate.
  pub fn server_time_ms(&self) -> u64 {
    let fitted = self.timeline.clock.server_time_at(self.now_ms as f64).unwrap_or(self.now_ms as f64).max(0.0) as u64;
    let carried = self.newest_stamp_ms + self.now_ms.saturating_sub(self.stamp_at_local_ms);
    fitted.max(carried)
  }

  /// The tick this client's simulation should have reached.
  pub fn target_tick(&self) -> u64 {
    self.server_time_ms() / SIM_STEP_MS
  }

  /// How far this client's simulation is from where its clock says it should
  /// be. Nonzero for a moment after a snapshot, and otherwise a sign that the
  /// catch-up budget is being hit.
  pub fn tick_lag(&self) -> i64 {
    self.target_tick() as i64 - self.sim.tick() as i64
  }

  fn note_stamp(&mut self, stamp_ms: u64) {
    if stamp_ms >= self.newest_stamp_ms {
      self.newest_stamp_ms = stamp_ms;
      self.stamp_at_local_ms = self.now_ms;
    }
  }

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    (
      self.timeline.clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      self.timeline.clock.sample_count(),
    )
  }

  pub fn ack_lag(&self) -> (u64, u64) {
    (self.seq, self.last_ack)
  }

  /// Asks for a tower. Nothing happens locally: see the module note.
  pub fn want_build(&mut self, cell: Cell, kind: TowerKind, upgrade: bool) {
    if !self.is_playing() {
      return;
    }
    self.seq += 1;
    send_framed(
      self.socket.as_ref(),
      &Op::Want {
        seq: self.seq,
        cell,
        kind,
        upgrade,
      },
    );
  }

  /// Drains the socket, folds in what arrived, and advances the simulation.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      let probe = self.timeline.begin(now_ms);
      self.probe = Some(probe);
      send_ping(self.socket.as_ref(), probe.sent_at);
    }

    self.socket.poll(&mut self.events);
    let mut events = std::mem::take(&mut self.events);
    if self.digests_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
      self.resume_drops += 1;
      // A probe sent before the freeze and answered after it measures the
      // freeze, not the network, and its origin still matches so the echo
      // check waves it through. The epoch is what discards it, along with
      // everything the estimators learned across a gap of unknown length.
      self.timeline.on_resume();
      self.probe = None;
    }

    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          send_hello(self.socket.as_ref());
        }
        Event::Text(text) => self.on_message(text.as_bytes(), controls),
        Event::Message(bytes) => self.on_message(&bytes, controls),
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

  /// Runs the local simulation up to the clock, then sends any request the
  /// simulation decided it needs. Call once per frame, after `poll`.
  pub fn tick(&mut self, controls: &Controls) {
    if !self.is_playing() {
      return;
    }
    self.sim.run_to(self.target_tick(), controls);
    if let Some(op) = self.sim.take_request() {
      send_framed(self.socket.as_ref(), &op);
    }
  }

  fn on_server_protocol(&mut self, body: &[u8]) {
    let Ok(theirs) = WIRE.decode::<ProtocolVersion>(body) else {
      return;
    };
    if ProtocolVersion(PROTOCOL).agrees_with(theirs) {
      return;
    }
    self.status = Status::Gone(format!(
      "this page was built for wire format {PROTOCOL} and the server speaks {}: reload to get the current client",
      theirs.0
    ));
  }

  fn on_message(&mut self, bytes: &[u8], controls: &Controls) {
    let Some((tag, body)) = frame::split(bytes) else {
      return;
    };
    match frame::Kind::from_byte(tag) {
      Some(frame::Kind::Ops) => {}
      Some(frame::Kind::Hello) => return self.on_server_protocol(body),
      // The server's session answers this one; this end only echoes the stamp
      // back, because the clock being measured is the server's.
      Some(frame::Kind::Ping) => {
        if let Some(reply) = frame::answer_ping(&WIRE, body, None) {
          let _ = self.socket.send(&reply);
        }
        return;
      }
      Some(frame::Kind::Pong) => {
        if let (Ok(pong), Some(probe)) = (WIRE.decode::<frame::Pong>(body), self.probe.take())
          && pong.origin == probe.sent_at
        {
          self.timeline.complete(probe, self.now_ms, pong.responder);
        }
        return;
      }
      None => return,
    }
    let Ok(ops) = WIRE.decode::<Vec<Op>>(body) else {
      return;
    };
    for op in ops {
      match op {
        Op::Welcome {
          player,
          seed,
          policy,
          field,
          server_time_ms,
        } => {
          self.me = Some(player);
          self.policy = Some(policy);
          self.note_stamp(server_time_ms);
          self.sim = SimClient::new(player);
          self.sim.on_welcome(seed, policy, &field);
          self.status = Status::Playing;
        }
        Op::Wave { wave, start_tick } => self.sim.on_wave(wave, start_tick),
        Op::Built { tick, build } => self.sim.on_built(tick, build),
        Op::Digest { tick, digest, enemies } => {
          self.digests_seen += 1;
          self.sim.on_digest(tick, digest, enemies, controls);
        }
        Op::Snapshot { field, server_time_ms } => {
          self.snapshots_received += 1;
          self.note_stamp(server_time_ms);
          self.sim.adopt(&field);
        }
        Op::Over { wave } => self.sim.over = Some(wave),
        Op::Ack { seq } => self.last_ack = self.last_ack.max(seq),
        Op::Refused { .. } => self.refusals += 1,
        Op::NoSeat { seats } => self.status = Status::NoSeat { seats },
        Op::Want { .. } | Op::WantSnapshot { .. } => {}
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

  use super::*;
  use crate::sim::rules::Field;
  use plaza_ws::State;
  use crate::sim::server::{Server, WORLD_SEED};

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
    let mut server = Server::new(1, WORLD_SEED);
    server.take_seat(0);
    feed.feed(vec![server.welcome(0, &controls())]);
    client.poll(0, &controls());
    client
  }

  #[test]
  fn a_welcome_hands_over_the_seed_and_the_client_starts_playing() {
    let feed = ScriptedSocket::new();
    let client = welcomed(&feed);
    assert_eq!(client.status, Status::Playing);
    assert_eq!(client.me, Some(0));
    assert_eq!(client.sim.seed, WORLD_SEED, "the one number the whole world comes from");
  }

  #[test]
  fn a_disagreeing_digest_puts_a_request_on_the_wire() {
    let feed = ScriptedSocket::new();
    let mut client = welcomed(&feed);
    let c = controls();

    // Reach a tick, then hand it a digest for that tick that cannot match.
    client.poll(2_000, &c);
    client.tick(&c);
    let at = client.sim.tick();
    assert!(at > 0, "the simulation ran");

    feed.feed(vec![Op::Digest {
      tick: at,
      digest: 0xDEAD_BEEF,
      enemies: 99,
    }]);
    client.poll(2_050, &c);
    client.tick(&c);

    assert_eq!(client.sim.mismatches, 1);
    assert!(
      feed.sent.lock().iter().any(|op| matches!(op, Op::WantSnapshot { .. })),
      "and it asked for the state"
    );
  }

  #[test]
  fn a_snapshot_replaces_the_field_and_stops_the_asking() {
    let feed = ScriptedSocket::new();
    let mut client = welcomed(&feed);
    let c = controls();
    client.poll(2_000, &c);
    client.tick(&c);

    let mut field = Field::default();
    field.tick = 500;
    field.gold = 12_345;
    feed.feed(vec![Op::Snapshot {
      field: Box::new(field),
      server_time_ms: 500 * SIM_STEP_MS,
    }]);
    client.poll(2_100, &c);

    assert_eq!(client.snapshots_received, 1);
    assert_eq!(client.sim.field.gold, 12_345, "the field was adopted whole");
    assert_eq!(client.sim.field.tick, 500);
  }

  #[test]
  fn a_build_request_carries_no_tower() {
    let feed = ScriptedSocket::new();
    let mut client = welcomed(&feed);
    let towers = client.sim.field.towers.len();
    client.want_build(Cell::new(4, 5), TowerKind::Arrow, false);

    let sent = feed.sent.lock().clone();
    assert!(sent.iter().any(|op| matches!(op, Op::Want { .. })));
    assert_eq!(client.sim.field.towers.len(), towers, "and nothing was built locally");
  }
}
