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
use crate::controls::Controls;
use crate::pack;
use crate::protocol::{frame_to_ms, Cubes, Encoding, FrameUpdate, PlayerId, YardOp};
use crate::state::YardState;

type Ctx = OpsQueue<YardOp, PlayerId>;

#[derive(Default)]
pub struct YardLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
  controls: Option<std::sync::Arc<parking_lot::Mutex<crate::controls::Controls>>>,
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

  /// Dials the host can turn while the yard runs. Shared memory rather than a
  /// wire message, because the host process is also the server.
  pub fn with_controls(mut self, controls: std::sync::Arc<parking_lot::Mutex<crate::controls::Controls>>) -> Self {
    self.controls = Some(controls);
    self
  }
}

/// Applies whatever the host has dialled, before the tick that will be sent
/// under it.
///
/// A live stream cannot simply be told the new encoding. Entering delta has to
/// start from nothing confirmed, and a client that joined under full width has
/// no stream at all, so one is built for it here rather than only on join.
pub fn retune(state: &mut YardState, wanted: Controls) {
  state.snap = wanted.snap;
  state.send_hz = wanted.send_hz.clamp(1, crate::protocol::TICK_HZ);
  if state.encoding == wanted.encoding {
    return;
  }
  state.encoding = wanted.encoding;

  let cubes = state.yard.len();
  let budgeted = matches!(wanted.encoding, Encoding::Budgeted | Encoding::Delta);
  let deltas = wanted.encoding == Encoding::Delta;
  if !budgeted {
    return;
  }
  let players: Vec<PlayerId> = state.agents.keys().copied().collect();
  for player in players {
    match state.streams.get_mut(&player) {
      Some(stream) => stream.retune(deltas, cubes),
      None => {
        // Joined under an unbudgeted encoding, so it already holds every cube
        // and needs no seed: a fresh accumulator refreshes them on priority.
        let mut stream = Stream::new(cubes);
        if deltas {
          stream = stream.with_delta(cubes);
        }
        state.streams.insert(player, stream);
      }
    }
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

    if let Some(controls) = &self.controls {
      let wanted = *controls.lock();
      retune(state, wanted);
    }

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

/// Whether this tick is one the wire carries.
fn on_air(state: &YardState) -> bool {
  let every = (crate::protocol::TICK_HZ / state.send_hz).max(1);
  state.tick.is_multiple_of(every)
}

fn step_once(state: &mut YardState, ctx: &mut Ctx) {
  state.tick += 1;
  let driving = state.driving;
  state.yard.step(&driving);
  if state.snap {
    state.yard.snap_to_wire();
  }

  if !on_air(state) {
    return;
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

  /// Drives the real logic through a dial change and decodes every frame the
  /// way a client does, which is the only way to catch a baseline the two ends
  /// stop sharing: a delta measured from a value the client never received
  /// decodes somewhere else and raises nothing.
  ///
  /// Scored over **the cubes each frame actually names**, never the whole yard.
  /// Under a budget most cubes are waiting their turn, and comparing those
  /// against truth measures staleness, which is the scheme working. A decode
  /// fault looks like a cube arriving and landing somewhere the server has
  /// never put it.
  async fn dial_and_decode(from: Encoding, to: Encoding) -> f32 {
    let mut state = YardState::at_rate(from, false, crate::protocol::TICK_HZ);
    let logic = YardLogic::new();
    let mut client: Vec<crate::protocol::CubeState> = Vec::new();
    let mut baseline: Vec<Option<crate::pack::Quantized>> = Vec::new();

    fn drain(
      ops: Vec<TargetedOp<YardOp, PlayerId>>,
      client: &mut Vec<crate::protocol::CubeState>,
      baseline: &mut Vec<Option<crate::pack::Quantized>>,
    ) -> Vec<usize> {
      let mut named = Vec::new();
      for targeted in ops {
        for op in targeted.ops {
          let YardOp::Frame(update) = op else { continue };
          let mut apply = |index: usize, cube: crate::protocol::CubeState, client: &mut Vec<_>| {
            if index >= client.len() {
              client.resize(index + 1, cube);
            }
            client[index] = cube;
            named.push(index);
          };
          match update.cubes {
            Cubes::Full(cubes) => {
              for (index, cube) in cubes.into_iter().enumerate() {
                apply(index, cube, client);
              }
            }
            Cubes::Packed(bytes) => {
              for (index, cube) in pack::unpack(bytes.as_ref()).unwrap().into_iter().enumerate() {
                apply(index, cube, client);
              }
            }
            Cubes::Subset(bytes) => {
              for (index, cube) in pack::unpack_subset(bytes.as_ref()).unwrap() {
                apply(index as usize, cube, client);
              }
            }
            Cubes::Delta(bytes) => {
              for (index, cube) in pack::unpack_delta(bytes.as_ref(), baseline).unwrap() {
                apply(index as usize, cube, client);
              }
            }
          }
        }
      }
      named
    }

    let joined = logic
      .process_input(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(7),
      })
      .await
      .unwrap();
    drain(joined.ops, &mut client, &mut baseline);

    let moved = logic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(7),
        ops: vec![YardOp::Drive(Drive { dx: -1, dz: 0, jump: false, rolling: true })],
      })
      .await
      .unwrap();
    drain(moved.ops, &mut client, &mut baseline);

    for _ in 0..120 {
      let ops = logic.process_input(&mut state, step()).await.unwrap();
      drain(ops.ops, &mut client, &mut baseline);
    }

    // The dial moves, and nothing else changes.
    retune(&mut state, Controls::new(to, false, crate::protocol::TICK_HZ));

    let (mut worst, mut checked) = (0.0f32, 0usize);
    for _ in 0..240 {
      let ops = logic.process_input(&mut state, step()).await.unwrap();
      let named = drain(ops.ops, &mut client, &mut baseline);

      let mut truth = Vec::new();
      state.yard.snapshot(&mut truth);
      for index in named {
        let (want, held) = (&truth[index], &client[index]);
        worst = worst.max(
          ((want.pos[0] - held.pos[0]).powi(2) + (want.pos[1] - held.pos[1]).powi(2) + (want.pos[2] - held.pos[2]).powi(2))
            .sqrt(),
        );
        checked += 1;
      }
    }
    assert!(checked > 1000, "the run has to actually carry cubes: {checked}");
    worst
  }

  fn step() -> LogicInput<YardOp, PlayerId> {
    LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    }
  }

  /// A cube a frame names should land within the wire's own rounding. Anything
  /// larger is the two ends measuring from different baselines.
  const DECODE_TOLERANCE: f32 = 0.02;

  #[tokio::test]
  async fn the_dial_can_move_to_delta_without_desyncing_the_baseline() {
    // Entering delta with a baseline carried over from before the switch means
    // measuring against values the client was never sent under this encoding.
    for from in [Encoding::Full, Encoding::Packed, Encoding::Budgeted] {
      let worst = dial_and_decode(from, Encoding::Delta).await;
      assert!(worst < DECODE_TOLERANCE, "{from:?} -> Delta decoded {worst} out");
    }
  }

  #[tokio::test]
  async fn the_dial_can_move_off_delta_and_back() {
    for to in [Encoding::Full, Encoding::Packed, Encoding::Budgeted] {
      let worst = dial_and_decode(Encoding::Delta, to).await;
      assert!(worst < DECODE_TOLERANCE, "Delta -> {to:?} decoded {worst} out");
    }
  }

  #[tokio::test]
  async fn a_client_seated_before_the_dial_moved_gets_a_stream() {
    let mut state = YardState::at_rate(Encoding::Full, false, crate::protocol::TICK_HZ);
    let logic = YardLogic::new();
    logic
      .process_input(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(7),
      })
      .await
      .unwrap();
    assert!(state.streams.is_empty(), "full width needs no stream");

    retune(&mut state, Controls::new(Encoding::Delta, false, crate::protocol::TICK_HZ));
    let stream = state.streams.get(&7).expect("the dial builds one for a seated client");
    assert!(stream.deltas());
  }

  #[tokio::test]
  async fn entering_delta_confirms_nothing(){
    let mut stream = Stream::new(8).with_delta(8);
    stream.baseline[3] = Some(crate::pack::quantize_cube(&crate::protocol::CubeState {
      pos: [1.0, 2.0, 3.0],
      rot: [0.0, 0.0, 0.0, 1.0],
      linvel: [0.0; 3],
      at_rest: true,
    }));
    stream.retune(true, 8);
    assert!(stream.baseline.iter().all(|b| b.is_none()), "nothing survives the switch");
    assert!(stream.deltas());

    stream.retune(false, 8);
    assert!(!stream.deltas(), "and leaving delta drops the baseline entirely");
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
      ops: vec![YardOp::Drive(Drive { dx: 1, dz: 0, jump: false, rolling: false })],
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
    state.driving[0] = Drive { dx: 1, dz: 1, jump: false, rolling: false };
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;

    assert_eq!(state.driving[0], Drive::default());
    let ops = tick(&mut state).await;
    assert!(!ops.is_empty(), "the yard does not stop when a driver leaves");
  }
}
