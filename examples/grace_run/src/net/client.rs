//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! This one carries the outbox: every acting op is sequenced, kept until the
//! server's snapshot acks it, and **re-sent in full after a resume**. That
//! resend is the at-least-once half; the server's per-seat sequence line is
//! the at-most-once half; together the delve is exactly-once across a drop.
//! The severed states are self-inflicted on purpose: the panel's buttons cut
//! the link to make the machinery visible.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{PlayerId, RunOp, RunView, PROTOCOL};

const WIRE: MsgPackCodec = MsgPackCodec;
const LOG_KEEP: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  /// The link is deliberately down; resuming when the clock says so.
  Severed { resume_in_ms: u64 },
  Gone(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Moment {
  DoorOpened,
  KeyBurned,
  SeatHeld(PlayerId),
  SeatResumed(PlayerId),
  SeatExpired(PlayerId),
  RunComplete(u32),
}

enum Link {
  Up(FramePump<MsgPackCodec>),
  Severed { until_ms: u64 },
}

pub struct NetClient {
  link: Link,
  base_url: String,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub view: Option<RunView>,
  pub log: VecDeque<String>,
  pub moments: Vec<Moment>,

  /// The next sequence to stamp, and everything stamped but not yet acked.
  seq: u64,
  outbox: Vec<RunOp>,
  /// Ops re-sent over the current link, for the panel.
  pub resent: u64,

  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    let pump = FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?;
    Ok(Self {
      link: Link::Up(pump),
      base_url: url.to_owned(),
      status: Status::Connecting,
      me: None,
      view: None,
      log: VecDeque::new(),
      moments: Vec::new(),
      seq: 0,
      outbox: Vec::new(),
      resent: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    })
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    match &self.link {
      Link::Up(pump) => pump.rtt_ms(),
      Link::Severed { .. } => None,
    }
  }

  pub fn outstanding(&self) -> usize {
    self.outbox.len()
  }

  /// Stamps, records, and sends an acting op. The closure builds the op
  /// around the sequence so the outbox holds exactly what went out.
  pub fn act(&mut self, build: impl Fn(u64) -> RunOp) {
    self.seq += 1;
    let op = build(self.seq);
    self.outbox.push(op.clone());
    if let Link::Up(pump) = &mut self.link {
      pump.send_op(&op);
    }
  }

  /// A dial change: not sequenced, not resent, lost with the link like any
  /// fire-and-forget setting.
  pub fn set(&mut self, op: RunOp) {
    if let Link::Up(pump) = &mut self.link {
      pump.send_op(&op);
    }
  }

  /// Cuts the link on purpose. The server sees an ordinary drop and holds the
  /// seat; this side resumes after `resume_in_ms` with the same identity and
  /// re-sends its outbox.
  pub fn sever(&mut self, resume_in_ms: u64) {
    if matches!(self.link, Link::Severed { .. }) {
      return;
    }
    self.link = Link::Severed {
      until_ms: self.now_ms + resume_in_ms,
    };
    self.status = Status::Severed { resume_in_ms };
    self.note(format!("link cut; resuming in {:.0}s", resume_in_ms as f32 / 1000.0));
  }

  fn note(&mut self, line: String) {
    self.log.push_front(line);
    self.log.truncate(LOG_KEEP);
  }

  pub fn poll(&mut self, now_ms: u64) {
    self.now_ms = now_ms;

    if let Link::Severed { until_ms } = self.link {
      if now_ms < until_ms {
        self.status = Status::Severed {
          resume_in_ms: until_ms - now_ms,
        };
        return;
      }
      let url = match self.me {
        Some(me) => format!("{}?p={me}", self.base_url),
        None => self.base_url.clone(),
      };
      match FramePump::connect(&url, WIRE, PROTOCOL) {
        Ok(mut pump) => {
          // The at-least-once half: everything unacked goes again, and the
          // server's sequence line decides what actually happens twice.
          for op in &self.outbox {
            pump.send_op(op);
            self.resent += 1;
          }
          let outstanding = self.outbox.len();
          self.link = Link::Up(pump);
          self.status = Status::Joined;
          self.note(format!("resumed; re-sent {outstanding} unacked op(s)"));
        }
        Err(e) => {
          self.status = Status::Gone(format!("could not resume: {e}"));
          return;
        }
      }
    }

    let Link::Up(pump) = &mut self.link else { return };
    let mut events = std::mem::take(&mut self.events);
    pump.drain(now_ms, &mut events);
    let mut arrivals = std::mem::take(&mut self.arrivals);
    pump.digest(&mut events, now_ms, &mut arrivals);
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
        Arrival::Closed(reason) => {
          if !matches!(self.status, Status::Severed { .. }) {
            self.status = Status::Gone(reason);
          }
        }
      }
    }
    self.arrivals = arrivals;

    if let Link::Up(pump) = &self.link
      && pump.state() == State::Closed
      && matches!(self.status, Status::Joined | Status::Connecting)
    {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<RunOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        RunOp::Snapshot(view) => {
          // The ack half: the seat's applied mark trims the outbox.
          if let Some(me) = self.me
            && let Some(seat) = view.seats.iter().find(|s| s.player == me)
          {
            let acked = seat.acked_seq;
            self.outbox.retain(|op| op_seq(op).is_none_or(|s| s > acked));
          }
          self.view = Some(*view);
        }
        RunOp::YouAre(id) => {
          self.me = Some(id);
          self.note(format!("you are P{id}"));
        }
        RunOp::DoorOpened { by, room } => {
          self.note(format!("P{by} opened the door of room {room}"));
          self.moments.push(Moment::DoorOpened);
        }
        RunOp::KeyBurned { by } => {
          self.note(format!("P{by}'s key turned in an OPEN door: burned"));
          self.moments.push(Moment::KeyBurned);
        }
        RunOp::SeatHeld { player, ms } => {
          self.note(format!("P{player} dropped; seat held {}s", ms / 1000));
          self.moments.push(Moment::SeatHeld(player));
        }
        RunOp::SeatResumed { player } => {
          self.note(format!("P{player} is back inside the window"));
          self.moments.push(Moment::SeatResumed(player));
        }
        RunOp::SeatExpired { player } => {
          self.note(format!("P{player}'s window closed; the run moves on"));
          self.moments.push(Moment::SeatExpired(player));
        }
        RunOp::RunComplete { coins } => {
          self.note(format!("run complete: {coins} coins"));
          self.moments.push(Moment::RunComplete(coins));
        }
        RunOp::Refused(why) => self.note(format!("refused: {why:?}")),
        _ => {}
      }
    }
  }
}

