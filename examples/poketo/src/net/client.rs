//! A client on the real wire, and two regimes to hold at once.
//!
//! What it keeps depends on which it is in, and the two are not a mode flag
//! here either: an overworld frame replaces the world, a battle frame replaces
//! the battle, and being in one means the other is simply absent. A client
//! holding a stale battle while walking around is the same bug as a body
//! standing in the grass while its owner is elsewhere, one side of the wire
//! along.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::battle::Choice;
use crate::grid::{Facing, Trainer};
use crate::protocol::{BattleState, Overworld, PoketoOp, PROTOCOL};

const WIRE: MsgPackCodec = MsgPackCodec;
const WINDOW_MS: u64 = 1000;

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

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub seat: Option<u16>,
  /// Kept across a reconnection by whoever owns this client. It is the only
  /// thing linking a new connection to what the last one was doing.
  pub token: Option<u64>,
  /// The overworld, when in it.
  pub world: Option<Overworld>,
  /// The battle, when in one. Never both.
  pub battle: Option<BattleState>,
  pub meter: Meter,
  /// Battles entered, for the panel: the number that says the two regimes are
  /// actually being switched between rather than one being simulated.
  pub battles_seen: u64,
  now_ms: u64,
  held: Option<Facing>,
  sent: Option<Option<Facing>>,
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
      token: None,
      world: None,
      battle: None,
      meter: Meter::default(),
      battles_seen: 0,
      now_ms: 0,
      held: None,
      sent: None,
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

  pub fn battling(&self) -> bool {
    self.battle.is_some()
  }

  pub fn ready(&self) -> bool {
    self.seat.is_some() && (self.world.is_some() || self.battle.is_some())
  }

  /// The trainers to draw, which is nothing at all while in a battle.
  pub fn trainers(&self) -> &[Trainer] {
    self.world.as_ref().map(|w| w.trainers.as_slice()).unwrap_or(&[])
  }

  pub fn mine(&self) -> Option<&Trainer> {
    let seat = self.seat?;
    self.trainers().iter().find(|t| t.seat == seat)
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
          // Offered before anything else, and harmless if unknown: a failed
          // resume and a first join are the same situation.
          if let Some(token) = self.token {
            self.pump.send_op(&PoketoOp::Resume { token });
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
    let Ok(ops) = WIRE.decode::<Vec<PoketoOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        PoketoOp::Seated { seat, token } => {
          self.seat = Some(seat);
          self.token = Some(token);
        }
        PoketoOp::World(world) => {
          // Arriving in the overworld is what ends a battle on this side, so a
          // client cannot be drawing a battle it is no longer in.
          self.battle = None;
          self.world = Some(*world);
        }
        PoketoOp::Battle(battle) => {
          if self.battle.is_none() {
            self.battles_seen += 1;
          }
          self.world = None;
          self.battle = Some(*battle);
        }
        PoketoOp::Returned => {
          self.battle = None;
        }
        _ => {}
      }
    }
  }

  /// Sends the held direction, and only when it changes.
  pub fn walk(&mut self, facing: Option<Facing>) {
    self.held = facing;
    if self.sent == Some(facing) {
      return;
    }
    self.sent = Some(facing);
    self.pump.send_op(&PoketoOp::Walk(facing));
  }

  /// Answers the current turn.
  ///
  /// Addressed to the turn it is for, so pressing twice, or a resend after a
  /// reconnection, is silence rather than a second move.
  pub fn choose(&mut self, choice: Choice) {
    let Some(state) = &self.battle else {
      return;
    };
    let turn = state.battle.turn;
    self.pump.send_op(&PoketoOp::Choose { turn, choice });
  }
}
