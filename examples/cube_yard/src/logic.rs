//! The yard's authority: step the solver, broadcast the world.
//!
//! Stage one broadcasts every cube to every client every tick, which is the
//! naive thing and the point: it produces the megabit figure the packing and
//! priority stages are measured against.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure};
use tracing::info;

use crate::budget::{Stream, BUDGET_BITS};
use crate::pack;
use crate::protocol::{frame_to_ms, Cubes, Encoding, FrameUpdate, PlayerId, YardOp};
use crate::state::YardState;

type Ctx = OpsQueue<YardOp, PlayerId>;

#[derive(Default)]
pub struct YardLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl std::fmt::Debug for YardLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("YardLogic")
  }
}

impl YardLogic {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<YardOp, PlayerId, YardState> for YardLogic {
  async fn process_input(
    &self,
    state: &mut YardState,
    input: LogicInput<YardOp, PlayerId>,
  ) -> Result<LogicOutput<YardOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentJoined { agent } => seat_player(state, &agent, &mut ctx),
      LogicInput::AgentLeft { agent_id } => depart(state, agent_id),
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        let Some(seat) = state.seat_of(player) else {
          return Ok(LogicOutput::ops(ctx.into_ops()));
        };
        for op in ops {
          if let YardOp::Drive(drive) = op {
            state.driving[seat] = drive;
          }
        }
      }
      LogicInput::TimeStep { .. } => {
        step_once(state, &mut ctx);
        if let Some(clock) = &self.clock {
          clock.store(frame_to_ms(state.tick), std::sync::atomic::Ordering::Relaxed);
        }
      }
    }

    Ok(LogicOutput::ops(ctx.into_ops()))
  }
}

fn seat_player(state: &mut YardState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  if state.agents.contains_key(&player) {
    return;
  }
  state.agents.insert(player, agent.clone());

  // A budgeted client would take seconds to learn the yard one packet at a
  // time, so it is handed the whole thing once and budgeted from then on.
  if matches!(state.encoding, Encoding::Budgeted | Encoding::Delta) {
    let mut stream = Stream::new(state.yard.len());
    if state.encoding == Encoding::Delta {
      stream = stream.with_delta(state.yard.len());
    }
    let mut cubes = Vec::new();
    state.yard.snapshot(&mut cubes);
    let seed = stream.seed(cubes.len());
    let deltas = stream.deltas();
    let payload = if deltas {
      pack::pack_delta(&cubes, &seed, &mut stream.baseline)
    } else {
      pack::pack_subset(&cubes, &seed)
    };
    state.streams.insert(player, stream);
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: state.tick,
      server_time_ms: frame_to_ms(state.tick),
      yours: None,
      cubes: if deltas { Cubes::Delta(payload.into()) } else { Cubes::Subset(payload.into()) },
    }))]));
  }

  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    info!(player, "the yard is full of drivers; watching");
    return;
  };
  state.driving[seat] = Default::default();
  let cube = state.yard.player_index(seat);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![YardOp::Seated { cube }]));
  info!(player, seat, cube, "took a cube");
}

fn depart(state: &mut YardState, player: PlayerId) {
  state.agents.remove(&player);
  state.streams.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    // A driverless cube stops driving; it keeps its momentum and its place.
    state.driving[seat] = Default::default();
    info!(player, seat, "left the yard");
  }
}

