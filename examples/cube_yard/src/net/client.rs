//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! There is no simulation here at all. The server owns the solver, this draws
//! what arrives and counts what it cost, which is the whole measurement the
//! example exists to produce.

use std::collections::VecDeque;

use plaza_client_utils::interpolation::SnapshotBuffer;
use plaza_client_utils::math::Vec3;
use plaza_client_utils::AdaptiveDecay;
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::pack;
use crate::protocol::{CubeState, Cubes, Drive, FrameUpdate, YardOp, PROTOCOL};

const WIRE: MsgPackCodec = MsgPackCodec;

/// Nominal milliseconds between server frames.
const TICK_MS: u64 = 1000 / crate::protocol::TICK_HZ;

/// One frame's gap, smoothed into the running estimate.
///
/// The raw gap cannot be used directly. A 60Hz stamp advances 16, 17, 17, 16,
/// because 1000/60 is not an integer, so any threshold sitting at 16 is crossed
/// and uncrossed several times a second.
fn smooth_interval(held: u64, gap: u64) -> u64 {
  (held * 3 + gap + 2) / 4
}

/// Whether to draw from the interpolation buffer or from the newest state.
///
/// Two thresholds, because this chooses between **two different sources**: the
/// buffer answers with a position a render delay in the past and the fallback
/// answers with the newest one, so a decision that flips frame to frame swings
/// every cube back and forth by whatever it travels in that delay. With a raw
/// gap and a single threshold at the tick interval it flipped on a repeating
/// three-frame cycle, which at 15 units a second is half a unit of shake, and
/// worst under `--encoding delta` where sparse samples put the two answers
/// furthest apart.
fn should_interpolate(interval_ms: u64, currently: bool) -> bool {
  if currently {
    // Held until the rate is clearly back at tick speed.
    interval_ms * 4 > TICK_MS * 5
  } else {
    interval_ms * 2 > TICK_MS * 3
  }
}

/// Bandwidth is averaged over this window, so the panel reads as a rate rather
/// than as whatever the last packet happened to be.
const WINDOW_MS: u64 = 1000;

/// Samples kept per cube for interpolation. Two is the minimum a spline needs;
/// a few more absorb a packet arriving out of order.
const SAMPLES: usize = 6;

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
  /// When the first packet landed, so a session rate is over the run rather
  /// than over a frame count that assumes a send rate.
  since_ms: Option<u64>,
}

