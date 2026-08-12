//! A client that owns where it is.
//!
//! Backwards from every other example in this tree, and the difference is worth
//! stating precisely: there is no predicted state here, so there is nothing to
//! reconcile and nothing to ease off. The local character moves because the
//! player moved it, and the position sent to the server is not a request that
//! will come back corrected, it is the truth being reported.
//!
//! Which leaves exactly one case to handle, and it is not smoothing: a
//! `Refused`. An honest client never sees one, so easing it would be easing a
//! cheat back into place. It snaps.
//!
//! Everyone **else** is a remote position arriving at the tick rate, and that
//! is ordinary: they interpolate, exactly as horde_playground's do.

use std::collections::HashMap;
use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{Authority, Because, Frame, GowOp, Seen, PROTOCOL, TICK_HZ};
use crate::relevance::Seat;

const WIRE: MsgPackCodec = MsgPackCodec;
const WINDOW_MS: u64 = 1000;

/// How often a position is reported, which is a bandwidth decision rather than
/// a fidelity one.
///
/// Sending on every rendered frame would trade a 144Hz client's whole frame
/// rate for detail nobody can see: the server tests a claim rather than
/// integrating it, so a report between ticks changes nothing anyone is told.
pub const SEND_EVERY_MS: u64 = 1000 / TICK_HZ;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// Bytes over a rolling window, in the unit the other examples report.
#[derive(Default)]
pub struct Meter {
  recent: VecDeque<(u64, usize)>,
  pub total_bytes: u64,
  pub frames: u64,
  since_ms: Option<u64>,
}

