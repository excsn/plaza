//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! The one live piece beyond decoding: the **claim**. A press is stamped with
//! the client's estimate of server time, from the pump's timeline over pongs
//! and every stamped op, then named as a tick and an offset inside it. The
//! server floors the claim; this side only aims it.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{DrawOp, DuelPhase, DuelView, PlayerId, Verdict, PROTOCOL, TICK_US};

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
  Steady,
  Signal,
  Ruled(Verdict),
  Phase { phase: DuelPhase, ends_in_ms: Option<u64> },
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub view: Option<DuelView>,
  pub log: VecDeque<String>,
  pub moments: Vec<Moment>,
  /// Ticks the local clock estimate has been fed with, for the panel.
  pub stamps_seen: u64,

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
      me: None,
      view: None,
      log: VecDeque::new(),
      moments: Vec::new(),
      stamps_seen: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// This client's estimate of server time now, in ms; what a press aims with.
  pub fn server_time_ms(&self) -> u64 {
    self.pump.server_time_ms(self.now_ms)
  }

  pub fn dueling(&self) -> bool {
    match (&self.view, self.me) {
      (Some(view), Some(me)) => view.duelists.contains(&me),
      _ => false,
    }
  }

  /// The trigger. Stamped now, on the local estimate of the server clock;
  /// legal in Steady too, because a false start is the server's to rule on,
  /// not this side's to hide.
  pub fn fire(&mut self) {
    let at_us = self.server_time_ms() * 1000;
    let op = DrawOp::Fire {
      tick: at_us / TICK_US,
      offset_us: (at_us % TICK_US) as u32,
    };
    self.pump.send_op(&op);
  }

  pub fn set_controls(&mut self, controls: crate::protocol::Controls) {
    self.pump.send_op(&DrawOp::SetControls(controls));
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
    let Ok(ops) = WIRE.decode::<Vec<DrawOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        DrawOp::Snapshot(view) => {
          self.pump.timeline_mut().note_stamp(view.server_now_ms, self.now_ms);
          self.stamps_seen += 1;
          self.view = Some(*view);
        }
        DrawOp::YouAre(id) => {
          self.me = Some(id);
          self.note(format!("you are P{id}"));
        }
        DrawOp::Steady { contest } => {
          self.note(format!("contest {contest}: steady..."));
          self.moments.push(Moment::Steady);
        }
        DrawOp::Signal { at_ms, .. } => {
          self.pump.timeline_mut().note_stamp(at_ms, self.now_ms);
          self.stamps_seen += 1;
          self.moments.push(Moment::Signal);
        }
        DrawOp::Ruled(verdict) => {
          let line = match (verdict.ruling, verdict.winner_subtick) {
            (crate::protocol::Ruling::FalseStart, Some(w)) => format!("false start: P{w} takes it"),
            (_, Some(w)) => format!("P{w} takes contest {}", verdict.contest),
            (_, None) => "nobody drew".to_owned(),
          };
          self.note(line);
          if verdict.disagreed {
            self.note("the two orderings disagreed here".to_owned());
          }
          self.moments.push(Moment::Ruled(*verdict));
        }
        DrawOp::PhaseChanged(phase) => {
          self.moments.push(Moment::Phase {
            phase: phase.new_phase,
            ends_in_ms: phase.duration_hint.map(|d| d.as_millis() as u64),
          });
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
  use crate::protocol::{Controls, HarnessStats, Ruling};

  fn feed(socket: &ScriptedSocket, ops: Vec<DrawOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn view() -> DuelView {
    DuelView {
      phase: DuelPhase::Steady,
      server_now_ms: 5_000,
      contest: 1,
      duelists: vec![1, 2],
      seats: vec![1, 2],
      wins: Vec::new(),
      controls: Controls::default(),
      last: None,
      live_disagreed: 0,
      live_contests: 0,
      harness: HarnessStats::default(),
    }
  }

  #[test]
  fn a_snapshot_seats_the_duelist_and_feeds_the_clock() {
    let socket = ScriptedSocket::new();
    feed(&socket, vec![DrawOp::YouAre(1), DrawOp::Snapshot(Box::new(view()))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert!(client.dueling());
    assert!(client.server_time_ms() >= 5_000, "the stamp floors the estimate");
  }

  #[test]
  fn a_verdict_is_a_moment_and_a_line() {
    let socket = ScriptedSocket::new();
    let verdict = Verdict {
      contest: 3,
      ruling: Ruling::CleanDraw,
      shots: Vec::new(),
      winner_subtick: Some(2),
      winner_arrival: Some(1),
      same_tick: true,
      disagreed: true,
    };
    feed(&socket, vec![DrawOp::Ruled(Box::new(verdict))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert!(client.log.iter().any(|l| l.contains("P2 takes contest 3")));
    assert!(client.log.iter().any(|l| l.contains("disagreed")));
    assert!(matches!(client.moments.as_slice(), [Moment::Ruled(v)] if v.contest == 3));
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
