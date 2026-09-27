//! A client on the real wire, shared by the desktop window and the wasm page.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::cards::{rank_name, suit_name};
use crate::protocol::{Act, Card, PokerOp, Seat, TableView, PROTOCOL};

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
  ToAct(Seat),
  Acted { seat: Seat, act: Act, allin: bool },
  Street,
  Awarded { seat: Seat, chips: u32 },
  Showdown,
  HandStarted,
  Refused(String),
}

pub fn card_name(card: Card) -> String {
  format!("{}{}", rank_name(card), suit_name(card))
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub my_seat: Option<Seat>,
  pub view: Option<TableView>,
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
      my_seat: None,
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

  /// Whether the ask is this client's.
  pub fn my_ask(&self) -> bool {
    match (&self.view, self.my_seat) {
      (Some(view), Some(seat)) => view.to_act == Some(seat),
      _ => false,
    }
  }

  pub fn take(&mut self, act: Act) {
    self.pump.send_op(&PokerOp::TakeAction { act });
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
        Arrival::Closed(closed) => self.status = Status::Gone(closed.reason),
      }
    }
    self.arrivals = arrivals;

    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<PokerOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        PokerOp::Snapshot(view) => {
          self.pump.timeline_mut().note_stamp(view.server_now_ms, self.now_ms);
          self.view = Some(*view);
        }
        PokerOp::YouAre { seat } => {
          self.my_seat = Some(seat);
          self.note(format!("you have seat {seat}"));
        }
        PokerOp::HandStarted { hand, button } => {
          self.note(format!("hand {hand}, button on seat {button}"));
          self.moments.push(Moment::HandStarted);
        }
        PokerOp::Holes { cards } => {
          self.note(format!("you hold {} {}", card_name(cards[0]), card_name(cards[1])));
        }
        PokerOp::StreetStarted { street, .. } => {
          self.note(format!("{street:?}"));
          self.moments.push(Moment::Street);
        }
        PokerOp::ActionTaken { seat, act, paid, allin } => {
          let what = match act {
            Act::Fold => "folds".to_owned(),
            Act::Call if paid == 0 => "checks".to_owned(),
            Act::Call => format!("calls {paid}"),
            Act::Raise => format!("raises, {paid} in"),
          };
          self.note(format!("seat {seat} {what}{}", if allin { ", all in" } else { "" }));
          self.moments.push(Moment::Acted { seat, act, allin });
        }
        PokerOp::ToAct { seat, .. } => self.moments.push(Moment::ToAct(seat)),
        PokerOp::Showdown { .. } => self.moments.push(Moment::Showdown),
        PokerOp::PotAwarded { seat, chips } => {
          self.note(format!("seat {seat} takes {chips}"));
          self.moments.push(Moment::Awarded { seat, chips });
        }
        PokerOp::Refused { reason } => {
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
