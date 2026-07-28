//! The authoritative arena, as `plaza` core wants it.
//!
//! The same shape as `bomb_grid`'s, which is the point: the netcode wrapper is
//! boilerplate once the simulation is shaped for it, and the only genuinely new
//! thing this arena sends is [`Op::TurnTaken`], the place a turn happened.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::server::Server;
use crate::sim::types::{Cell, Controls, Maze, PlayerId, PlayerState, MATCH_ROUNDS};

pub type PlayerKey = u64;

const IMPAIR_SEED: u64 = 0x1A2E_0B0B;

/// Everything the omniscient half of a host needs.
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub maze: Maze,
  pub players: Vec<PlayerState>,
  pub pellets: Vec<Cell>,
  pub round: u32,
  pub server_now_ms: u64,

  pub turns_taken: u64,
  pub turns_expired: u64,
  pub catches: u64,
  pub pellets_eaten: u64,
  /// Pursuers eaten by an energized runner. The counter that says whether the
  /// role inversion is ever actually reached, as opposed to merely implemented.
  pub devoured: u64,
  pub match_round: u32,
  pub match_rounds: u32,
  /// Per seat: `(taken, expired)`. The buffer's two failure modes, which say
  /// opposite things and must not be averaged together.
  pub turn_stats: Vec<(u64, u64)>,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
  pub seats_taken: usize,
  pub seats: usize,
}

type Downlink = LatencyLink<Op>;

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  acked: HashMap<PlayerKey, u64>,
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
      acked: HashMap::new(),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    ServerPolicy {
      sync_hz: self.controls.sync_hz,
      playout_delay_ms: self.controls.playout_delay_ms,
      render_delay_ms: self.controls.render_delay_ms,
      // Told, never assumed. The buffer decides whether a turn is taken or
      // forgotten, so a client guessing differently would predict a turn the
      // server dropped and then run down a corridor the server never entered.
      turn_buffer_ms: self.controls.turn_buffer_ms,
      input_max_late_ticks: self.controls.input_max_late_ticks,
      input_max_early_ticks: self.controls.input_max_early_ticks,
      players: self.sim.seats(),
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
    self.acked.remove(key);
    self.down.remove(key);
  }

  fn host_view(&self) -> HostView {
    HostView {
      maze: self.sim.maze.clone(),
      players: self.sim.players.clone(),
      pellets: self.sim.pellets.clone(),
      round: self.sim.round(),
      server_now_ms: self.sim.now_ms(),
      turns_taken: self.sim.turns_taken,
      turns_expired: self.sim.turns_expired,
      catches: self.sim.catches,
      pellets_eaten: self.sim.pellets_eaten,
      devoured: self.sim.devoured,
      match_round: self.sim.match_round(),
      match_rounds: MATCH_ROUNDS,
      turn_stats: self.sim.turn_stats(),
      input_verdicts: self.sim.input_verdicts(),
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
            let policy = state.policy();
            let round = state.sim.round_start();
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
              key,
              vec![Op::Welcome {
                player: seat as PlayerId,
                policy,
                round: Box::new(round),
              }],
            )]))
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
          let droppable = matches!(op, Op::Turn { .. });
          if droppable && controls.loss_pct > 0.0 && state.rng.unit() * 100.0 < controls.loss_pct {
            continue;
          }
          match op {
            Op::Turn { seq, dir, tick } => {
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, dir, &controls);
            }
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              replies.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            Op::Hello { protocol } if protocol != PROTOCOL => {
              tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
              replies.push(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]));
            }
            Op::Hello { .. } => {}
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

        let mut outbound: Vec<Op> = Vec::new();
        if let Some(round) = out.round_start {
          outbound.push(Op::Round(Box::new(round)));
        }
        if let Some((runner, by, next_in_ms)) = out.caught {
          outbound.push(Op::Caught { runner, by, next_in_ms });
        }
        // Before the frame: a turn describes a tick the frame is about to
        // summarise, and a client that learned the position first would compare
        // against a turn it had not been told about.
        for taken in out.turns {
          outbound.push(Op::TurnTaken(Box::new(taken)));
        }
        for (by, cells) in out.eaten {
          outbound.push(Op::Eaten { by, cells });
        }
        for (by, cell, kind, until_ms) in out.powers {
          outbound.push(Op::PowerTaken { by, cell, kind, until_ms });
        }
        for (runner, pursuer) in out.devoured {
          outbound.push(Op::Devoured { runner, pursuer });
        }
        if let Some((standings, next_in_ms)) = out.match_over {
          outbound.push(Op::MatchOver { standings, next_in_ms });
        }

        let controls = state.controls;
        // Seat by seat, because the **frame is per recipient**: a hidden runner
        // is absent from everybody else's copy, which is the only way to keep
        // the secret. Everything else is the same message for all of them.
        let seated: Vec<(usize, PlayerKey)> = state.seats.by_seat().iter().map(|(seat, key)| (*seat, *key)).collect();
        for (seat, key) in seated {
          let mine = out.frames.iter().find(|(id, _)| *id as usize == seat).map(|(_, f)| f.clone());
          let link = state.down.entry(key).or_default();
          for op in &outbound {
            link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
          if let Some(frame) = mine {
            link.send(now, Op::Frame(Box::new(frame)), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
        }

        let mut targeted = Vec::new();
        for (key, link) in state.down.iter_mut() {
          for op in link.drain_due(now) {
            targeted.push(TargetedOp::new_system_to(*key, vec![op]));
          }
        }
        for (key, seq) in &state.acked {
          targeted.push(TargetedOp::new_system_to(*key, vec![Op::InputAck { seq: *seq }]));
        }

        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(targeted))
      }
    }
  }
}