fn op_seq(op: &RunOp) -> Option<u64> {
  match op {
    RunOp::GrabKey { seq } | RunOp::GrabCoins { seq } | RunOp::Unlock { seq } => Some(*seq),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, ProtocolVersion};
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::protocol::{Meters, Presence, SeatView};

  fn feed(socket: &ScriptedSocket, ops: Vec<RunOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn from_socket(socket: &ScriptedSocket) -> NetClient {
    NetClient {
      link: Link::Up(FramePump::new(Box::new(socket.clone()), WIRE, PROTOCOL)),
      base_url: "ws://test/ws".to_owned(),
      status: Status::Connecting,
      me: None,
      view: None,
      log: VecDeque::new(),
      moments: Vec::new(),
      seq: 0,
      outbox: Vec::new(),
      resent: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    }
  }

  fn view_with_ack(me: PlayerId, acked: u64) -> RunView {
    RunView {
      room: 1,
      rooms: 8,
      door_locked: true,
      chest_keys: 2,
      seats: vec![SeatView {
        player: me,
        presence: Presence::Here,
        keys: 0,
        coins: 0,
        pocketed: false,
        acked_seq: acked,
      }],
      dedup_on: true,
      grace_ms: 10_000,
      meters: Meters::default(),
      runs_completed: 0,
      complete: false,
      intermission_ms_left: None,
    }
  }

  #[test]
  fn the_outbox_holds_until_the_seat_acks() {
    let socket = ScriptedSocket::new();
    let mut client = from_socket(&socket);
    feed(&socket, vec![RunOp::YouAre(1)]);
    client.poll(0);

    client.act(|seq| RunOp::GrabKey { seq });
    client.act(|seq| RunOp::GrabCoins { seq });
    assert_eq!(client.outstanding(), 2);

    feed(&socket, vec![RunOp::Snapshot(Box::new(view_with_ack(1, 1)))]);
    client.poll(10);
    assert_eq!(client.outstanding(), 1, "the ack trimmed the grab, the coins still ride");

    feed(&socket, vec![RunOp::Snapshot(Box::new(view_with_ack(1, 2)))]);
    client.poll(20);
    assert_eq!(client.outstanding(), 0);
  }

  #[test]
  fn a_severed_link_counts_down_rather_than_dying() {
    let socket = ScriptedSocket::new();
    let mut client = from_socket(&socket);
    client.poll(0);
    client.sever(3_000);
    client.poll(1_000);
    assert!(matches!(client.status, Status::Severed { resume_in_ms } if resume_in_ms == 2_000));
  }

  #[test]
  fn a_server_on_another_wire_format_is_reported_rather_than_ignored() {
    let socket = ScriptedSocket::new();
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Hello, &mut bytes);
    WIRE.encode_into(&ProtocolVersion(PROTOCOL.wrapping_add(1)), &mut bytes).unwrap();
    socket.feed_message(bytes);

    let mut client = from_socket(&socket);
    client.poll(0);
    assert!(matches!(client.status, Status::Gone(_)));
  }
}
