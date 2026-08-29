//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! Deliberately thin: the server already cut this side's view, so there is no
//! filtering to do here, only decoding, moments for the window to spend and
//! the offer state the defender acts on.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{BattlePhase, Cell, FieldView, OfferView, Order, UnitId, WatchOp, PROTOCOL};

const WIRE: MsgPackCodec = MsgPackCodec;

const LOG_KEEP: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// A thing that just happened, drained once by the window and spent on
/// effects.
#[derive(Clone, Debug, PartialEq)]
pub enum Moment {
  Stepped { unit: UnitId, at: Cell },
  Shot { shooter: UnitId, target: UnitId, felled: bool, overwatch: bool },
  OfferOpened(OfferView),
  RoundStarted(u32),
  SideToAct(u8),
  BattleStarted,
  BattleOver { winner: u8 },
  Refused(String),
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub my_side: Option<u8>,
  pub view: Option<FieldView>,
  pub log: VecDeque<String>,
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
      my_side: None,
      view: None,
      log: VecDeque::new(),
      moments: Vec::new(),
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// Whether this client owes the next activation.
  pub fn my_activation(&self) -> bool {
    match (&self.view, self.my_side) {
      (Some(view), Some(side)) => {
        view.phase == BattlePhase::Fighting && view.side_to_act == side && view.marching.is_none()
      }
      _ => false,
    }
  }

  /// The offer waiting on this client, when there is one.
  pub fn my_offer(&self) -> Option<OfferView> {
    let side = self.my_side?;
    let view = self.view.as_ref()?;
    view
      .offer
      .filter(|o| view.yours.iter().any(|u| u.id == o.watcher && u.side == side))
  }

  pub fn act(&mut self, order: Order) {
    self.pump.send_op(&WatchOp::Act(order));
  }

  pub fn answer(&mut self, watcher: UnitId, fire: bool) {
    self.pump.send_op(&WatchOp::Answer { watcher, fire });
  }

  fn note(&mut self, line: String) {
    self.log.push_front(line);
    self.log.truncate(LOG_KEEP);
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
    let Ok(ops) = WIRE.decode::<Vec<WatchOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        WatchOp::Snapshot(view) => {
          self.pump.timeline_mut().note_stamp(view.server_now_ms, self.now_ms);
          self.view = Some(*view);
        }
        WatchOp::YouAre { side } => {
          self.my_side = Some(side);
          self.note(format!("you command {}", ["blue", "red"][side as usize % 2]));
        }
        WatchOp::Stepped { unit, at } => self.moments.push(Moment::Stepped { unit, at }),
        WatchOp::Shot {
          shooter,
          target,
          felled,
          overwatch,
          ..
        } => {
          if overwatch {
            self.note(format!("overwatch! unit {shooter} fires on unit {target}"));
          }
          if felled {
            self.note(format!("unit {target} falls"));
          }
          self.moments.push(Moment::Shot {
            shooter,
            target,
            felled,
            overwatch,
          });
        }
        WatchOp::NowWatching { unit } => self.note(format!("unit {unit} holds its fire and watches")),
        WatchOp::OfferOpened(offer) => {
          self.note("your watcher has a shot: fire or hold".to_owned());
          self.moments.push(Moment::OfferOpened(offer));
        }
        WatchOp::RoundStarted { round } => self.moments.push(Moment::RoundStarted(round)),
        WatchOp::SideToAct { side } => self.moments.push(Moment::SideToAct(side)),
        WatchOp::BattleStarted { battle } => {
          self.note(format!("skirmish {battle}"));
          self.moments.push(Moment::BattleStarted);
        }
        WatchOp::BattleOver { winner } => {
          self.note(format!("{} side takes the field", ["blue", "red"][winner as usize % 2]));
          self.moments.push(Moment::BattleOver { winner });
        }
        WatchOp::Refused { reason } => {
          self.note(format!("refused: {reason}"));
          self.moments.push(Moment::Refused(reason));
        }
        _ => {}
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, ProtocolVersion};
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;

  fn feed(socket: &ScriptedSocket, ops: Vec<WatchOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  #[test]
  fn an_offer_is_only_yours_when_your_unit_holds_it() {
    use crate::protocol::{BattlePhase, FieldView, Panel, Stance, Unit};
    let socket = ScriptedSocket::new();
    let mine = Unit {
      id: 3,
      side: 1,
      at: (8, 0),
      hp: 2,
      stance: Stance::Watching,
      acted: true,
      alive: true,
    };
    let view = FieldView {
      phase: BattlePhase::Fighting,
      server_now_ms: 0,
      battle: 1,
      round: 1,
      side_to_act: 0,
      seats: vec![1, 2],
      commanders: [1, 2],
      you: 1,
      yours: vec![mine],
      seen: Vec::new(),
      unseen: 3,
      marching: None,
      offer: Some(OfferView {
        watcher: 3,
        mover: 0,
        mover_at: (2, 0),
      }),
      panel: Panel::default(),
    };
    feed(&socket, vec![WatchOp::YouAre { side: 1 }, WatchOp::Snapshot(Box::new(view))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert_eq!(client.my_offer().map(|o| o.watcher), Some(3));
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
