//! The authoritative arena, as `plaza` core wants it.
//!
//! The thinnest of the playground arenas, and that is the finding rather than a
//! shortcut. There is no `TimeStep` work at all beyond a clock: nothing is
//! being simulated here, because a time trial has nothing to arbitrate between
//! players. The arena exists to hold the leaderboard and to **replay evidence
//! on demand**.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::log::Rejection;
use crate::sim::protocol::{Ghost, Op, PROTOCOL};
use crate::sim::server::Server;
use crate::sim::types::Controls;

pub type PlayerKey = u64;

/// Everything the omniscient half of a host needs.
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub board: Vec<Ghost>,
  pub submissions: u64,
  pub accepted: u64,
  pub refused: u64,
  pub last_refusal: Option<Rejection>,
  pub ticks_replayed: u64,
  pub bytes_in: u64,
  pub bytes_out: u64,
  pub bytes_if_paths: u64,
  pub seats_taken: usize,
  pub seats: usize,
  pub server_now_ms: u64,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count),
      controls,
      seats: SeatTable::new(count),
    }
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
  }

  fn host_view(&self) -> HostView {
    HostView {
      board: self.sim.board.clone(),
      submissions: self.sim.submissions,
      accepted: self.sim.accepted,
      refused: self.sim.refused,
      last_refusal: self.sim.last_refusal,
      ticks_replayed: self.sim.ticks_replayed,
      bytes_in: self.sim.bytes_in,
      bytes_out: self.sim.bytes_out,
      bytes_if_paths: self.sim.bytes_if_paths,
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
      server_now_ms: self.sim.now_ms(),
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
          Some(seat) => Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![state.sim.welcome(seat)])])),
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
        let mut out = Vec::new();
        for op in ops {
          match op {
            Op::Submit { log, claimed_ms } => {
              for answer in state.sim.submit(seat, *log, claimed_ms) {
                match answer {
                  // A verified run is everybody's: it is a ghost to race. A
                  // refusal belongs to the one client that sent the log.
                  accepted @ Op::Accepted { .. } => {
                    let seated: Vec<PlayerKey> = state.seats.by_seat().iter().map(|(_, k)| *k).collect();
                    for target in seated {
                      out.push(TargetedOp::new_system_to(target, vec![accepted.clone()]));
                    }
                  }
                  refused => out.push(TargetedOp::new_system_to(key, vec![refused])),
                }
              }
            }
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              out.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            Op::Hello { protocol } if protocol != PROTOCOL => {
              tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
              out.push(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]));
            }
            _ => {}
          }
        }
        Ok(LogicOutput::ops(out))
      }

      LogicInput::TimeStep { delta_time } => {
        let live = *self.controls.lock();
        state.controls = Controls {
          players: state.controls.players,
          ..live
        };
        // The only thing a tick does here. Nothing is being simulated: the runs
        // happen on the machines driving them and arrive as finished evidence.
        state.sim.advance(delta_time.as_millis() as u64);
        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::none())
      }
    }
  }
}

/// State reaches a joiner as `Op::Welcome`, which carries the track and every
/// ghost worth racing.
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
  use crate::sim::log::{InputLog, Recorder};
  use crate::sim::rules;
  use crate::sim::types::*;
  use crate::sim::world::autopilot;
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

  fn a_run(track: &Track, version: u32) -> (InputLog, u64) {
    let mut racer = Racer::at_start(track);
    let mut recorder = Recorder::new(version);
    let mut finished = 0;
    for tick in 0..crate::sim::log::MAX_TICKS {
      let input = autopilot(&racer, track, tick);
      recorder.observe(input);
      rules::step(&mut racer, input, track);
      if rules::finished(&racer) {
        finished = tick;
        break;
      }
    }
    (recorder.finish(), (finished as u64 + 1) * SIM_STEP_MS)
  }

  #[test]
  fn a_joiner_is_given_the_track_and_every_ghost() {
    let logic = logic();
    let mut state = Arena::new(quiet());
    let out = step(
      &logic,
      &mut state,
      LogicInput::AgentJoined {
        agent: Agent::new_human(1u64),
      },
    );
    let ops: Vec<Op> = out.ops.into_iter().flat_map(|t| t.ops).collect();
    assert!(matches!(ops.as_slice(), [Op::Welcome { .. }]));
  }

  #[test]
  fn an_accepted_run_goes_to_every_seat_and_a_refusal_only_to_the_sender() {
    // A ghost is for racing, so everybody needs it. A refusal is a private
    // conversation with the client that sent the log.
    let logic = logic();
    let mut state = Arena::new(quiet());
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });

    let (log, time) = a_run(&state.sim.track, state.sim.rules_version);
    let out = step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: Agent::new_human(1u64),
        ops: vec![Op::Submit {
          log: Box::new(log.clone()),
          claimed_ms: time,
        }],
      },
    );
    assert_eq!(out.ops.len(), 2, "both seats were sent the ghost");

    let out = step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: Agent::new_human(1u64),
        ops: vec![Op::Submit {
          log: Box::new(log),
          claimed_ms: 1,
        }],
      },
    );
    assert_eq!(out.ops.len(), 1, "and only the sender was told about the lie");
    assert!(out.ops[0].ops.iter().any(|op| matches!(op, Op::Refused { .. })));
  }

  #[test]
  fn a_tick_simulates_nothing() {
    // Worth asserting rather than assuming: this arena holds a leaderboard and
    // replays evidence, and if a tick ever starts producing ops then something
    // has grown a simulation that does not belong here.
    let logic = logic();
    let mut state = Arena::new(quiet());
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    for _ in 0..50 {
      let out = step(
        &logic,
        &mut state,
        LogicInput::TimeStep {
          delta_time: Duration::from_millis(SIM_STEP_MS),
        },
      );
      assert!(out.ops.is_empty());
    }
    assert!(state.sim.now_ms() > 0, "but the clock moved");
  }
}
