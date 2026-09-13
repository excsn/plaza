//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! The live piece beyond decoding is the [`OrderMirror`]: every op feeds it,
//! it derives who must act next, and each turn the server opens is checked
//! against that derivation. The panel shows how many turns diverged.

use std::collections::VecDeque;

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::mirror::{Observed, OrderMirror};
use crate::protocol::{BattleView, GaugeOp, Move, UnitId, PROTOCOL};

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
  TurnOpened(UnitId),
  Acted { unit: UnitId, mv: Move, crit: bool },
  Fell(UnitId),
  BattleStarted,
  BattleEnded { victor: u8, series: [u32; 2], series_over: bool },
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub my_team: Option<u8>,
  pub view: Option<BattleView>,
  pub mirror: OrderMirror,
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
      my_team: None,
      view: None,
      mirror: OrderMirror::new(),
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

  /// Whether the unit whose turn it is answers to this client.
  pub fn my_turn(&self) -> bool {
    match (&self.view, self.my_team) {
      (Some(view), Some(team)) => view
        .current
        .and_then(|id| view.units.iter().find(|u| u.id == id))
        .is_some_and(|u| u.team == team),
      _ => false,
    }
  }

  pub fn act(&mut self, unit: UnitId, mv: Move) {
    self.pump.send_op(&GaugeOp::Act { unit, mv });
  }

  pub fn set_regime(&mut self, regime: crate::protocol::Regime) {
    self.pump.send_op(&GaugeOp::SetRegime(regime));
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
    let Ok(ops) = WIRE.decode::<Vec<GaugeOp>>(body) else {
      return;
    };
    for op in ops {
      // The mirror sees everything first; the audit is what the panel prints.
      match self.mirror.observe(&op) {
        Observed::Agreed(unit) => self.moments.push(Moment::TurnOpened(unit)),
        Observed::Diverged { server, mine } => {
          self.note(format!("PROJECTION MISSED: server {server:?}, mine {mine:?}"));
          if let Some(unit) = server {
            self.moments.push(Moment::TurnOpened(unit));
          }
        }
        Observed::Noted => {}
      }

      match op {
        GaugeOp::Snapshot(view) => {
          self.pump.timeline_mut().note_stamp(view.server_now_ms, self.now_ms);
          self.view = Some(*view);
        }
        GaugeOp::YouCommand { team } => {
          self.my_team = Some(team);
          self.note(format!("you command {}", ["blue", "red"][team as usize % 2]));
        }
        GaugeOp::BattleStarted { battle, regime, .. } => {
          self.note(format!("battle {battle} under {regime:?}"));
          self.moments.push(Moment::BattleStarted);
        }
        GaugeOp::ActionDone {
          unit,
          mv,
          crit,
          ref effects,
        } => {
          self.moments.push(Moment::Acted { unit, mv, crit });
          for effect in effects {
            if !effect.alive {
              self.note(format!("unit {} falls", effect.unit));
              self.moments.push(Moment::Fell(effect.unit));
            }
          }
        }
        GaugeOp::BattleEnded {
          victor,
          series,
          series_over,
        } => {
          let side = ["blue", "red"][victor as usize % 2];
          self.note(if series_over {
            format!("{side} takes the series {}-{}", series[0], series[1])
          } else {
            format!("{side} takes the battle, {}-{}", series[0], series[1])
          });
          self.moments.push(Moment::BattleEnded {
            victor,
            series,
            series_over,
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
  use crate::order;
  use crate::protocol::{BattlePhase, Panel, Regime};

  fn feed(socket: &ScriptedSocket, ops: Vec<GaugeOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  #[test]
  fn a_battle_start_arms_the_mirror_and_a_turn_is_audited() {
    let socket = ScriptedSocket::new();
    let units = order::fresh_units();
    let first = order::ctb_next(&units).unwrap();
    feed(&socket, vec![
      GaugeOp::YouCommand { team: 0 },
      GaugeOp::BattleStarted {
        battle: 7,
        regime: Regime::Ctb,
        units,
      },
      GaugeOp::TurnChanged(plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload {
        new_turn_actor: Some(first),
        previous_turn_actor: None,
        turn_number: 1,
        time_limit_for_turn: None,
      }),
    ]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert_eq!(client.my_team, Some(0));
    assert_eq!(client.mirror.checked, 1);
    assert_eq!(client.mirror.diverged, 0);
    assert!(client.moments.contains(&Moment::TurnOpened(first)));
  }

  #[test]
  fn a_snapshot_baselines_a_joiner_mid_battle() {
    let socket = ScriptedSocket::new();
    let mut units = order::fresh_units();
    for unit in units.iter_mut() {
      unit.next_at += 3;
    }
    let view = BattleView {
      phase: BattlePhase::Fighting,
      regime: Regime::Ctb,
      server_now_ms: 4_000,
      battle: 3,
      round: 0,
      turn: 9,
      series: [1, 0],
      commanders: [1, 2],
      seats: vec![1, 2],
      units: units.clone(),
      current: order::ctb_next(&units),
      order: Vec::new(),
      panel: Panel::default(),
    };
    feed(&socket, vec![GaugeOp::Snapshot(Box::new(view))]);
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);

    assert_eq!(client.mirror.expected(), order::ctb_next(&units), "derivation resumes from the baseline");
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