fn step_once(state: &mut YardState, ctx: &mut Ctx) {
  state.tick += 1;
  let driving = state.driving;
  state.yard.step(&driving);
  if state.snap {
    state.yard.snap_to_wire();
  }

  let mut cubes = Vec::new();
  state.yard.snapshot(&mut cubes);

  let whole = match state.encoding {
    Encoding::Full => Some(Cubes::Full(cubes.clone())),
    Encoding::Packed => Some(Cubes::Packed(pack::pack(&cubes).into())),
    // A budget is per link, so there is no one frame to broadcast.
    Encoding::Budgeted | Encoding::Delta => None,
  };

  if let Some(cubes) = whole {
    ctx.ops_q().push(TargetedOp::new_system_all(vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: state.tick,
      server_time_ms: frame_to_ms(state.tick),
      yours: None,
      cubes,
    }))]));
    return;
  }

  // Each client is scored from where it is standing, so the yard around it
  // updates faster than the yard behind it.
  let players: Vec<PlayerId> = state.agents.keys().copied().collect();
  for player in players {
    let viewer = state
      .seat_of(player)
      .map(|seat| cubes[state.yard.player_index(seat) as usize].pos);
    let Some(stream) = state.streams.get_mut(&player) else {
      continue;
    };
    let deltas = stream.deltas();
    let (payload, picked) = if deltas {
      // Packed until the packet is actually full rather than planned against an
      // estimate: a delta cube costs anywhere from eight bits to a full
      // absolute, and no single estimate covers that without wasting the
      // difference.
      let order = stream.rank(&cubes, viewer).to_vec();
      let (payload, sent) = pack::pack_delta_until_full(&cubes, &order, &mut stream.baseline, BUDGET_BITS);
      stream.sent(&sent);
      (payload, sent)
    } else {
      let picked = stream.pick(&cubes, viewer, BUDGET_BITS).to_vec();
      (pack::pack_subset(&cubes, &picked), picked)
    };
    if picked.is_empty() {
      continue;
    }
    ctx.ops_q().push(TargetedOp::new_system_to(player, vec![YardOp::Frame(Box::new(FrameUpdate {
      frame: state.tick,
      server_time_ms: frame_to_ms(state.tick),
      yours: None,
      cubes: if deltas { Cubes::Delta(payload.into()) } else { Cubes::Subset(payload.into()) },
    }))]));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::{Drive, CUBES};
  use crate::sim::MAX_PLAYERS;

  async fn run(state: &mut YardState, input: LogicInput<YardOp, PlayerId>) -> Vec<TargetedOp<YardOp, PlayerId>> {
    let out = YardLogic::new().process_input(state, input).await.unwrap();
    out.ops
  }

  async fn tick(state: &mut YardState) -> Vec<TargetedOp<YardOp, PlayerId>> {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await
  }

  #[tokio::test]
  async fn a_tick_broadcasts_every_cube() {
    let mut state = YardState::new();
    let ops = tick(&mut state).await;
    let YardOp::Frame(update) = &ops[0].ops[0] else {
      panic!("a tick broadcasts a frame");
    };
    let Cubes::Full(cubes) = &update.cubes else {
      panic!("the default encoding is full width");
    };
    assert_eq!(cubes.len(), CUBES + MAX_PLAYERS);
    assert_eq!(update.frame, 1);
  }

  #[tokio::test]
  async fn a_joiner_is_told_which_cube_is_theirs() {
    let mut state = YardState::new();
    let ops = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let YardOp::Seated { cube } = &ops[0].ops[0] else {
      panic!("a joiner is seated");
    };
    assert_eq!(*cube, CUBES as u16);
    assert_eq!(state.seat_of(7), Some(0));
  }

  #[tokio::test]
  async fn a_drive_is_a_level_that_holds_until_replaced() {
    let mut state = YardState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![YardOp::Drive(Drive { dx: 1, dz: 0, jump: false })],
    })
    .await;

    assert_eq!(state.driving[0].dx, 1);
    tick(&mut state).await;
    assert_eq!(state.driving[0].dx, 1, "a level survives a tick that said nothing");
  }

  #[tokio::test]
  async fn a_leaver_stops_driving_and_the_yard_keeps_going() {
    let mut state = YardState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    state.driving[0] = Drive { dx: 1, dz: 1, jump: false };
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;

    assert_eq!(state.driving[0], Drive::default());
    let ops = tick(&mut state).await;
    assert!(!ops.is_empty(), "the yard does not stop when a driver leaves");
  }
}
