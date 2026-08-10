//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! There is no simulation here at all. The server owns the solver, this draws
//! what arrives and counts what it cost, which is the whole measurement the
//! example exists to produce.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::pack;
use crate::protocol::{CubeState, Cubes, Drive, FrameUpdate, YardOp, PROTOCOL};

const WIRE: MsgPackCodec = MsgPackCodec;

/// Bandwidth is averaged over this window, so the panel reads as a rate rather
/// than as whatever the last packet happened to be.
const WINDOW_MS: u64 = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// What the wire cost, measured where it actually lands.
#[derive(Default)]
pub struct Meter {
  /// `(arrival_ms, bytes)` inside the averaging window.
  recent: VecDeque<(u64, usize)>,
  pub total_bytes: u64,
  pub frames: u64,
}

impl Meter {
  fn record(&mut self, now_ms: u64, bytes: usize) {
    self.recent.push_back((now_ms, bytes));
    self.total_bytes += bytes as u64;
    self.frames += 1;
    while let Some((at, _)) = self.recent.front() {
      if now_ms.saturating_sub(*at) > WINDOW_MS {
        self.recent.pop_front();
      } else {
        break;
      }
    }
  }

  /// Kilobits per second over the last [`WINDOW_MS`].
  ///
  /// Divided by the whole window rather than by the span of what is still in
  /// it, so a link that has gone quiet reads as falling to zero instead of
  /// quoting the rate it had when the last packet landed.
  pub fn kbps(&self, now_ms: u64) -> f32 {
    let bytes: usize = self
      .recent
      .iter()
      .filter(|(at, _)| now_ms.saturating_sub(*at) <= WINDOW_MS)
      .map(|(_, b)| b)
      .sum();
    bytes as f32 * 8.0 / WINDOW_MS as f32
  }

  /// Mean bytes in one frame, which is the number the packing stages move.
  pub fn bytes_per_frame(&self) -> f32 {
    if self.frames == 0 {
      0.0
    } else {
      self.total_bytes as f32 / self.frames as f32
    }
  }
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  /// The wire index of the cube this client drives.
  pub mine: Option<u16>,
  pub cubes: Vec<CubeState>,
  pub frame: u64,
  pub meter: Meter,
  /// Whether the frames arriving are bit-packed, for the panel to name.
  pub packed: bool,
  /// Frames whose packed payload would not read back. Must stay zero: it means
  /// the layout and its reader have drifted apart.
  pub unreadable: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
  sent: Option<Drive>,
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
      mine: None,
      cubes: Vec::new(),
      frame: 0,
      meter: Meter::default(),
      packed: false,
      unreadable: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
      sent: None,
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  pub fn ready(&self) -> bool {
    !self.cubes.is_empty()
  }

  pub fn now_ms(&self) -> u64 {
    self.now_ms
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
        Arrival::Ops(frame) => {
          // Measured as it lands, before decoding: the bytes the link carried
          // are the bytes the technique has to reduce.
          self.meter.record(now_ms, frame.body().len());
          self.on_ops(frame.body());
        }
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
    let Ok(ops) = WIRE.decode::<Vec<YardOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        YardOp::Seated { cube } => self.mine = Some(cube),
        YardOp::Frame(update) => self.on_frame(*update),
        YardOp::Drive(_) => {}
      }
    }
  }

  fn on_frame(&mut self, update: FrameUpdate) {
    self.pump.timeline_mut().note_stamp(update.server_time_ms, self.now_ms);
    self.frame = update.frame;
    self.packed = update.cubes.is_packed();
    match update.cubes {
      Cubes::Full(cubes) => self.cubes = cubes,
      Cubes::Packed(payload) => match pack::unpack(payload.as_slice()) {
        Some(cubes) => self.cubes = cubes,
        None => self.unreadable += 1,
      },
    }
  }

  /// Sends the held direction, and only when it changes: a level repeats on the
  /// server until replaced, so resending it every frame is pure noise on a link
  /// this example is trying to measure.
  pub fn drive(&mut self, drive: Drive) {
    if self.sent == Some(drive) {
      return;
    }
    self.sent = Some(drive);
    self.pump.send_op(&YardOp::Drive(drive));
  }

  /// The cube this client drives, if the server has named one and sent it.
  pub fn mine_state(&self) -> Option<&CubeState> {
    self.cubes.get(self.mine? as usize)
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame;
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::protocol::frame_to_ms;

  fn feed(socket: &ScriptedSocket, ops: Vec<YardOp>) -> usize {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    // The meter counts the body, not the one-byte kind tag ahead of it, since
    // the tag is the same at every stage and the body is what packing moves.
    let body = bytes.len() - 1;
    socket.feed_message(bytes);
    body
  }

  fn yard(cubes: usize) -> Vec<CubeState> {
    (0..cubes)
      .map(|i| CubeState {
        pos: [i as f32, 1.0, 0.0],
        rot: [0.0, 0.0, 0.0, 1.0],
        linvel: [0.0; 3],
        at_rest: false,
      })
      .collect()
  }

  fn frame_op(frame: u64, cubes: usize) -> YardOp {
    YardOp::Frame(Box::new(FrameUpdate {
      frame,
      server_time_ms: frame_to_ms(frame),
      yours: None,
      cubes: Cubes::Full(yard(cubes)),
    }))
  }

  #[test]
  fn a_frame_becomes_the_world_to_draw() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![YardOp::Seated { cube: 3 }, frame_op(1, 8)]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(client.ready());
    assert_eq!(client.cubes.len(), 8);
    assert_eq!(client.mine, Some(3));
    assert!(client.mine_state().is_some());
  }

  #[test]
  fn the_meter_counts_what_the_link_carried() {
    let socket = ScriptedSocket::new();
    let bytes = feed(&socket, vec![frame_op(1, 100)]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert_eq!(client.meter.total_bytes, bytes as u64);
    assert_eq!(client.meter.frames, 1);
    assert!(client.meter.bytes_per_frame() > 100.0, "100 cubes is not free");
  }

  #[test]
  fn a_held_direction_is_sent_once_not_every_frame() {
    let socket = ScriptedSocket::new();
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    let held = Drive { dx: 1, dz: 0, jump: false };
    client.drive(held);
    let after_first = socket.sent().len();
    client.drive(held);
    client.drive(held);
    assert_eq!(socket.sent().len(), after_first, "a repeat is not a message");

    client.drive(Drive { dx: -1, dz: 0, jump: false });
    assert!(socket.sent().len() > after_first, "a change is");
  }

  #[test]
  fn the_rate_falls_back_to_zero_when_nothing_arrives() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![frame_op(1, 10)]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(client.meter.kbps(0) >= 0.0);
    // Long after the window has passed, the rate is not still quoting old bytes.
    client.poll(10_000);
    assert_eq!(client.meter.kbps(10_000), 0.0);
  }
}
