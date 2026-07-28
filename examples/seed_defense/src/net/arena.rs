//! The authoritative arena, as `plaza` core wants it.
//!
//! Structurally the same wrapper as `bomb_grid`'s and `pellet_maze`'s, and
//! deliberately so: once a simulation is shaped for this, the netcode layer is
//! boilerplate. What is different here is how little goes through it. There is
//! no frame. The regular outbound traffic is one digest every half second, and
//! everything else is an event that happened because somebody did something.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::rules::Field;
use crate::sim::server::{Phase, Server};
use crate::sim::types::Controls;

pub type PlayerKey = u64;

const IMPAIR_SEED: u64 = 0x5EED_0B0B;

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

type Downlink = LatencyLink<Op>;

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  down: HashMap<PlayerKey, Downlink>,
  rng: Rng,
}

impl Arena {
  pub fn new(controls: Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count, seed),
      controls,
      seats: SeatTable::new(count),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
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
    self.down.remove(key);
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

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self { controls, view }
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
            let mut ops = vec![state.sim.welcome(seat, &controls)];
            // A joiner during a prep phase never heard the wave announcement,
            // and the field in its welcome does not hold the wave yet, because
            // the wave has not been laid out. Without this it would sit out the
            // whole wave agreeing with nobody.
            if let Some(op) = state.sim.pending_wave_op() {
              ops.push(op);
            }
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, ops)]))
          }
          None => Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![Op::NoSeat { seats: state.sim.seats() }])])),
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
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let mut replies = Vec::new();
        let controls = state.controls;
        for op in ops {
          match op {
            Op::Want { seq, cell, kind, upgrade } => {
              // A build request is dropped by the impairment like anything
              // else. The player sees nothing happen and asks again, which is
              // the honest behaviour: there is no state to reconcile, only a
              // cause that did or did not occur.
              if controls.loss_pct > 0.0 && state.rng.unit() * 100.0 < controls.loss_pct {
                continue;
              }
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
                      let link = state.down.entry(target).or_default();
                      link.send(now, built.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
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
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              replies.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            Op::Hello { protocol } if protocol != PROTOCOL => {
              tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
              replies.push(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]));
            }
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        let live = *self.controls.lock();
        state.controls = Controls {
          players: state.controls.players,
          ..live
        };

        let out = state.sim.advance(delta_time.as_millis() as u64, &state.controls);
        let now = state.sim.now_ms();
        let controls = state.controls;
        let seated: Vec<PlayerKey> = state.seats.by_seat().iter().map(|(_, k)| *k).collect();
        state.sim.charge_wire(&out.ops, seated.len());

        for target in seated {
          let link = state.down.entry(target).or_default();
          for op in &out.ops {
            link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
        }

        let mut targeted = Vec::new();
        for (key, link) in state.down.iter_mut() {
          for op in link.drain_due(now) {
            targeted.push(TargetedOp::new_system_to(*key, vec![op]));
          }
        }

        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(targeted))
      }
    }
  }
}

/// State reaches a joiner as `Op::Welcome`, which carries the seed, the policy
/// and the field together.
pub struct NoSnapshots;

#[async_trait]
impl plaza::snapshot::SnapshotProvider<PlayerKey, Arena, Op> for NoSnapshots {
  async fn create_snapshot(
    &self,
    _full_state: &Arena,
    _target_agent: Option<&plaza::Agent<PlayerKey>>,
    _context: Option<plaza::snapshot::SnapshotContext>,
  ) -> Result<Option<Op>, plaza::snapshot::SnapshotError<PlayerKey>> {
    Ok(None)
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
    // The whole of what a client needs to reproduce the world: one seed, and
    // the announcement of a wave that has been scheduled but not laid out.
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
    // A `Built` is a cause, not a reply. A client that never heard it would
    // simulate a world with one fewer tower for the rest of the wave.
    let logic = logic();
    let mut state = Arena::new(quiet(), 1);
    joined(&logic, &mut state, 1);
    joined(&logic, &mut state, 2);

    step(
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
    let out = step(
      &logic,
      &mut state,
      LogicInput::TimeStep {
        delta_time: Duration::from_millis(SIM_STEP_MS),
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
    // Ten seconds of arena, with the wave running, counted by kind. The
    // example's claim as an assertion over what the netcode layer emits.
    let logic = logic();
    let mut state = Arena::new(quiet(), 0xC0FFEE);
    joined(&logic, &mut state, 1);

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