impl Meter {
  fn record(&mut self, now_ms: u64, bytes: usize) {
    self.recent.push_back((now_ms, bytes));
    self.total_bytes += bytes as u64;
    self.frames += 1;
    self.since_ms.get_or_insert(now_ms);
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

  /// Kibibytes a second over the same window, which is the unit
  /// horde_playground reports and the one to read across examples.
  pub fn kib_per_sec(&self, now_ms: u64) -> f32 {
    self.kbps(now_ms) * 1000.0 / 8.0 / 1024.0
  }

  /// Kibibytes a second over the whole run.
  ///
  /// The pair answers two questions: **recent** responds to what just changed
  /// and settles when the world does, **session** is what a configuration
  /// actually cost. Session sits below recent while it is still climbing
  /// toward it, which is a property of an average rather than of the traffic.
  pub fn session_kib_per_sec(&self, now_ms: u64) -> f32 {
    let Some(since) = self.since_ms else {
      return 0.0;
    };
    let elapsed = now_ms.saturating_sub(since).max(1) as f32 / 1000.0;
    self.total_bytes as f32 / elapsed / 1024.0
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
  /// How many cubes the last frame carried, which under a budget is the
  /// number the panel should show rather than the yard's size.
  pub patched: u32,
  /// Position samples per cube, blended in a **straight line** when the send
  /// rate is low enough to leave gaps.
  ///
  /// Deliberately not `HermiteView`, and the measurement is in
  /// `tests/baseline.rs`: across 300 of these cubes at 10Hz the spline came out
  /// 13x worse than the chord, because it left the segment its own samples
  /// bracket on half of all frames. Velocity at a sample is a promise about the
  /// path to the next one, and a pile of colliding cubes breaks that promise
  /// after the packet has left. A chord cannot leave the segment, which is
  /// exactly the property this scene needs.
  views: Vec<SnapshotBuffer<u64, Vec3>>,
  /// Where the render clock is pointed: the newest stamp seen, less a delay of
  /// two send intervals so two real samples bracket it.
  render_at: u64,
  /// Measured gap between frames, which is what sets that delay.
  send_interval_ms: u64,
  last_stamp: Option<u64>,
  /// Where each cube was drawn before its last correction, minus where it is
  /// now, bled off over the following frames. Under a budget a cube can go
  /// several ticks without an update and then move a long way at once, and a
  /// snap that size is exactly what a viewer notices.
  pub offsets: Vec<[f32; 3]>,
  decay: AdaptiveDecay,
  /// The quantised state this client is known to hold, which is what a delta
  /// frame is measured against. Both ends keep it identically, which the TCP
  /// transport is what makes safe.
  baseline: Vec<Option<pack::Quantized>>,
  /// Frames whose packed payload would not read back. Must stay zero: it means
  /// the layout and its reader have drifted apart.
  pub unreadable: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
  stamp: u64,
  interpolating: bool,
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
      views: Vec::new(),
      render_at: 0,
      send_interval_ms: TICK_MS,
      last_stamp: None,
      offsets: Vec::new(),
      decay: AdaptiveDecay::default(),
      baseline: Vec::new(),
      patched: 0,
      unreadable: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
      stamp: 0,
      interpolating: false,
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
    // The send rate is measured rather than configured, so a client does not
    // have to be told what the server chose.
    if let Some(previous) = self.last_stamp {
      let gap = update.server_time_ms.saturating_sub(previous);
      if gap > 0 {
        self.send_interval_ms = smooth_interval(self.send_interval_ms, gap);
      }
    }
    self.last_stamp = Some(update.server_time_ms);
    self.stamp = update.server_time_ms;
    self.frame = update.frame;
    self.packed = update.cubes.is_packed();
    match update.cubes {
      Cubes::Full(cubes) => self.cubes = cubes,
      Cubes::Packed(payload) => match pack::unpack(payload.as_slice()) {
        Some(cubes) => self.cubes = cubes,
        None => self.unreadable += 1,
      },
      // A budgeted frame is a patch, not a world: whatever it does not mention
      // is still whatever this client last heard about it.
      Cubes::Subset(payload) => match pack::unpack_subset(payload.as_slice()) {
        Some(patch) => self.apply(patch),
        None => self.unreadable += 1,
      },
      Cubes::Delta(payload) => match pack::unpack_delta(payload.as_slice(), &mut self.baseline) {
        Some(patch) => self.apply(patch),
        None => self.unreadable += 1,
      },
    }
  }

  /// Patches whatever arrived into the yard this client already holds, keeping
  /// the visual offset of anything that moved so it eases rather than jumps.
  fn apply(&mut self, patch: Vec<(u32, CubeState)>) {
    self.patched = patch.len() as u32;
    let stamp = self.stamp;
    for (index, cube) in patch {
      let index = index as usize;
      if index >= self.cubes.len() {
        self.cubes.resize(index + 1, cube);
        self.offsets.resize(index + 1, [0.0; 3]);
      }
      let was = self.cubes[index].pos;
      for axis in 0..3 {
        self.offsets[index][axis] += was[axis] - cube.pos[axis];
      }
      self.cubes[index] = cube;
      self.sample(index, &cube, stamp);
    }
  }

  /// Files a cube's new position for interpolation.
  fn sample(&mut self, index: usize, cube: &CubeState, stamp: u64) {
    if index >= self.views.len() {
      self.views.resize_with(index + 1, || SnapshotBuffer::new(SAMPLES));
    }
    self.views[index].add_snapshot(stamp, Vec3::new(cube.pos[0], cube.pos[1], cube.pos[2]));
  }

  /// Bleeds off the visual offsets. Call once per rendered frame, with real
  /// seconds: the decay is a rate, not a per-frame constant.
  pub fn ease(&mut self, dt_secs: f32) {
    for offset in &mut self.offsets {
      let magnitude = (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]).sqrt();
      if magnitude < 1e-4 {
        *offset = [0.0; 3];
        continue;
      }
      let keep = self.decay.retain(magnitude, dt_secs);
      for axis in offset.iter_mut() {
        *axis *= keep;
      }
    }
  }

  /// Where to draw a cube: its last known state plus whatever of its last
  /// correction has not bled off yet.
  /// Where to draw a cube.
  ///
  /// Blended between two real samples when the send rate leaves gaps, which is
  /// what a low rate needs; otherwise the last known state plus whatever of its
  /// correction has not bled off. The two are alternatives rather than a blend:
  /// interpolating already puts the cube where it should be, so easing on top
  /// would fight it.
  pub fn drawn(&self, index: usize) -> [f32; 3] {
    if self.interpolating {
      if let Some(view) = self.views.get(index) {
        if let Some(at) = view.get_interpolated_state(self.render_at) {
          return [at.x, at.y, at.z];
        }
      }
    }
    let pos = self.cubes[index].pos;
    match self.offsets.get(index) {
      Some(offset) => [pos[0] + offset[0], pos[1] + offset[1], pos[2] + offset[2]],
      None => pos,
    }
  }

  /// Whether the send rate is low enough that interpolating is worth it.
  ///
  /// At the tick rate a chord is 16ms and a straight line is invisible; the
  /// spline only earns its keep once the gaps are long enough to corner.
  pub fn interpolating(&self) -> bool {
    self.interpolating
  }

