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

use crate::protocol::{Authority, Because, Frame, GowOp, Landed, Seen, You, PROTOCOL, TICK_HZ};
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

/// An ability going off, kept long enough to draw.
///
/// A landing is an event, so this is the client's own memory of one: nothing
/// on the wire mentions it again, and an animation outlives the frame that
/// carried it by design.
#[derive(Clone, Copy, Debug)]
pub struct Effect {
  pub ability: u8,
  pub seat: Seat,
  pub victim: Option<Seat>,
  pub since_ms: u64,
}

impl Effect {
  /// How long each kind stays on screen. A swing is short because the arm has
  /// to be back where the body is; a heal lingers because it is the only one
  /// with nothing violent to say.
  pub fn life_ms(&self) -> u64 {
    match self.ability {
      0 => 320,
      1 => 420,
      _ => 700,
    }
  }

  /// How far through, `0.0..1.0`.
  pub fn age(&self, now_ms: u64) -> f32 {
    (now_ms.saturating_sub(self.since_ms) as f32 / self.life_ms() as f32).clamp(0.0, 1.0)
  }

  pub fn done(&self, now_ms: u64) -> bool {
    now_ms.saturating_sub(self.since_ms) >= self.life_ms()
  }
}

/// A number that floated off somebody when their health moved.
#[derive(Clone, Copy, Debug)]
pub struct Popup {
  pub at: (f32, f32, f32),
  /// Positive for damage taken, negative for healing.
  pub amount: i32,
  pub since_ms: u64,
  /// Whether it happened to the local player, which is drawn louder.
  pub mine: bool,
}

impl Popup {
  /// How long one stays on screen.
  pub const LIFE_MS: u64 = 1100;

