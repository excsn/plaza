//! The authoritative arena, as `plaza` core wants it.
//!
//! Structurally the same wrapper as `bomb_grid`'s and `pellet_maze`'s, and
//! deliberately so: once a simulation is shaped for this, the netcode layer is
//! boilerplate. What is different here is how little goes through it. There is
//! no frame. The regular outbound traffic is one digest every half second, and
//! everything else is an event that happened because somebody did something.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_server_utils::oneshot::Pending as OneShots;
use plaza_session::{Delivery, DirectionProfile, LinkProfile, LinkPublisher};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::rules::Field;
use crate::sim::server::{Phase, Server};
use crate::sim::types::Controls;

pub type PlayerKey = u64;

/// Everything the omniscient half of a host needs.
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub field: Option<Field>,
  pub server_now_ms: u64,
  pub phase_label: &'static str,
  pub next_wave_in_ms: u64,

  pub builds_admitted: u64,
  pub builds_refused: u64,
  pub snapshots_sent: u64,
  pub digests_sent: u64,
  /// The headline pair: what actually went out, against what the same session
  /// would have cost if the field were streamed at the send rate.
  pub bytes_sent: u64,
  pub bytes_if_streamed: u64,
  pub seats_taken: usize,
  pub seats: usize,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  /// One-shot ops the client has not yet proved it heard.
  pending: OneShots<PlayerKey, Op>,
}

impl Arena {
  pub fn new(controls: Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count, seed),
      controls,
      seats: SeatTable::new(count),
      pending: OneShots::new(),
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    self.sim.policy(&self.controls)
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.seat_of(key)
  }

  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    let seating = self.seats.seat(key);
    if let Seating::Fresh(seat) = seating {
      self.sim.take_seat(seat);
    }
    seating.index()
  }

  fn unseat(&mut self, key: &PlayerKey) {
    if let Some(seat) = self.seats.unseat(key) {
      self.sim.release_seat(seat);
    }
    self.pending.confirm(key);
  }

  fn host_view(&self) -> HostView {
    let (label, next_in) = match self.sim.phase {
      Phase::Prep { until_tick } => (
        "building",
        until_tick.saturating_sub(self.sim.tick()) * crate::sim::types::SIM_STEP_MS,
      ),
      Phase::Running => ("wave", 0),
      Phase::Lost => ("overrun", 0),
    };
    HostView {
      field: Some(self.sim.field.clone()),
      server_now_ms: self.sim.now_ms(),
      phase_label: label,
      next_wave_in_ms: next_in,
      builds_admitted: self.sim.builds_admitted,
      builds_refused: self.sim.builds_refused,
      snapshots_sent: self.sim.snapshots_sent,
      digests_sent: self.sim.digests_sent,
      bytes_sent: self.sim.bytes_sent,
      bytes_if_streamed: self.sim.bytes_if_streamed,
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
    }
  }
}

/// Publishes the panel's impairment sliders to the transport that owns the
/// link. The arena states what the link should be and stops there.
pub use plaza_session::LinkSink;

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkPublisher>,
  /// Where the arena publishes its simulation clock, so the session can stamp
  /// a `Pong` with the clock clients synchronise against.
  clock: Option<Arc<AtomicU64>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      link: None,
      clock: None,
    }
  }

  /// Where the impairment sliders take effect.
  pub fn with_link(mut self, link: LinkSink) -> Self {
    self.link = Some(LinkPublisher::new(link));
    self
  }

  /// Where to publish the simulation clock for the session to read.
  pub fn with_clock(mut self, clock: Arc<AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  /// Pushes the panel's link settings down to the transport when they change.
  fn publish_link(&self, controls: &Controls) {
    let Some(link) = &self.link else { return };
    // One way, applied in each direction, which is what the slider has always
    // meant here.
    let one_way = DirectionProfile {
      delay: Duration::from_millis(controls.latency_ms),
      jitter: Duration::from_millis(controls.jitter_ms),
      loss: controls.loss_pct / 100.0,
      delivery: if controls.datagram_link {
        Delivery::Datagram
      } else {
        Delivery::Reliable
      },
    };
    link.publish(LinkProfile::symmetric(one_way));
  }
}