impl Meter {
  pub fn record(&mut self, now_ms: u64, bytes: usize) {
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

  pub fn kib_per_sec(&self, now_ms: u64) -> f32 {
    let bytes: usize = self
      .recent
      .iter()
      .filter(|(at, _)| now_ms.saturating_sub(*at) <= WINDOW_MS)
      .map(|(_, b)| b)
      .sum();
    bytes as f32 * 1000.0 / WINDOW_MS as f32 / 1024.0
  }

  pub fn session_kib_per_sec(&self, now_ms: u64) -> f32 {
    let Some(since) = self.since_ms else {
      return 0.0;
    };
    let elapsed = now_ms.saturating_sub(since).max(1) as f32 / 1000.0;
    self.total_bytes as f32 / elapsed / 1024.0
  }
}

/// Somebody else, and where they were the last two times we heard.
#[derive(Clone, Copy, Debug)]
pub struct Other {
  pub seen: Seen,
  /// Where they were on the previous frame, to interpolate from.
  pub was: (f32, f32, f32),
  pub since_ms: u64,
}

impl Other {
  /// Where to draw them, eased across the gap between ticks.
  pub fn drawn_at(&self, now_ms: u64) -> (f32, f32, f32) {
    let t = ((now_ms.saturating_sub(self.since_ms)) as f32 / SEND_EVERY_MS as f32).clamp(0.0, 1.0);
    (
      self.was.0 + (self.seen.at.0 - self.was.0) * t,
      self.was.1 + (self.seen.at.1 - self.was.1) * t,
      self.was.2 + (self.seen.at.2 - self.was.2) * t,
    )
  }
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub seat: Option<Seat>,
  /// Where the player is, which is the truth rather than a prediction of one.
  pub at: (f32, f32, f32),
  /// Whether the server's first frame has said where this client starts.
  ///
  /// Taken from the wire rather than computed from the seat, so the spawn is
  /// derived once. A client that ran the same placement itself would be a
  /// second derivation of one fact, and those drift.
  pub seeded: bool,
  pub others: HashMap<Seat, Other>,
  pub tick: u64,
  pub meter: Meter,
  /// Claims the server threw out. Zero for an honest client, which is what
  /// makes it worth showing.
  pub refused: u64,
  /// Casts that went off nearby since the last frame, for the client to play
  /// once and forget.
  pub landed: Vec<Seat>,
  /// Which mode the zone said it is running, on the last frame.
  pub authority: Authority,
  /// How far the server's idea of this client's own position is from the
  /// client's, right now.
  ///
  /// The measurement the whole comparison comes down to. Under client
  /// authority it is the send interval's worth of travel and nothing else.
  /// Under server authority it is a round trip of it, because the local
  /// character has moved and the answer has not come back yet.
  pub gap: f32,
  pub worst_gap: f32,
  now_ms: u64,
  last_sent_ms: u64,
  last_sent_at: Option<(f32, f32, f32)>,
  last_intent: Option<(u32, i8)>,
  events: Vec<Event>,
  arrivals: Vec<Arrival>,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(
      FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?,
    ))
  }

  pub fn from_socket(socket: Box<dyn plaza_ws::Socket>) -> Self {
    Self::from_pump(FramePump::new(socket, WIRE, PROTOCOL))
  }

  fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      status: Status::Connecting,
      seat: None,
      at: (0.0, 0.0, 0.0),
      seeded: false,
      others: HashMap::new(),
      tick: 0,
      meter: Meter::default(),
      refused: 0,
      landed: Vec::new(),
      authority: Authority::Client,
      gap: 0.0,
      worst_gap: 0.0,
      now_ms: 0,
      last_sent_ms: 0,
      last_sent_at: None,
      last_intent: None,
      events: Vec::new(),
      arrivals: Vec::new(),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.now_ms
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  pub fn ready(&self) -> bool {
    self.seat.is_some()
  }

  /// The party, which is who to draw a frame for whether or not they are in
  /// view.
  pub fn party(&self) -> impl Iterator<Item = &Other> {
    self.others.values().filter(|o| o.seen.because.is_subscribed())
  }

  /// Who is close enough to draw a body for.
  pub fn in_view(&self) -> impl Iterator<Item = &Other> {
    self.others.values().filter(|o| o.seen.because.is_near())
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
    let Ok(ops) = WIRE.decode::<Vec<GowOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        GowOp::Seated { seat } => self.seat = Some(seat),
        GowOp::World(frame) => self.on_frame(*frame),
        GowOp::Refused { at } => {
          // No easing, on purpose. An honest client never gets here, so a
          // smooth correction would only ever be smoothing a cheat.
          self.refused += 1;
          self.at = at;
          self.last_sent_at = None;
        }
        GowOp::Moved { .. }
        | GowOp::Intent { .. }
        | GowOp::Cast { .. }
        | GowOp::Party { .. }
        | GowOp::Unparty => {}
      }
    }
  }

  fn on_frame(&mut self, frame: Frame) {
    self.tick = frame.tick;
    self.landed = frame.landed;
    self.authority = frame.authority;
    let now = self.now_ms;
    let mine = self.seat;

    for seen in &frame.characters {
      if Some(seen.seat) == mine {
        if !self.seeded {
          // Taken once, to learn where the zone put us.
          self.at = seen.at;
          self.seeded = true;
        } else if self.authority == Authority::Server {
          // The server owns it, so this is not an echo, it is the answer. No
          // prediction and no reconciliation: the character is drawn where the
          // server last said, which is what makes the round trip visible
          // rather than hidden, and visible is the point of the comparison.
          self.gap = crate::movement::distance(self.at, seen.at);
          self.at = seen.at;
        } else {
          // Under client authority it *is* an echo of what we already said, so
          // the distance to it is the round trip's worth of travel and nothing
          // is applied.
          self.gap = crate::movement::distance(self.at, seen.at);
        }
        self.worst_gap = self.worst_gap.max(self.gap);
        continue;
      }
      self
        .others
        .entry(seen.seat)
        .and_modify(|other| {
          other.was = other.drawn_at(now);
          other.seen = *seen;
          other.since_ms = now;
        })
        .or_insert(Other {
          seen: *seen,
          was: seen.at,
          since_ms: now,
        });
    }

    // A frame is the whole audience, not a delta, so absence means out of it.
    // Safe here in a way it is not over an unreliable transport: a lost frame
    // would otherwise despawn everybody at once.
    let present: std::collections::HashSet<Seat> = frame.characters.iter().map(|s| s.seat).collect();
    self.others.retain(|seat, _| present.contains(seat));
  }

  /// Reports the direction being held, for when the server owns the position.
  ///
  /// Sent at the same rate a position is, and only on a change, so the two
  /// arms of the comparison are not separated by their send policy.
  pub fn intend(&mut self, yaw: f32, forward: i8) {
    if self.now_ms.saturating_sub(self.last_sent_ms) < SEND_EVERY_MS {
      return;
    }
    if self.last_intent == Some((yaw.to_bits(), forward)) {
      return;
    }
    self.last_sent_ms = self.now_ms;
    self.last_intent = Some((yaw.to_bits(), forward));
    self.pump.send_op(&GowOp::Intent { yaw, forward });
  }

  /// Clears the worst-case reading, for comparing one mode against the other
  /// in one session.
  pub fn forget_the_worst(&mut self) {
    self.worst_gap = 0.0;
    self.refused = 0;
  }

  /// Reports where the player is, at the send rate and only when it changed.
  ///
  /// The local position is authoritative, so this is a report rather than a
  /// request: nothing waits on the answer and nothing here is rolled back.
  pub fn moved_to(&mut self, at: (f32, f32, f32)) {
    self.at = at;
    if self.now_ms.saturating_sub(self.last_sent_ms) < SEND_EVERY_MS {
      return;
    }
    // Standing still costs nothing, which is most of a zone most of the time.
    if self.last_sent_at == Some(at) {
      return;
    }
    self.last_sent_ms = self.now_ms;
    self.last_sent_at = Some(at);
    self.pump.send_op(&GowOp::Moved { at });
  }

  pub fn cast(&mut self, ability: u8, cast_ms: u32) {
    self.pump.send_op(&GowOp::Cast { ability, cast_ms });
  }

  pub fn party_with(&mut self, seat: Seat) {
    self.pump.send_op(&GowOp::Party { seat });
  }

  pub fn unparty(&mut self) {
    self.pump.send_op(&GowOp::Unparty);
  }

  /// What a character is doing, for the nameplate.
  pub fn casting_of(&self, seat: Seat) -> Option<u32> {
    self.others.get(&seat).and_then(|o| o.seen.casting_ms)
  }

  pub fn because_of(&self, seat: Seat) -> Option<Because> {
    self.others.get(&seat).map(|o| o.seen.because)
  }
}
