//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! Thin on purpose: the server resnapshots after every change and the view
//! carries the commanding army's options, so this holds a [`BattleView`] and a
//! short log, sends orders, and decodes what comes back. The game rules it
//! ships are the ones `protocol.rs` compiles into every build.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{Army, BattleOp, BattlePhase, BattleView, PlayerId, UnitOrders, PROTOCOL};

/// One codec for the whole client, matching the one the host is built with.
const WIRE: MsgPackCodec = MsgPackCodec;

const LOG_KEEP: usize = 9;

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  /// Connected; the first snapshot says whether a seat was free.
  Joined,
  Gone(String),
}

/// A thing that just happened, for the window to animate. The log is the
/// history; a moment is drained once and spent on effects.
#[derive(Clone, Debug, PartialEq)]
pub enum Moment {
  Phase {
    phase: BattlePhase,
    /// How long the phase says it will last, when it says.
    ends_in_ms: Option<u64>,
  },
  Round {
    number: u32,
  },
  Struck {
    target: u8,
    /// Where the target stood as the blow landed, resolved before the next
    /// snapshot moves or removes it.
    at: Option<crate::protocol::Cell>,
    hp_left: i8,
    felled: bool,
    counter: bool,
  },
  Healed {
    target: u8,
    at: Option<crate::protocol::Cell>,
    mended: i8,
  },
  Over {
    winner: Army,
  },
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub view: Option<BattleView>,
  /// Newest first.
  pub log: VecDeque<String>,
  /// Drained by the window every frame; see [`Moment`].
  pub moments: Vec<Moment>,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
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
      me: None,
      view: None,
      log: VecDeque::new(),
      moments: Vec::new(),
      events: Vec::new(),
      arrivals: Vec::new(),
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// The army this player commands, or `None` for a spectator.
  pub fn my_army(&self) -> Option<Army> {
    let me = self.me?;
    let view = self.view.as_ref()?;
    view.commanders.iter().find(|(p, _)| *p == me).map(|(_, a)| *a)
  }

  /// Whether it is this player's phase to command.
  pub fn commanding(&self) -> bool {
    match (&self.view, self.my_army()) {
      (Some(view), Some(army)) => view.phase == BattlePhase::Command(army),
      _ => false,
    }
  }

  /// The options the server computed for one of my units, this phase.
  pub fn orders_for(&self, unit: u8) -> Option<&UnitOrders> {
    self.view.as_ref()?.orders.iter().find(|o| o.unit == unit)
  }

  pub fn send(&mut self, op: &BattleOp) {
    self.pump.send_op(op);
  }

  fn note(&mut self, line: String) {
    self.log.push_front(line);
    self.log.truncate(LOG_KEEP);
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64) {
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
    let Ok(ops) = WIRE.decode::<Vec<BattleOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        BattleOp::Snapshot(view) => self.view = Some(*view),
        BattleOp::YouAre(id) => {
          self.me = Some(id);
          self.note(format!("you are P{id}"));
        }
        BattleOp::Struck {
          unit,
          target,
          hp_left,
          felled,
          counter,
        } => {
          let verb = if counter { "answers" } else { "strikes" };
          let fate = if felled { "felled".to_owned() } else { format!("{hp_left} hp left") };
          self.note(format!("unit {unit} {verb} {target}: {fate}"));
          let at = self
            .view
            .as_ref()
            .and_then(|v| v.units.iter().find(|u| u.id == target))
            .map(|u| u.at);
          self.moments.push(Moment::Struck {
            target,
            at,
            hp_left,
            felled,
            counter,
          });
        }
        BattleOp::Healed { unit, target, hp_now } => {
          self.note(format!("unit {unit} mends {target}: {hp_now} hp"));
          let mended = self
            .view
            .as_ref()
            .and_then(|v| v.units.iter().find(|u| u.id == target))
            .map(|u| hp_now - u.hp)
            .unwrap_or(0);
          let at = self
            .view
            .as_ref()
            .and_then(|v| v.units.iter().find(|u| u.id == target))
            .map(|u| u.at);
          self.moments.push(Moment::Healed { target, at, mended });
        }
        BattleOp::Refused(why) => self.note(format!("refused: {why:?}")),
        BattleOp::BattleOver { winner } => {
          self.note(format!("{winner:?} takes the field"));
          self.moments.push(Moment::Over { winner });
        }
        BattleOp::PhaseChanged(phase) => {
          if let Some(reason) = phase.reason.clone() {
            self.note(reason);
          }
          self.moments.push(Moment::Phase {
            phase: phase.new_phase,
            ends_in_ms: phase.duration_hint.map(|d| d.as_millis() as u64),
          });
        }
        BattleOp::RoundStarted(round) => self.moments.push(Moment::Round {
          number: round.round_number,
        }),
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
  use crate::protocol::Terrain;

  fn feed(socket: &ScriptedSocket, ops: Vec<BattleOp>) {
    // Built through the same codec the client decodes with, so the test cannot
    // pass while the two ends disagree about the format.
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn view() -> BattleView {
    BattleView {
      phase: BattlePhase::Command(Army::Blue),
      round: 1,
      games: 1,
      map: crate::protocol::MapSize::Small,
      mustered: Vec::new(),
      host: None,
      map_choice: None,
      muster_close_in_ms: None,
      terrain: vec![vec![Terrain::Plain]],
      units: Vec::new(),
      fallen: Vec::new(),
      commanders: vec![(1, Army::Blue), (2, Army::Red)],
      orders: Vec::new(),
      winner: None,
      wins: Vec::new(),
    }
  }

  #[test]
  fn a_seat_and_a_snapshot_make_a_commander() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![BattleOp::YouAre(1), BattleOp::Snapshot(Box::new(view()))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert_eq!(client.me, Some(1));
    assert_eq!(client.my_army(), Some(Army::Blue));
    assert!(client.commanding());
  }

  #[test]
  fn an_unseated_arrival_is_a_spectator() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![BattleOp::Snapshot(Box::new(view()))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert_eq!(client.my_army(), None);
    assert!(!client.commanding());
  }

  #[test]
  fn a_refusal_lands_in_the_log_rather_than_vanishing() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![BattleOp::Refused(crate::protocol::Refusal::OutOfReach)]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert!(client.log.iter().any(|l| l.contains("OutOfReach")));
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