#[async_trait]
impl StateLogic<Op, PlayerKey, Arena> for ArenaLogic {
  async fn process_input(&self, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> Result<LogicOutput<Op, PlayerKey>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(key) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        match state.seat(key) {
          Some(seat) => {
            let controls = state.controls;
            let now = state.sim.now_ms();
            let welcome = state.sim.welcome(seat, &controls);
            // Declared rather than merely sent: a datagram link can lose it,
            // and nothing else in this protocol would mention the seat again.
            let mut ops = vec![state.pending.declare(key, welcome, now)];
            // A joiner during a prep phase never heard the wave announcement,
            // and the field in its welcome does not hold the wave yet, because
            // the wave has not been laid out. Without this it would sit out the
            // whole wave agreeing with nobody.
            if let Some(op) = state.sim.pending_wave_op() {
              ops.push(op);
            }
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, ops)]))
          }
          None => {
            let now = state.sim.now_ms();
            let op = state.pending.declare(key, Op::NoSeat { seats: state.sim.seats() }, now);
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![op])]))
          }
        }
      }

      LogicInput::AgentLeft { agent_id } => {
        state.unseat(&agent_id);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        // A client that is talking has plainly received whatever let it talk, so
        // this is the acknowledgement and no ack op has to exist. Before the
        // seat gate: a seatless client's traffic confirms its `NoSeat` too, and
        // that verdict is just as unrepeatable as a welcome.
        state.pending.confirm(&key);
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let mut replies = Vec::new();
        let controls = state.controls;
        for op in ops {
          match op {
            Op::Want { seq, cell, kind, upgrade } => {
              // A build request that the link lost never arrives here at all.
              // The player sees nothing happen and asks again, which is the
              // honest behaviour: there is no state to reconcile, only a cause
              // that did or did not occur.
              let answers = state.sim.want_build(seat, seq, cell, kind, upgrade, &controls);
              let now = state.sim.now_ms();
              for answer in answers {
                match answer {
                  // A `Built` is not a reply, it is a cause: every machine has
                  // to apply it, so it goes to every seat through the impaired
                  // link like any other outbound op.
                  built @ Op::Built { .. } => {
                    let seated: Vec<PlayerKey> = state.seats.by_seat().iter().map(|(_, k)| *k).collect();
                    for target in seated {
                      replies.push(TargetedOp::new_system_to(target, vec![built.clone()]));
                    }
                  }
                  reply => replies.push(TargetedOp::new_system_to(key, vec![reply])),
                }
              }
            }
            Op::WantSnapshot { .. } => {
              let snapshot = state.sim.snapshot();
              replies.push(TargetedOp::new_system_to(key, vec![snapshot]));
            }
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        let live = *self.controls.lock();
        self.publish_link(&live);
        if let Some(clock) = &self.clock {
          clock.store(state.sim.now_ms(), Ordering::Relaxed);
        }
        state.controls = Controls {
          players: state.controls.players,
          ..live
        };

        let out = state.sim.advance(delta_time.as_millis() as u64, &state.controls);
        let now = state.sim.now_ms();
        let seated: Vec<PlayerKey> = state.seats.by_seat().iter().map(|(_, k)| *k).collect();
        state.sim.charge_wire(&out.ops, seated.len());

        let mut targeted = Vec::new();
        for target in seated {
          for op in &out.ops {
            targeted.push(TargetedOp::new_system_to(target, vec![op.clone()]));
          }
        }
        targeted.extend(
          state
            .pending
            .due(now, live.datagram_link)
            .into_iter()
            .map(|(key, op)| TargetedOp::new_system_to(key, vec![op])),
        );

        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(targeted))
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{Cell, TowerKind, SIM_STEP_MS};
  use plaza::Agent;
  use std::time::Duration;

  fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
    tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap()
      .block_on(logic.process_input(state, input))
      .unwrap()
  }

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      players: 2,
      ..Controls::default()
    }
  }

  /// The wiring a dead tracker passes silently: `confirm` and `due` present,
  /// `declare` missing, so nothing is ever held and the one-shot goes out once
  /// on a link that can lose it.
  #[test]
  fn a_lost_welcome_is_said_again_only_where_it_could_have_been_lost() {
    for datagram in [true, false] {
      let controls = Controls { datagram_link: datagram, ..quiet() };
      let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
      let mut state = Arena::new(controls, 1);
      joined(&logic, &mut state, 1);

      let mut repeats = 0;
      for _ in 0..80 {
        let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
        repeats += out.ops.iter().filter(|t| t.ops.iter().any(|op| matches!(op, Op::Welcome { .. }))).count();
      }
      if datagram {
        assert!(repeats > 0, "a datagram link can lose it, so it is said again");
      } else {
        assert_eq!(repeats, 0, "a reliable link cannot, so saying it twice is noise");
      }
    }
  }

  /// The other half of the contract, and the half whose absence is silent: a
  /// welcome that is never confirmed is repeated into a client that treats it
  /// as a fresh start, so the first seconds of play rebuild the world over and
  /// over. The guard above only asserts that repeats happen.
  #[test]
  fn traffic_from_a_client_stops_the_repeats() {
    let controls = Controls { datagram_link: true, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, 1);
    joined(&logic, &mut state, 1);
    step(&logic, &mut state, LogicInput::AgentOps {
      source: Agent::new_human(1u64),
      ops: vec![Op::Ack { seq: 1 }],
    });

    let mut repeats = 0;
    for _ in 0..80 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
      repeats += out.ops.iter().filter(|t| t.ops.iter().any(|op| matches!(op, Op::Welcome { .. }))).count();
    }
    assert_eq!(repeats, 0, "confirmed, so nothing is repeated");
  }

  /// What the arena still owns of impairment: turning the panel's numbers into
  /// a link profile, once, and only when they change. Holding the frames back
  /// is the session's, and is tested where that happens.
  #[test]
  fn the_sliders_are_published_to_the_link_rather_than_applied_here() {
    let controls = Controls { latency_ms: 200, jitter_ms: 40, loss_pct: 25.0, ..quiet() };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
      let seen = seen.clone();
      Arc::new(move |profile: LinkProfile| seen.lock().push(profile)) as LinkSink
    };
    let shared = Arc::new(Mutex::new(controls));
    let logic = ArenaLogic::new(shared.clone(), None).with_link(sink);
    let mut state = Arena::new(controls, 1);

    for _ in 0..3 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    }
    assert_eq!(seen.lock().len(), 1, "an unchanged panel says nothing");

    let published = seen.lock()[0];
    assert_eq!(published.up, published.down, "the sliders describe a round trip");
    assert_eq!(published.up.delay, Duration::from_millis(200), "one way, each direction");
    assert_eq!(published.up.loss, 0.25, "the panel reads percent, the link takes a probability");

    shared.lock().latency_ms = 40;
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(seen.lock().len(), 2, "a dragged slider is published once more");
    assert_eq!(seen.lock()[1].up.delay, Duration::from_millis(40));
  }

  fn logic() -> ArenaLogic {
    ArenaLogic::new(Arc::new(Mutex::new(quiet())), None)
  }

  fn joined(logic: &ArenaLogic, state: &mut Arena, key: PlayerKey) -> Vec<Op> {
    step(
      logic,
      state,
      LogicInput::AgentJoined {
        agent: Agent::new_human(key),
      },
    )
    .ops
    .into_iter()
    .flat_map(|t| t.ops)
    .collect()
  }

  #[test]
  fn a_joiner_is_given_the_seed_and_the_wave_it_would_otherwise_have_missed() {
    let logic = logic();
    let mut state = Arena::new(quiet(), 0xC0FFEE);

    let ops = joined(&logic, &mut state, 1);
    assert!(ops.iter().any(|op| matches!(op, Op::Welcome { .. })), "a welcome with the seed");

    step(
      &logic,
      &mut state,
      LogicInput::TimeStep {
        delta_time: Duration::from_millis(SIM_STEP_MS),
      },
    );
    let ops = joined(&logic, &mut state, 2);
    assert!(
      ops.iter().any(|op| matches!(op, Op::Wave { .. })),
      "and the outstanding wave, which the field does not carry yet"
    );
  }

  #[test]
  fn a_build_reaches_every_seat_and_not_only_the_one_that_asked() {
    let logic = logic();
    let mut state = Arena::new(quiet(), 1);
    joined(&logic, &mut state, 1);
    joined(&logic, &mut state, 2);

    // Answered on the op that caused it. Whatever delay the link is configured
    // for has already happened on the way in, so there is nothing left to hold
    // it for.
    let out = step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: Agent::new_human(1u64),
        ops: vec![Op::Want {
          seq: 1,
          cell: Cell::new(4, 5),
          kind: TowerKind::Arrow,
          upgrade: false,
        }],
      },
    );

    let told = out
      .ops
      .iter()
      .filter(|t| t.ops.iter().any(|op| matches!(op, Op::Built { .. })))
      .count();
    assert_eq!(told, 2, "both seats were told about the tower");
  }

  #[test]
  fn the_regular_traffic_is_a_digest_and_nothing_else() {
    let logic = logic();
    let mut state = Arena::new(quiet(), 0xC0FFEE);
    joined(&logic, &mut state, 1);
    // The welcome is confirmed, so the one thing this arena legitimately says
    // twice is out of the way and anything left is regular traffic.
    state.pending.confirm(&1u64);

    let mut digests = 0;
    let mut waves = 0;
    let mut other = 0;
    for _ in 0..(20_000 / SIM_STEP_MS) {
      let out = step(
        &logic,
        &mut state,
        LogicInput::TimeStep {
          delta_time: Duration::from_millis(SIM_STEP_MS),
        },
      );
      for op in out.ops.iter().flat_map(|t| t.ops.iter()) {
        match op {
          Op::Digest { .. } => digests += 1,
          Op::Wave { .. } => waves += 1,
          _ => other += 1,
        }
      }
    }
    assert!(digests > 20 && waves == 1, "{digests} digests, {waves} waves");
    assert_eq!(other, 0, "and nothing else went out at all");
    assert!(state.sim.field.next_enemy > 5, "while a wave came and went");
  }

  #[test]
  fn a_seat_is_released_when_its_client_goes() {
    let logic = logic();
    let mut state = Arena::new(quiet(), 1);
    joined(&logic, &mut state, 1);
    // Whichever seat it got: `SeatTable` does not fill from the front, and a
    // test that assumed it did would be asserting an implementation detail.
    assert!(state.seat_of(&1).is_some());
    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1 });
    assert_eq!(state.seat_of(&1), None);
  }
}
