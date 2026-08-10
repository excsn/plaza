//! The rink's authority. One job per tick: gather what each seat holds (the
//! schedule for a human, the chase for a bot), step the shared simulation
//! once, and broadcast the world **with the inputs that made it**. That echo
//! is the whole contract with the clients' rollback sessions: the server is
//! the input orderer, the step is the same code, and the digest proves it.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure, SeatState};
use tracing::{debug, info};

use crate::protocol::{frame_to_ms, FrameUpdate, PlayerId, RinkOp};
use crate::sim::{self, PaddleInput, SEATS};
use crate::state::{RinkState, WINDOW};

type Ctx = OpsQueue<RinkOp, PlayerId>;

/// Ticks a bot holds its input before re-running the chase, staggered by
/// seat: a human reaction time, not a 60Hz one. Re-deciding every tick also
/// chatters between -1, 0 and 1, and every flip is a guaranteed misprediction
/// for every client's repeat-last predictor; a held decision is what makes
/// repeat-last mostly right.
pub const BOT_DECIDE_EVERY: u64 = 12;
/// Ticks at the tail of each hold a bot coasts with no input, so its
/// sustained speed sits below a human's held key.
pub const BOT_REST_TICKS: u64 = 4;

/// What a bot seat applies at `tick`, deciding and resting on the cadence
/// above. `held` is the seat's decision slot between re-decisions.
pub fn bot_held(tick: u64, seat: usize, world: &sim::World, held: &mut PaddleInput) -> PaddleInput {
  let phase = (tick + seat as u64) % BOT_DECIDE_EVERY;
  if phase == 0 {
    *held = sim::bot_chase(world, seat);
  }
  if phase >= BOT_DECIDE_EVERY - BOT_REST_TICKS {
    PaddleInput::default()
  } else {
    *held
  }
}

/// What a joining client is handed before its first frame, for a backend whose
/// frames are not complete baselines. `None` is the fixed-point answer and the
/// reason the rink shipped without a snapshot provider at all: its world goes
/// out inside every frame.
///
/// Wrap it in [`plaza::SnapshotFn`] to make it a provider.
pub fn baseline(state: &RinkState, _target: Option<&Agent<PlayerId>>) -> Option<RinkOp> {
  match state.body.baseline()? {
    Ok(bytes) => Some(RinkOp::Baseline {
      frame: state.tick,
      physics: state.body.physics(),
      state: bytes,
    }),
    Err(e) => {
      tracing::error!(error = %e, "could not serialise the simulation for a joining client");
      None
    }
  }
}

#[derive(Default)]
pub struct RinkLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl std::fmt::Debug for RinkLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("RinkLogic")
  }
}

impl RinkLogic {
  pub fn new() -> Self {
    Self::default()
  }

  /// A slot the logic writes the simulation clock into, so the session's
  /// pongs carry sim time and every client aims its input frames at it.
  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }
}

#[async_trait]
impl StateLogic<RinkOp, PlayerId, RinkState> for RinkLogic {
  async fn process_input(
    &self,
    state: &mut RinkState,
    input: LogicInput<RinkOp, PlayerId>,
  ) -> Result<LogicOutput<RinkOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentJoined { agent } => {
        seat_player(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        depart(state, agent_id);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          if let RinkOp::Input { frame, input } = op {
            submit(state, player, frame, input);
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

fn seat_player(state: &mut RinkState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  if state.agents.contains_key(&player) {
    return;
  }
  state.agents.insert(player, agent.clone());

  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    info!(player, "rink is full of people; watching");
    return;
  };
  state.schedules[seat].clear();
  state.held[seat] = PaddleInput::default();
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![RinkOp::Seated { seat: seat as u8 }]));
  info!(player, seat, "took a paddle from the bot");
}

fn depart(state: &mut RinkState, player: PlayerId) {
  state.agents.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    state.schedules[seat].clear();
    state.held[seat] = PaddleInput::default();
    info!(player, seat, "left; the bot takes the paddle back");
  }
}

fn submit(state: &mut RinkState, player: PlayerId, frame: u64, input: PaddleInput) {
  let Some(seat) = state.seat_of(player) else {
    return;
  };
  let verdict = state.schedules[seat].submit(frame, input, state.tick, WINDOW);
  if !verdict.accepted() {
    debug!(player, frame, current = state.tick, ?verdict, "input dropped");
  }
}

