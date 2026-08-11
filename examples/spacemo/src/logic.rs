//! The authoritative tick.
//!
//! One structural difference from cube_yard, and it is the example's whole
//! shape: there is no broadcast. Every tick queries relevance once per client
//! and sends each of them a different frame, from the first stage rather than
//! as a later optimisation. In a volume the alternative does not exist, because
//! nobody can hold the world.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure};
use tracing::info;

use crate::pack;
use crate::protocol::{frame_to_ms, FrameUpdate, PlayerId, ShipState, SpaceOp};
use crate::state::SpaceState;

type Ctx = OpsQueue<SpaceOp, PlayerId>;

#[derive(Default)]
pub struct SpaceLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
  controls: Option<std::sync::Arc<parking_lot::Mutex<crate::controls::Controls>>>,
}

impl std::fmt::Debug for SpaceLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("SpaceLogic")
  }
}

impl SpaceLogic {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  pub fn with_controls(mut self, controls: std::sync::Arc<parking_lot::Mutex<crate::controls::Controls>>) -> Self {
    self.controls = Some(controls);
    self
  }
}

#[async_trait]
impl StateLogic<SpaceOp, PlayerId, SpaceState> for SpaceLogic {
  async fn process_input(
    &self,
    state: &mut SpaceState,
    input: LogicInput<SpaceOp, PlayerId>,
  ) -> Result<LogicOutput<SpaceOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    if let Some(controls) = &self.controls {
      let wanted = *controls.lock();
      // Swapping the strategy rebuilds the index on the next tick, and nothing
      // else: the query is the only thing that differs between them.
      state.strategy = wanted.strategy;
      state.packed = wanted.packed;
    }

    match input {
      LogicInput::AgentJoined { agent } => seat_player(state, &agent, &mut ctx),
      LogicInput::AgentLeft { agent_id } => depart(state, agent_id),
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        if let Some(seat) = state.seat_of(player) {
          for op in ops {
            if let SpaceOp::Fly(fly) = op {
              state.apply(seat, fly);
            }
          }
        }
      }
      LogicInput::TimeStep { .. } => step_once(state, &mut ctx),
      _ => {}
    }

    if let Some(clock) = &self.clock {
      clock.store(frame_to_ms(state.tick), std::sync::atomic::Ordering::Relaxed);
    }
    Ok(LogicOutput {
      ops: ctx.into_ops(),
      ..Default::default()
    })
  }
}

fn seat_player(state: &mut SpaceState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  if state.agents.contains_key(&player) {
    return;
  }
  state.agents.insert(player, agent.clone());

  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    info!(player, "no seats left; watching");
    return;
  };
  state.space.spawn(seat);
  state.flying[seat] = Default::default();
  ctx.ops_q().push(TargetedOp::new_system_to(player, vec![SpaceOp::Seated { seat: seat as u16 }]));
  info!(player, seat, "joined");
}

fn depart(state: &mut SpaceState, player: PlayerId) {
  state.agents.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    // The ship goes with the pilot. Unlike cube_yard's driverless cube, there
    // is nothing here for an abandoned hull to rest on.
    state.space.remove(seat);
    state.flying[seat] = Default::default();
    info!(player, seat, "left");
  }
}

fn step_once(state: &mut SpaceState, ctx: &mut Ctx) {
  let flying = state.flying;
  state.space.step(&flying);
  state.tick = state.space.tick;
  state.reindex();

  let players: Vec<PlayerId> = state.agents.keys().copied().collect();
  for player in players {
    let Some(seat) = state.seat_of(player) else {
      continue;
    };
    let seen = state.visible_to(seat).to_vec();
    let ships: Vec<ShipState> = seen.iter().map(|id| ship_state(state, *id as usize)).collect();
    let ships = if state.packed {
      // The packed path still builds the same list; what changes is what
      // crosses. Keeping both live is what lets the panel price one against
      // the other without a second run.
      let bytes = pack::pack(&ships);
      state.last_bytes[seat] = bytes.len();
      pack::unpack(&bytes).unwrap_or(ships)
    } else {
      state.last_bytes[seat] = ships.len() * pack::ship_bits_full() / 8;
      ships
    };

    ctx.ops_q().push(TargetedOp::new_system_to(
      player,
      vec![SpaceOp::Frame(Box::new(FrameUpdate {
        frame: state.tick,
        server_time_ms: frame_to_ms(state.tick),
        yours: Some(seat as u16),
        ships,
      }))],
    ));
  }
}