  /// Advances the render clock. Call once per rendered frame.
  ///
  /// The target trails the newest stamp by two send intervals so two real
  /// samples bracket it, which is the same trade every remote entity makes:
  /// a fixed, small staleness in exchange for motion assembled from real
  /// states rather than guessed ones.
  pub fn advance_render_clock(&mut self) {
    let delay = self.send_interval_ms * 2;
    self.render_at = self.stamp.saturating_sub(delay);
    self.interpolating = should_interpolate(self.send_interval_ms, self.interpolating);
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

  use crate::pack;

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
  fn a_budgeted_patch_updates_only_what_it_names() {
    let socket = ScriptedSocket::new();
    let mut cubes = yard(10);
    feed(&socket, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: 1,
      server_time_ms: 0,
      yours: None,
      cubes: Cubes::Subset(pack::pack_subset(&cubes, &(0..10).collect::<Vec<_>>()).into()),
    }))]);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert_eq!(client.cubes.len(), 10);

    // A second frame naming two cubes leaves the other eight where they were.
    let before = client.cubes[5].pos;
    cubes[2].pos[1] = 9.0;
    cubes[7].pos[1] = 9.0;
    feed(&socket, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: 2,
      server_time_ms: 16,
      yours: None,
      cubes: Cubes::Subset(pack::pack_subset(&cubes, &[2, 7]).into()),
    }))]);
    client.poll(16);

    assert_eq!(client.patched, 2);
    assert!((client.cubes[2].pos[1] - 9.0).abs() < 0.01);
    assert_eq!(client.cubes[5].pos, before, "an unnamed cube is left alone");
    assert_eq!(client.unreadable, 0);
  }

  #[test]
  fn a_correction_is_eased_rather_than_snapped() {
    let socket = ScriptedSocket::new();
    let mut cubes = yard(4);
    feed(&socket, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: 1,
      server_time_ms: 0,
      yours: None,
      cubes: Cubes::Subset(pack::pack_subset(&cubes, &(0..4).collect::<Vec<_>>()).into()),
    }))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    cubes[1].pos[1] += 5.0;
    feed(&socket, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: 2,
      server_time_ms: 16,
      yours: None,
      cubes: Cubes::Subset(pack::pack_subset(&cubes, &[1]).into()),
    }))]);
    client.poll(16);

    // Drawn short of the new truth, then closing on it.
    let first = client.drawn(1)[1];
    assert!(first < client.cubes[1].pos[1] - 1.0, "the jump should not land at once");
    client.ease(0.1);
    let later = client.drawn(1)[1];
    assert!(later > first && later <= client.cubes[1].pos[1]);

    for _ in 0..40 {
      client.ease(0.05);
    }
    assert!((client.drawn(1)[1] - client.cubes[1].pos[1]).abs() < 0.01, "and it gets there");
  }

  #[test]
  fn a_held_direction_is_sent_once_not_every_frame() {
    let socket = ScriptedSocket::new();
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    let held = Drive { dx: 1, dz: 0, jump: false, rolling: false };
    client.drive(held);
    let after_first = socket.sent().len();
    client.drive(held);
    client.drive(held);
    assert_eq!(socket.sent().len(), after_first, "a repeat is not a message");

    client.drive(Drive { dx: -1, dz: 0, jump: false, rolling: false });
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

#[cfg(test)]
mod render_clock_tests {
  use super::*;

  /// The stamps a 60Hz server actually produces.
  fn tick_gaps(frames: u64) -> Vec<u64> {
    (1..=frames)
      .map(crate::protocol::frame_to_ms)
      .scan(0u64, |previous, at| {
        let gap = at - *previous;
        *previous = at;
        Some(gap)
      })
      .collect()
  }

  #[test]
  fn a_sixty_hertz_server_never_toggles_interpolation() {
    // 1000/60 is not an integer, so the gaps are 16, 17, 17. A threshold at the
    // tick interval reading the raw gap flipped on that cycle, and each flip
    // moved every cube by a render delay's worth of travel.
    let gaps = tick_gaps(600);
    assert!(gaps.contains(&16) && gaps.contains(&17), "{gaps:?}");

    let mut held = TICK_MS;
    let mut on = false;
    let mut flips = 0;
    for gap in gaps {
      held = smooth_interval(held, gap);
      let next = should_interpolate(held, on);
      if next != on {
        flips += 1;
      }
      on = next;
    }
    assert_eq!(flips, 0, "interpolation toggled {flips} times at the tick rate");
    assert!(!on, "and should be off, since a chord at 16ms is invisible");
  }

  #[test]
  fn a_slow_send_rate_turns_it_on_and_keeps_it_on() {
    let mut held = TICK_MS;
    let mut on = false;
    for _ in 0..40 {
      held = smooth_interval(held, 100);
      on = should_interpolate(held, on);
    }
    assert!(on, "at 10 sends a second a chord flattens 100ms of path");

    // And jitter around that rate must not drop it again.
    for gap in [96, 104, 98, 102, 91, 109] {
      held = smooth_interval(held, gap);
      assert!(should_interpolate(held, on), "dropped out at a {gap}ms gap");
    }
  }

  #[test]
  fn the_two_thresholds_do_not_overlap() {
    // Hysteresis only helps while turning on is harder than staying on.
    let on_at = (1..400).find(|ms| should_interpolate(*ms, false)).unwrap();
    let off_at = (1..400).find(|ms| should_interpolate(*ms, true)).unwrap();
    assert!(off_at < on_at, "on at {on_at}ms, stays on from {off_at}ms");
  }
}
