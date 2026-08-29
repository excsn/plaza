//! A client on the real wire, shared by the desktop window and the wasm page.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{CastSpell, DuelOp, DuelView, Spell, PROTOCOL};

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
  Put(CastSpell),
  Resolved(CastSpell),
  Fizzled(CastSpell),
  PriorityTo(u8),
  TurnStarted { active: u8 },
  DuelStarted,
  DuelOver { winner: u8 },
  Refused(String),
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub my_seat: Option<u8>,
  pub view: Option<DuelView>,
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

  /// Whether this client holds the window right now.
  pub fn my_window(&self) -> bool {
    match (&self.view, self.my_seat) {
      (Some(view), Some(seat)) => view.priority == seat,
      _ => false,
    }
  }

  pub fn cast(&mut self, spell: Spell) {
    self.pump.send_op(&DuelOp::Cast { spell });
  }

  pub fn pass(&mut self) {
    self.pump.send_op(&DuelOp::Pass);
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
    let Ok(ops) = WIRE.decode::<Vec<DuelOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        DuelOp::Snapshot(view) => {
          self.pump.timeline_mut().note_stamp(view.server_now_ms, self.now_ms);
          self.view = Some(*view);
        }
        DuelOp::YouAre { seat } => {
          self.my_seat = Some(seat);
          self.note(format!("you duel as {}", ["blue", "red"][seat as usize % 2]));
        }
        DuelOp::Put { cast, depth } => {
          self.note(format!(
            "{} speaks {:?} (stack {depth})",
            ["blue", "red"][cast.caster as usize % 2],
            cast.spell
          ));
          self.moments.push(Moment::Put(cast));
        }
        DuelOp::Resolved { cast, .. } => {
          self.note(format!("{:?} resolves", cast.spell));
          self.moments.push(Moment::Resolved(cast));
        }
        DuelOp::Fizzled { cast } => {
          self.note(format!("{:?} is countered", cast.spell));
          self.moments.push(Moment::Fizzled(cast));
        }
        DuelOp::PriorityTo { seat } => self.moments.push(Moment::PriorityTo(seat)),
        DuelOp::TurnStarted { active, .. } => self.moments.push(Moment::TurnStarted { active }),
        DuelOp::DuelStarted { duel } => {
          self.note(format!("duel {duel}"));
          self.moments.push(Moment::DuelStarted);
        }
        DuelOp::DuelOver { winner } => {
          self.note(format!("{} has the last word", ["blue", "red"][winner as usize % 2]));
          self.moments.push(Moment::DuelOver { winner });
        }
        DuelOp::Refused { reason } => {
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