fn ship_state(state: &SpaceState, seat: usize) -> ShipState {
  let ship = &state.space.ships[seat];
  ShipState {
    seat: seat as u16,
    pos: [ship.at.x, ship.at.y, ship.at.z],
    rot: quaternion(ship.yaw, ship.pitch),
    vel: [ship.vel.x, ship.vel.y, ship.vel.z],
  }
}

/// Yaw and pitch to a unit quaternion, which is what the wire carries.
///
/// The simulation reasons in angles because a flight model does; the wire wants
/// a quaternion because smallest-three is 29 bits against 64 for two f32s, and
/// because a client blending orientations wants something it can slerp.
pub fn quaternion(yaw: f32, pitch: f32) -> [f32; 4] {
  let (sy, cy) = (yaw * 0.5).sin_cos();
  // Negated, because a positive rotation about X takes +Z toward -Y while the
  // flight model treats positive pitch as nose up. The two conventions differ
  // by exactly this sign, and nothing but the nose test would have caught it:
  // positions were correct throughout, and every ship simply rendered pitched
  // the wrong way.
  let (sp, cp) = (-pitch * 0.5).sin_cos();
  // Yaw about Y, then pitch about X.
  [cy * sp, sy * cp, -sy * sp, cy * cp]
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Fly;
  use crate::relevance::Strategy;
  use crate::state::VIEW;
  use plaza_client_utils::math::Vec3;

  async fn run(
    state: &mut SpaceState,
    input: LogicInput<SpaceOp, PlayerId>,
  ) -> Vec<TargetedOp<SpaceOp, PlayerId>> {
    SpaceLogic::new().process_input(state, input).await.unwrap().ops
  }

  async fn tick(state: &mut SpaceState) -> Vec<TargetedOp<SpaceOp, PlayerId>> {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await
  }

  fn frames(ops: &[TargetedOp<SpaceOp, PlayerId>]) -> Vec<&FrameUpdate> {
    ops
      .iter()
      .flat_map(|t| t.ops.iter())
      .filter_map(|op| match op {
        SpaceOp::Frame(update) => Some(update.as_ref()),
        _ => None,
      })
      .collect()
  }

  /// Rotates `v` by a unit quaternion, the long way round, so the test does
  /// not share an implementation with the thing it is checking.
  fn rotate(q: [f32; 4], v: Vec3) -> Vec3 {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (vx, vy, vz) = (v.x, v.y, v.z);
    // t = 2 * (q_vec x v)
    let tx = 2.0 * (y * vz - z * vy);
    let ty = 2.0 * (z * vx - x * vz);
    let tz = 2.0 * (x * vy - y * vx);
    Vec3::new(
      vx + w * tx + (y * tz - z * ty),
      vy + w * ty + (z * tx - x * tz),
      vz + w * tz + (x * ty - y * tx),
    )
  }

  #[test]
  fn the_wire_orientation_is_the_nose_the_ship_actually_flies_along() {
    // The sim reasons in yaw and pitch and the wire carries a quaternion, so
    // these are two expressions of one thing that nothing forces to agree.
    // Wrong, every ship renders pointing somewhere it is not going, and no
    // test of positions would ever notice.
    for yaw in [-2.0f32, -0.7, 0.0, 0.4, 1.6, 3.0] {
      for pitch in [-1.2f32, -0.3, 0.0, 0.5, 1.1] {
        let ship = crate::sim::Ship {
          yaw,
          pitch,
          ..Default::default()
        };
        let nose = ship.facing();
        let turned = rotate(quaternion(yaw, pitch), Vec3::new(0.0, 0.0, 1.0));
        let dot = nose.x * turned.x + nose.y * turned.y + nose.z * turned.z;
        assert!(
          dot > 0.999,
          "yaw {yaw} pitch {pitch}: sim faces {nose:?}, wire says {turned:?}, dot {dot}"
        );
      }
    }
  }

  #[tokio::test]
  async fn a_joiner_is_seated_and_flies_something() {
    let mut state = SpaceState::new();
    let ops = run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    let SpaceOp::Seated { seat } = &ops[0].ops[0] else {
      panic!("a joiner is seated");
    };
    assert_eq!(*seat, 0);
    assert!(state.space.ships[0].alive);
  }

  #[tokio::test]
  async fn every_client_gets_its_own_frame_rather_than_a_broadcast() {
    // The structural claim. Two pilots far apart must not receive the same
    // list, or relevance is decorative.
    let mut state = SpaceState::new();
    for id in [7, 8] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await;
    }
    state.space.ships[0].at = Vec3::new(0.0, 0.0, 0.0);
    state.space.ships[1].at = Vec3::new(VIEW * 5.0, 0.0, 0.0);

    let ops = tick(&mut state).await;
    let sent = frames(&ops);
    assert_eq!(sent.len(), 2, "one frame each");
    for update in sent {
      assert_eq!(update.ships.len(), 1, "each sees only itself: {:?}", update.ships);
      assert_eq!(update.ships[0].seat, update.yours.unwrap());
    }
  }

  #[tokio::test]
  async fn a_held_level_keeps_flying_the_ship() {
    let mut state = SpaceState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![SpaceOp::Fly(Fly {
        thrust: 1,
        yaw: 0,
        pitch: 0,
        firing: false,
      })],
    })
    .await;

    let from = state.space.ships[0].at;
    for _ in 0..60 {
      tick(&mut state).await;
    }
    let moved = Vec3::new(
      state.space.ships[0].at.x - from.x,
      state.space.ships[0].at.y - from.y,
      state.space.ships[0].at.z - from.z,
    );
    assert!(moved.length() > 5.0, "one level should hold across ticks: {moved:?}");
  }

  #[tokio::test]
  async fn a_leaver_takes_its_ship_with_it() {
    let mut state = SpaceState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(7),
    })
    .await;
    assert_eq!(state.space.alive(), 1);
    run(&mut state, LogicInput::AgentLeft { agent_id: 7 }).await;
    assert_eq!(state.space.alive(), 0, "no abandoned hulls in space");
  }

  #[tokio::test]
  async fn the_strategy_decides_who_is_told_about_whom() {
    // The same scene under two strategies, through the real tick: one sends a
    // ship four view-radii overhead and the other does not.
    for (strategy, expect) in [(Strategy::Flat, 2), (Strategy::FlatBand, 1)] {
      let mut state = SpaceState::with(strategy);
      for id in [7, 8] {
        run(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_human(id),
        })
        .await;
      }
      state.space.ships[0].at = Vec3::ZERO;
      state.space.ships[1].at = Vec3::new(0.0, VIEW * 4.0, 0.0);

      let ops = tick(&mut state).await;
      let mine = frames(&ops).into_iter().find(|f| f.yours == Some(0)).unwrap();
      assert_eq!(mine.ships.len(), expect, "{} sent {:?}", strategy.name(), mine.ships);
    }
  }

  #[tokio::test]
  async fn the_packed_path_carries_the_same_world() {
    let mut state = SpaceState::new();
    state.packed = true;
    for id in [7, 8] {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await;
    }
    state.space.ships[0].at = Vec3::ZERO;
    state.space.ships[1].at = Vec3::new(10.0, 4.0, -6.0);

    let ops = tick(&mut state).await;
    let mine = frames(&ops).into_iter().find(|f| f.yours == Some(0)).unwrap();
    assert_eq!(mine.ships.len(), 2);
    let other = mine.ships.iter().find(|s| s.seat == 1).unwrap();
    let truth = state.space.ships[1].at;
    let error = ((other.pos[0] - truth.x).powi(2) + (other.pos[1] - truth.y).powi(2) + (other.pos[2] - truth.z).powi(2)).sqrt();
    assert!(error < pack::position_error() * 2.0, "packing moved it {error}");
    assert!(state.last_bytes[0] > 0, "and the panel has a byte count");
  }
}