/// State reaches a joiner as `Op::Welcome`, which carries the maze, the players
/// and the pellets together.
pub struct NoSnapshots;

#[async_trait]
impl plaza::snapshot::SnapshotProvider<PlayerKey, Arena, Op> for NoSnapshots {
  async fn create_snapshot(
    &self,
    _full_state: &Arena,
    _target_agent: Option<&Agent<PlayerKey>>,
    _context: Option<plaza::snapshot::SnapshotContext>,
  ) -> Result<Option<Op>, plaza::snapshot::SnapshotError<PlayerKey>> {
    Ok(None)
  }
}

#[allow(dead_code)]
fn _target_is_used(_: MessageTarget<PlayerKey>) {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{MAZE_SEED, SIM_STEP_MS};
  use std::time::Duration;

  fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(logic.process_input(state, input)).unwrap()
  }

  fn slots(controls: Controls) -> (Arc<Mutex<Controls>>, Arc<Mutex<HostView>>) {
    (Arc::new(Mutex::new(controls)), Arc::new(Mutex::new(HostView::default())))
  }

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      sync_hz: 60,
      bots: false,
      players: 2,
      ..Controls::default()
    }
  }

  fn count(out: &LogicOutput<Op, PlayerKey>, want: fn(&Op) -> bool) -> usize {
    out.ops.iter().filter(|t| t.ops.iter().any(want)).count()
  }

  #[test]
  fn a_joiner_is_seated_welcomed_with_a_maze_and_then_fed_frames() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, MAZE_SEED);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let round = joined
      .ops
      .iter()
      .find_map(|t| {
        t.ops.iter().find_map(|op| match op {
          Op::Welcome { round, .. } => Some(round.clone()),
          _ => None,
        })
      })
      .expect("a joiner is welcomed");
    assert!(!round.maze.tiles.is_empty(), "the welcome carries the maze");
    assert!(!round.pellets.is_empty(), "and the pellets");

    let mut frames = 0;
    for _ in 0..10 {
      frames += count(
        &step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }),
        |op| matches!(op, Op::Frame(_)),
      );
    }
    assert!(frames > 0);
    assert_eq!(view.lock().seats_taken, 1);
  }

  #[test]
  fn a_turn_reaches_the_simulation_and_its_place_is_broadcast() {
    // The op nothing else in the repository sends: where a turn happened.
    let controls = Controls { input_playout: false, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, MAZE_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let seat = state.seat_of(&1u64).expect("seated");
    let at = state.sim.players[seat].cell;
    let heading = state.sim.players[seat].heading;
    let want = state.sim.maze.exits(at).into_iter().find(|d| *d != heading).expect("a turn");

    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Turn { seq: 1, dir: want, tick: 0 }],
    });

    let mut reported = 0;
    for _ in 0..200 {
      reported += count(
        &step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }),
        |op| matches!(op, Op::TurnTaken(_)),
      );
      if reported > 0 {
        break;
      }
    }
    assert!(reported > 0, "the place was broadcast");
  }

  #[test]
  fn a_full_arena_says_so_rather_than_going_silent() {
    let controls = Controls { players: 1, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, MAZE_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let second = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });
    assert_eq!(count(&second, |op| matches!(op, Op::NoSeat { .. })), 1);
  }

  #[test]
  fn a_client_on_another_wire_format_is_told_to_reload() {
    let controls = quiet();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, MAZE_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    let out = step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Hello { protocol: PROTOCOL.wrapping_add(1) }],
    });
    assert_eq!(count(&out, |op| matches!(op, Op::Outdated { .. })), 1);
  }

  #[test]
  fn a_departing_player_hands_the_seat_back() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, MAZE_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 1);

    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1u64 });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 0);
  }

  #[test]
  fn the_policy_tells_a_client_the_turn_buffer() {
    // A client that guessed this would predict turns the server had already
    // forgotten, which is a wrong junction manufactured out of a mismatched
    // constant rather than out of the network.
    let controls = Controls { turn_buffer_ms: 321, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let state = Arena::new(controls, MAZE_SEED);
    let _ = logic;
    assert_eq!(state.policy().turn_buffer_ms, 321);
  }
}