  /// How far through its life it is, `0.0..1.0`.
  pub fn age(&self, now_ms: u64) -> f32 {
    (now_ms.saturating_sub(self.since_ms) as f32 / Self::LIFE_MS as f32).clamp(0.0, 1.0)
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
  /// How fast they are moving over the ground, in units a second.
  ///
  /// Derived from the two samples the interpolation already keeps rather than
  /// sent, because a walk cycle is presentation and nothing on the wire should
  /// pay for it.
  pub fn speed(&self) -> f32 {
    let gap = SEND_EVERY_MS as f32 / 1000.0;
    crate::movement::ground_distance(self.was, self.seen.at) / gap.max(0.001)
  }
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
  /// How the last frame reached this client, and how its positions were
  /// written. Read off the wire rather than off the dial, so the panel shows
  /// what the server is doing rather than what it was last asked to do.
  pub delivery: crate::protocol::Delivery,
  pub precision: crate::protocol::Precision,
  /// Cell payloads received this tick under [`Delivery::Cells`](crate::protocol::Delivery::Cells),
  /// drained when the frame that terminates the tick arrives.
  cells: Vec<crate::protocol::Packed>,
  pub others: HashMap<Seat, Other>,
  /// Everything the server says about you, which is where the local interface
  /// reads health, mana, the cast bar and the cooldown from. Nothing here is
  /// derivable from `others`, which never contains your own seat.
  pub you: Option<You>,
  /// The last spawn counter this client acted on, so a relocation is applied
  /// exactly once however many frames repeat it.
  spawn_seen: u32,
  /// Where the server put us, waiting to be taken by the frame loop.
  teleport: Option<(f32, f32, f32)>,
  pub tick: u64,
  pub meter: Meter,
  /// Claims the server threw out. Zero for an honest client, which is what
  /// makes it worth showing.
  pub refused: u64,
  /// Casts that went off nearby since the last frame.
  ///
  /// Replaced every frame, so reading it is the only chance to see one.
  pub landed: Vec<Landed>,
  /// When each seat's cast last landed, on the client's clock.
  ///
  /// Kept because a landing is an **event**: no later frame mentions it, so a
  /// client that does not remember one has no way to draw it for longer than
  /// the single frame it arrived in. This is the client-side half of what
  /// makes an event different from a state, and forgetting it is why the
  /// server bothers to send it at all.
  pub flashes: HashMap<Seat, u64>,
  /// Who the next ability is aimed at.
  pub target: Option<Seat>,
  /// Health changes worth showing, and when each was noticed.
  ///
  /// Derived from watching the number move rather than sent, because the
  /// server already says what everyone's health is and a second message
  /// saying by how much it changed would be two derivations of one fact.
  pub popups: Vec<Popup>,
  /// Abilities going off nearby, for as long as each is worth drawing.
  pub effects: Vec<Effect>,
  health_was: HashMap<Seat, u16>,
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
  last_sent_yaw: f32,
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
      delivery: crate::protocol::Delivery::default(),
      precision: crate::protocol::Precision::default(),
      cells: Vec::new(),
      others: HashMap::new(),
      you: None,
      spawn_seen: 0,
      teleport: None,
      tick: 0,
      meter: Meter::default(),
      refused: 0,
      landed: Vec::new(),
      flashes: HashMap::new(),
      target: None,
      popups: Vec::new(),
      effects: Vec::new(),
      health_was: HashMap::new(),
      authority: Authority::Client,
      gap: 0.0,
      worst_gap: 0.0,
      now_ms: 0,
      last_sent_ms: 0,
      last_sent_at: None,
      last_sent_yaw: 0.0,
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

  /// Whether there is a character to draw and move.
  ///
  /// Both halves, because a seat without a position is a client that knows its
  /// number and not where it is standing.
  pub fn ready(&self) -> bool {
    self.seat.is_some() && self.seeded
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
        // Held until the tick's frame arrives, which is what says the cells
        // are all in. Applying them as they land would draw a half-assembled
        // world on any tick whose ops were split across reads.
        GowOp::Cell(payload) => self.cells.push(payload),
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
        | GowOp::Target { .. }
        | GowOp::Party { .. }
        | GowOp::Unparty => {}
      }
    }
  }

  fn on_frame(&mut self, frame: Frame) {
    self.tick = frame.tick;
    self.you = frame.you;
    if let Some(you) = frame.you {
      self.target = you.target;
      if you.spawn != self.spawn_seen {
        self.spawn_seen = you.spawn;
        self.at = you.at;
        self.teleport = Some(you.at);
        // Or the send filter decides nothing changed and the server never
        // hears where we actually are.
        self.last_sent_at = None;
      }
    }
    // Unpacked once. The bodies are shared cell payloads plus per-client
    // extras, labelled at decode; see `crate::pack`. Under `Delivery::Cells`
    // they arrived as their own ops and are drained here, so the two modes
    // converge on one list before anything reads it.
    let cells = std::mem::take(&mut self.cells);
    let bodies = frame.seen_with(&cells);
    self.landed = frame.landed;
    self.authority = frame.authority;
    self.delivery = frame.delivery;
    self.precision = frame.precision;
    for hit in &self.landed {
      self.flashes.insert(hit.seat, self.now_ms);
      self.effects.push(Effect {
        ability: hit.ability,
        seat: hit.seat,
        victim: hit.victim,
        since_ms: self.now_ms,
      });
    }
    let now = self.now_ms;
    let mine = self.seat;

    // Your own body comes from `you`, not from the audience.
    //
    // Not a tidy-up: the audience is quantised, and this is the one position in
    // the frame whose exactness is a claim the example makes. A 2mm rounding
    // here would put a floor under "client authority cannot disagree with
    // itself" and the number that says so would never read zero again.
    if let Some(you) = frame.you {
      if !self.seeded {
        // Taken once, to learn where the zone put us.
        self.at = you.at;
        self.seeded = true;
      } else if self.authority == Authority::Server {
        // The server owns it, so this is not an echo, it is the answer. No
        // prediction and no reconciliation: the character is drawn where the
        // server last said, which is what makes the round trip visible
        // rather than hidden, and visible is the point of the comparison.
        self.gap = crate::movement::distance(self.at, you.at);
        self.at = you.at;
      } else {
        // Under client authority it *is* an echo of what we already said, so
        // the distance to it is the round trip's worth of travel and nothing
        // is applied.
        self.gap = crate::movement::distance(self.at, you.at);
      }
      self.worst_gap = self.worst_gap.max(self.gap);
    }

    for seen in &bodies {
      if Some(seen.seat) == mine {
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

    for seen in &bodies {
      if let Some(before) = self.health_was.insert(seen.seat, seen.health)
        && before != seen.health
      {
        self.popups.push(Popup {
          at: seen.at,
          amount: before as i32 - seen.health as i32,
          since_ms: now,
          mine: Some(seen.seat) == mine,
        });
      }
    }
    if let Some(you) = frame.you
      && let Some(before) = self.health_was.insert(you.seat, you.health)
      && before != you.health
    {
      self.popups.push(Popup {
        at: self.at,
        amount: before as i32 - you.health as i32,
        since_ms: now,
        mine: true,
      });
    }

    // A frame is the whole audience, not a delta, so absence means out of it.
    // Safe here in a way it is not over an unreliable transport: a lost frame
    // would otherwise despawn everybody at once.
    let present: std::collections::HashSet<Seat> = bodies.iter().map(|s| s.seat).collect();
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
  pub fn moved_to(&mut self, at: (f32, f32, f32), yaw: f32) {
    self.at = at;
    if self.now_ms.saturating_sub(self.last_sent_ms) < SEND_EVERY_MS {
      return;
    }
    // Standing still costs nothing, which is most of a zone most of the time.
    // The facing is part of that: turning on the spot is worth a packet
    // because everyone else draws a body from it.
    if self.last_sent_at == Some(at) && (self.last_sent_yaw - yaw).abs() < 0.05 {
      return;
    }
    self.last_sent_ms = self.now_ms;
    self.last_sent_at = Some(at);
    self.last_sent_yaw = yaw;
    self.pump.send_op(&GowOp::Moved { at, yaw });
  }

  pub fn cast(&mut self, ability: u8) {
    self.pump.send_op(&GowOp::Cast {
      ability,
      cast_ms: crate::abilities::ability(ability).map(|a| a.cast_ms as u32).unwrap_or(0),
    });
  }

  /// Aims at a seat, and remembers it so the interface can say so.
  pub fn aim_at(&mut self, seat: Option<Seat>) {
    self.target = seat;
    self.pump.send_op(&GowOp::Target { seat });
  }

  pub fn party_with(&mut self, seat: Seat) {
    self.pump.send_op(&GowOp::Party { seat });
  }

  pub fn unparty(&mut self) {
    self.pump.send_op(&GowOp::Unparty);
  }

  /// How long a landing stays on screen.
  pub const FLASH_MS: u64 = 350;

  /// Seats whose cast landed recently enough to still be drawn.
  ///
  /// Aged out here rather than on arrival, because the frame that would have
  /// cleared it is the frame that never mentions it again.
  pub fn flashing(&self, now_ms: u64) -> std::collections::HashSet<Seat> {
    self
      .flashes
      .iter()
      .filter(|(_, at)| now_ms.saturating_sub(**at) < Self::FLASH_MS)
      .map(|(seat, _)| *seat)
      .collect()
  }

  /// Drops flashes and popups nobody will draw again, so neither grows for the
  /// length of a session.
  pub fn forget_old_flashes(&mut self, now_ms: u64) {
    let cutoff = Self::FLASH_MS;
    self.flashes.retain(|_, at| now_ms.saturating_sub(*at) < cutoff);
    self
      .popups
      .retain(|p| now_ms.saturating_sub(p.since_ms) < Popup::LIFE_MS);
    self.effects.retain(|e| !e.done(now_ms));
    self.health_was.retain(|seat, _| {
      self.others.contains_key(seat) || Some(*seat) == self.seat
    });
  }

  /// What a character is doing, for the nameplate.
  pub fn casting_of(&self, seat: Seat) -> Option<u32> {
    self.others.get(&seat).and_then(|o| o.seen.casting_ms)
  }

  /// Somewhere the server put this client, to be taken once.
  ///
  /// A respawn moves a character the client otherwise owns, so this is the one
  /// place a position comes *down* the wire and is applied rather than
  /// compared. Everything else about the local player's position is a report.
  pub fn take_teleport(&mut self) -> Option<(f32, f32, f32)> {
    self.teleport.take()
  }

  /// Whether the player is down and waiting to come back up.
  pub fn is_down(&self) -> bool {
    self.you.is_some_and(|you| you.up_in_ms.is_some())
  }

  /// What the local player is casting, if anything, as a share run so far.
  ///
  /// Read from `you` rather than from the audience list, which is the whole
  /// point of that block existing: a client never appears in its own list.
  pub fn my_cast(&self) -> Option<(u8, f32)> {
    let you = self.you?;
    let index = you.casting?;
    let left = you.casting_ms.unwrap_or(0) as f32;
    let total = crate::abilities::ability(index)?.cast_ms.max(1) as f32;
    Some((index, (1.0 - left / total).clamp(0.0, 1.0)))
  }

  /// Whether an ability may be pressed right now, and why not if not.
  pub fn can_cast(&self, index: u8) -> Result<(), &'static str> {
    let Some(you) = self.you else { return Err("not seated") };
    if you.up_in_ms.is_some() {
      return Err("you are down");
    }
    let Some(spell) = crate::abilities::ability(index) else {
      return Err("no such ability");
    };
    if you.casting.is_some() {
      return Err("already casting");
    }
    if you.ready_in_ms > 0 {
      return Err("cooling down");
    }
    if you.mana < spell.mana {
      return Err("not enough mana");
    }
    if spell.hostile && self.target.is_none() {
      return Err("no target");
    }
    Ok(())
  }

  pub fn because_of(&self, seat: Seat) -> Option<Because> {
    self.others.get(&seat).map(|o| o.seen.because)
  }
}