fn step_once(state: &mut RinkState, ctx: &mut Ctx) {
  state.tick += 1;

  let before = state.world();
  let mut applied = [PaddleInput::default(); SEATS];
  for seat in 0..SEATS {
    applied[seat] = match state.roster.seat_state(seat) {
      SeatState::Human(_) => {
        if let Some(input) = state.schedules[seat].execute_due(state.tick) {
          state.held[seat] = input;
        }
        state.held[seat]
      }
      _ => bot_held(state.tick, seat, &before, &mut state.held[seat]),
    };
  }

  state.body.step(&applied);

  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![RinkOp::Frame(Box::new(FrameUpdate {
      frame: state.tick,
      server_time_ms: frame_to_ms(state.tick),
      world: state.world(),
      applied,
      digest: state.body.digest(),
      occupants: state.occupants(),
      physics: state.body.physics(),
    }))]));
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Occupant;
  use crate::sim::PADDLE_R;

  async fn run(state: &mut RinkState, input: LogicInput<RinkOp, PlayerId>) {
    RinkLogic::new().process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut RinkState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await;
  }

  async fn join(state: &mut RinkState, player: PlayerId) {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(player),
    })
    .await;
  }

  async fn send(state: &mut RinkState, player: PlayerId, frame: u64, dx: i8, dy: i8) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(player),
      ops: vec![RinkOp::Input {
        frame,
        input: PaddleInput { dx, dy },
      }],
    })
    .await;
  }

  #[tokio::test]
  async fn a_human_takes_a_bot_seat_and_a_leaver_hands_it_back() {
    let mut state = RinkState::new();
    join(&mut state, 7).await;
    assert_eq!(state.occupants()[0], Occupant::Human(7));
    assert_eq!(state.seat_of(7), Some(0));

    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;
    assert_eq!(state.occupants()[0], Occupant::Bot);
  }

  #[tokio::test]
  async fn the_rink_never_waits_for_people() {
    let mut state = RinkState::new();
    let before = state.world().paddles;
    for _ in 0..120 {
      tick(&mut state).await;
    }
    assert_ne!(state.world().paddles, before, "four bots are already skating");
  }

  #[tokio::test]
  async fn an_input_executes_on_the_frame_it_named() {
    let mut state = RinkState::new();
    join(&mut state, 7).await;
    let x0 = state.world().paddles[0].x;

    send(&mut state, 7, 3, 1, 0).await;
    tick(&mut state).await;
    tick(&mut state).await;
    assert_eq!(state.world().paddles[0].x, x0, "not yet: the input names frame 3");

    tick(&mut state).await;
    assert!(state.world().paddles[0].x > x0, "and on frame 3 it runs");

    // A level repeats until replaced.
    tick(&mut state).await;
    let moved_twice = plaza_client_utils::fixed::Fx::from_int(2 * 3);
    assert_eq!(state.world().paddles[0].x - x0, moved_twice);
  }

  #[tokio::test]
  async fn an_input_naming_a_closed_tick_is_dropped() {
    let mut state = RinkState::new();
    join(&mut state, 7).await;
    for _ in 0..20 {
      tick(&mut state).await;
    }
    let x0 = state.world().paddles[0].x;
    send(&mut state, 7, 2, 1, 0).await;
    tick(&mut state).await;
    assert_eq!(state.world().paddles[0].x, x0, "frame 2 is history and stays history");
    assert!(state.schedules[0].rejected() > 0);
  }

  #[tokio::test]
  async fn a_fifth_human_watches() {
    let mut state = RinkState::new();
    for player in 1..=5 {
      join(&mut state, player).await;
    }
    assert_eq!(state.seat_of(5), None);
    assert!(state.occupants().iter().all(|s| matches!(s, Occupant::Human(_))));

    // A spectator's input goes nowhere.
    let before = state.world().paddles;
    send(&mut state, 5, 1, 1, 1).await;
    tick(&mut state).await;
    let _ = before;
  }

  #[tokio::test]
  async fn paddles_stay_inside_their_half_under_bot_play() {
    let mut state = RinkState::new();
    for _ in 0..600 {
      tick(&mut state).await;
    }
    let mid = crate::sim::RINK_W / 2;
    assert!(state.world().paddles[0].x.to_int() <= mid - PADDLE_R);
    assert!(state.world().paddles[3].x.to_int() >= mid + PADDLE_R);
  }
}
