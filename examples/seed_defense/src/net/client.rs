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
//! That mechanism is [`Timeline::note_stamp`] now: a stamp the server wrote is
//! a lower bound that needs no synchronisation to trust, and it has to keep
//! advancing between messages or the simulation stalls between digests and
//! then catches up in a burst.
//!
//! What this client does **not** do is predict its own build. It asks, and the
//! tower appears when the server's op arrives naming a tick everyone applies it
//! on. Predicting it locally would mean simulating a cause the server might
//! refuse, and there is no correction here to undo that: it would be a
//! divergence the digest catches half a second later.
//!
//! [`sim::Client`]: crate::sim::Client
//! [`Timeline::note_stamp`]: plaza_client_utils::Timeline::note_stamp

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::Event;

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Cell, Controls, PlayerId, TowerKind, SIM_STEP_MS};

/// One codec for the whole client, matching the one the host is built with.
const WIRE: MsgPackCodec = MsgPackCodec;

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
  pump: FramePump<MsgPackCodec>,
  /// The same client the offline harness runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  seq: u64,
  last_ack: u64,
  pub refusals: u64,
  /// Digests received, so the panel can say whether checking is happening at
  /// all rather than only whether it has failed.
  pub digests_seen: u64,
  pub snapshots_received: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
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
      sim: SimClient::new(0),
      status: Status::Connecting,
      me: None,
      policy: None,
      seq: 0,
      last_ack: 0,
      refusals: 0,
      digests_seen: 0,
      snapshots_received: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
      resume_drops: 0,
    }
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// This client's best estimate of server time now: the fitted clock, floored
  /// by the newest stamp carried forward at wall rate.
  pub fn server_time_ms(&self) -> u64 {
    self.pump.server_time_ms(self.now_ms)
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

  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    let clock = &self.pump.timeline().clock;
    (
      clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      clock.sample_count(),
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
    self.pump.send_op(&Op::Want {
      seq: self.seq,
      cell,
      kind,
      upgrade,
    });
  }

  /// Drains the socket, folds in what arrived, and advances the simulation.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    if self.digests_seen > 0 && plaza_ws::trim_backlog(&mut events, BACKLOG_TRIGGER, BACKLOG_KEEP).is_some() {
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
        Arrival::Ops(frame) => self.on_ops(frame.body(), controls),
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;
  }

  /// Runs the local simulation up to the clock, then sends any request the
  /// simulation decided it needs. Call once per frame, after `poll`.
  pub fn tick(&mut self, controls: &Controls) {
    if !self.is_playing() {
      return;
    }
    self.sim.run_to(self.target_tick(), controls);
    if let Some(op) = self.sim.take_request() {
      self.pump.send_op(&op);
    }
  }

  fn on_ops(&mut self, body: &[u8], controls: &Controls) {
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
          self.pump.timeline_mut().note_stamp(server_time_ms, self.now_ms);
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
          self.pump.timeline_mut().note_stamp(server_time_ms, self.now_ms);
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

#[cfg(test)]
mod tests {
  use plaza_wire::frame;
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::sim::rules::Field;
  use crate::sim::server::{Server, WORLD_SEED};

  fn feed(socket: &ScriptedSocket, ops: Vec<Op>) {
    let mut buf = Vec::new();
    frame::begin(frame::Kind::Ops, &mut buf);
    WIRE.encode_into(&ops, &mut buf).expect("encode");
    socket.feed_message(buf);
  }

  /// Every op sent so far, decoded through the same codec the client encodes
  /// with, so a test cannot pass while the two ends disagree about the format.
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
    let mut server = Server::new(1, WORLD_SEED);
    server.take_seat(0);
    feed(socket, vec![server.welcome(0, &controls())]);
    client.poll(0, &controls());
    client
  }

  #[test]
  fn a_welcome_hands_over_the_seed_and_the_client_starts_playing() {
    let socket = ScriptedSocket::new();
    let client = welcomed(&socket);
    assert_eq!(client.status, Status::Playing);
    assert_eq!(client.me, Some(0));
    assert_eq!(client.sim.seed, WORLD_SEED, "the one number the whole world comes from");
  }

  #[test]
  fn a_disagreeing_digest_puts_a_request_on_the_wire() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket);
    let c = controls();

    // Reach a tick, then hand it a digest for that tick that cannot match.
    client.poll(2_000, &c);
    client.tick(&c);
    let at = client.sim.tick();
    assert!(at > 0, "the simulation ran");

    feed(&socket, vec![Op::Digest {
      tick: at,
      digest: 0xDEAD_BEEF,
      enemies: 99,
    }]);
    client.poll(2_050, &c);
    client.tick(&c);

    assert_eq!(client.sim.mismatches, 1);
    assert!(
      sent_ops(&socket).iter().any(|op| matches!(op, Op::WantSnapshot { .. })),
      "and it asked for the state"
    );
  }

  #[test]
  fn a_snapshot_replaces_the_field_and_stops_the_asking() {
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket);
    let c = controls();
    client.poll(2_000, &c);
    client.tick(&c);

    let mut field = Field::default();
    field.tick = 500;
    field.gold = 12_345;
    feed(&socket, vec![Op::Snapshot {
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
    let socket = ScriptedSocket::new();
    let mut client = welcomed(&socket);
    let towers = client.sim.field.towers.len();
    client.want_build(Cell::new(4, 5), TowerKind::Arrow, false);

    assert!(sent_ops(&socket).iter().any(|op| matches!(op, Op::Want { .. })));
    assert_eq!(client.sim.field.towers.len(), towers, "and nothing was built locally");
  }
}
