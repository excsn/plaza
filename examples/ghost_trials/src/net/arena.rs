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
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::log::Rejection;
use crate::sim::protocol::{Ghost, Op, PROTOCOL};
use crate::sim::server::Server;
use crate::sim::types::Controls;

pub type PlayerKey = u64;

const IMPAIR_SEED: u64 = 0x6057_0B0B;

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
  pub lost_submissions: u64,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  /// The impairment, on the real path.
  ///
  /// It does not touch the lap, which is the example's whole claim. It decides
  /// when a ghost turns up and when a verdict lands.
  down: std::collections::HashMap<PlayerKey, LatencyLink<Op>>,
  rng: Rng,
  /// Submissions the link ate, so a dropped run is a number rather than a
  /// mystery.
  pub lost_submissions: u64,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count),
      controls,
      seats: SeatTable::new(count),
      down: std::collections::HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
      lost_submissions: 0,
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
    self.down.remove(key);
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
      lost_submissions: self.lost_submissions,
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
              // A lost submission is a lap nobody recorded. There is no retry,
              // deliberately: it costs the run and never the board, because the
              // board only ever holds runs that were verified.
              let controls = state.controls;
              if controls.loss_pct > 0.0 && state.rng.unit() * 100.0 < controls.loss_pct {
                state.lost_submissions += 1;
                continue;
              }
              let now = state.sim.now_ms();
              for answer in state.sim.submit(seat, *log, claimed_ms) {
                // A verified run is everybody's: it is a ghost to race. A
                // refusal belongs to the one client that sent the log. Both go
                // down the impaired link, so the panel's sliders act on the
                // real path rather than on a readout.
                let targets: Vec<PlayerKey> = match answer {
                  Op::Accepted { .. } => state.seats.by_seat().iter().map(|(_, k)| *k).collect(),
                  _ => vec![key],
                };
                for target in targets {
                  let link = state.down.entry(target).or_default();
                  link.send(now, answer.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
                }
              }
            }
            Op::Ping { origin_ms } => {
              // Through the link like everything else, so the round trip the
              // panel shows is the round trip the sliders describe.
              let server_ms = state.sim.now_ms();
              let controls = state.controls;
              let link = state.down.entry(key).or_default();
              link.send(
                server_ms,
                Op::Pong { origin_ms, server_ms },
                controls.latency_ms,
                controls.jitter_ms,
                controls.loss_pct,
                &mut state.rng,
              );
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
        // A tick moves the clock and delivers whatever the link is holding.
        // Nothing is simulated: the runs happen on the machines driving them
        // and arrive as finished evidence.
        state.sim.advance(delta_time.as_millis() as u64);
        let now = state.sim.now_ms();
        let mut out = Vec::new();
        for (key, link) in state.down.iter_mut() {
          for op in link.drain_due(now) {
            out.push(TargetedOp::new_system_to(*key, vec![op]));
          }
        }
        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(out))
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
    let mut world = rules::World::trial(track);
    let mut recorder = Recorder::new(version, Mode::Trial, track.size, 1);
    let mut finished = 0;
    for tick in 0..crate::sim::log::MAX_TICKS {
      let input = autopilot(&world.racers[0], track, world.tick, 0);
      recorder.observe(input);
      let inputs = rules::field_inputs(&world, track, input, 0);
      rules::step_world(&mut world, &inputs, track);
      if rules::finished(&world.racers[0]) {
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
    // conversation with the client that sent the log. Both come back through
    // the link, so they arrive on a later tick rather than in the same breath.
    let logic = logic();
    let mut state = Arena::new(quiet());
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });

    let (log, time) = a_run(&Track::circuit(), state.sim.rules_version);
    submit(&logic, &mut state, 1, log.clone(), time);
    let told = drain(&logic, &mut state, |op| matches!(op, Op::Accepted { .. }));
    assert_eq!(told, 2, "both seats were sent the ghost");

    submit(&logic, &mut state, 1, log, 1);
    let told = drain(&logic, &mut state, |op| matches!(op, Op::Refused { .. }));
    assert_eq!(told, 1, "and only the sender was told about the lie");
  }

  fn submit(logic: &ArenaLogic, state: &mut Arena, key: PlayerKey, log: InputLog, claimed_ms: u64) {
    step(
      logic,
      state,
      LogicInput::AgentOps {
        source: Agent::new_human(key),
        ops: vec![Op::Submit {
          log: Box::new(log),
          claimed_ms,
        }],
      },
    );
  }

  /// Runs the clock until the link has handed everything over, and counts the
  /// deliveries matching a shape.
  fn drain(logic: &ArenaLogic, state: &mut Arena, want: fn(&Op) -> bool) -> usize {
    let mut seen = 0;
    for _ in 0..40 {
      let out = step(
        logic,
        state,
        LogicInput::TimeStep {
          delta_time: Duration::from_millis(SIM_STEP_MS),
        },
      );
      seen += out.ops.iter().filter(|t| t.ops.iter().any(want)).count();
    }
    seen
  }

  #[test]
  fn the_impairment_is_on_the_real_path() {
    let controls = Controls {
      latency_ms: 200,
      jitter_ms: 0,
      ..quiet()
    };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });

    let (log, time) = a_run(&Track::circuit(), state.sim.rules_version);
    let immediate = step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: Agent::new_human(1u64),
        ops: vec![Op::Submit {
          log: Box::new(log),
          claimed_ms: time,
        }],
      },
    );
    assert!(immediate.ops.is_empty(), "nothing comes back in the same breath");

    let mut delivered_at = None;
    for tick in 0..40 {
      let out = step(
        &logic,
        &mut state,
        LogicInput::TimeStep {
          delta_time: Duration::from_millis(SIM_STEP_MS),
        },
      );
      if out.ops.iter().any(|t| t.ops.iter().any(|op| matches!(op, Op::Accepted { .. }))) {
        delivered_at = Some((tick + 1) * SIM_STEP_MS);
        break;
      }
    }
    let at = delivered_at.expect("the verdict arrived eventually");
    assert!(at >= 200, "and it waited for the link: {at} ms");
  }

  #[test]
  fn a_tick_simulates_nothing() {
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
      // With nothing in flight a tick produces nothing. If this ever starts
      // failing, something has grown a simulation that does not belong here.
      assert!(out.ops.is_empty());
    }
    assert!(state.sim.now_ms() > 0, "but the clock moved");
  }
}
