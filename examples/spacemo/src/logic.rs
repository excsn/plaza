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
use crate::protocol::{frame_to_ms, BoltState, FrameUpdate, PlayerId, ShipState, SpaceOp};
use crate::sim::quaternion;
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
      state.sticky_locks = wanted.sticky_locks;
      state.relative = wanted.relative;
      state.view = wanted.view.clamp(40.0, crate::max_view());
      state.stream_bolts = wanted.stream_bolts;
      if state.bots != wanted.bots {
        state.bots = wanted.bots;
        state.space.set_bots(wanted.bots);
      }
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
  state.space.step_with(&flying, state.sticky_locks);
  state.tick = state.space.tick;
  state.follow_locks(state.sticky_locks);
  state.reindex();

  let players: Vec<PlayerId> = state.agents.keys().copied().collect();
  for player in players {
    let Some(seat) = state.seat_of(player) else {
      continue;
    };
    let seen = state.visible_to(seat).to_vec();
    let ships: Vec<ShipState> = seen.iter().map(|id| ship_state(state, *id as usize)).collect();
    let near = state.bolts_visible_to(seat).to_vec();
    let stream = state.stream_bolts;
    let visible: Vec<BoltState> = near
      .iter()
      .filter_map(|id| state.space.bolts.get(*id as usize))
      .map(|bolt| BoltState {
        id: (bolt.key.index << 8) | bolt.key.generation as u32 & 0xff,
        homing: bolt.chasing.is_some(),
        pos: [bolt.at.x, bolt.at.y, bolt.at.z],
        vel: [bolt.vel.x, bolt.vel.y, bolt.vel.z],
        life: bolt.life,
      })
      .collect();

    // Anything no longer in flight is forgotten by the diff, which is also
    // what lets a reused slot be announced again rather than mistaken for the
    // shot that vacated it.
    let mut announced = std::collections::HashSet::new();
    state.told.diff(
      player,
      visible.iter().filter(|bolt| !bolt.homing).map(|bolt| (bolt.id, ())),
      |id, fresh| {
        if fresh.is_some() {
          announced.insert(id);
        }
      },
    );

    let bolts: Vec<BoltState> = visible
      .into_iter()
      .filter(|bolt| {
        // A homing shot is sent every frame because its path depends on where
        // its target goes next, and nobody knows that in advance. A straight
        // one is sent once, and the client carries it forward itself.
        stream || bolt.homing || announced.contains(&bolt.id)
      })
      .collect();
    let (ships, bolts) = if state.packed {
      // The packed path still builds the same lists; what changes is what
      // crosses. Keeping both live is what lets the panel price one against
      // the other without a second run.
      let anchor = ships
        .iter()
        .find(|s| s.seat == seat as u16)
        .map(|s| s.pos)
        .unwrap_or([0.0; 3]);
      let ship_bytes = if state.relative {
        pack::pack_relative(&ships, anchor)
      } else {
        pack::pack(&ships)
      };
      let bolt_bytes = pack::pack_bolts(&bolts);
      state.last_bytes[seat] = ship_bytes.len();
      state.last_bolt_bytes[seat] = bolt_bytes.len();
      let decoded = if state.relative {
        pack::unpack_relative(&ship_bytes)
      } else {
        pack::unpack(&ship_bytes)
      };
      (decoded.unwrap_or(ships), pack::unpack_bolts(&bolt_bytes).unwrap_or(bolts))
    } else {
      state.last_bytes[seat] = ships.len() * pack::ship_bits_full() / 8;
      state.last_bolt_bytes[seat] = bolts.len() * 8 * 32 / 8;
      (ships, bolts)
    };

    ctx.ops_q().push(TargetedOp::new_system_to(
      player,
      vec![SpaceOp::Frame(Box::new(FrameUpdate {
        frame: state.tick,
        server_time_ms: frame_to_ms(state.tick),
        yours: Some(seat as u16),
        locked: state.space.lock_for(seat),
        reload: state.space.reload_left(seat),
        ships,
        bolts,
        // Only the ones this client can see, so a hit on the far side of the
        // volume is not an event it has to be told about.
        hits: state
          .space
          .hits
          .iter()
          .copied()
          .filter(|struck| seen.contains(&(*struck as u32)))
          .collect(),
        // Visible, *or* about this client. Being told you died by someone you
        // never saw is the whole experience of being sniped, and withholding
        // the name would be relevance applied past the point it helps.
        kills: state
          .space
          .kills
          .iter()
          .copied()
          .filter(|kill| {
            kill.killer == seat as u16
              || kill.victim == seat as u16
              || seen.contains(&(kill.victim as u32))
          })
          .map(|k| crate::protocol::Kill {
            killer: k.killer,
            victim: k.victim,
            streak: k.streak,
          })
          .collect(),
      }))],
    ));
  }
}

fn ship_state(state: &SpaceState, seat: usize) -> ShipState {
  let ship = &state.space.ships[seat];
  ShipState {
    seat: seat as u16,
    health: ship.health,
    pos: [ship.at.x, ship.at.y, ship.at.z],
    rot: quaternion(ship.yaw, ship.pitch),
    vel: [ship.vel.x, ship.vel.y, ship.vel.z],
  }
}


#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Fly;
  use crate::relevance::Strategy;
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

  /// What transient entities cost against the standing world.
  ///
  /// Every other example in the tree measures steady state: N bodies updating
  /// every tick. This is the other half, and the reason the answer is not
  /// obvious is that bolts are individually cheap and collectively numerous.
  #[tokio::test]
  async fn what_churn_costs_against_a_standing_world() {
    let mut state = SpaceState::new();
    for id in 0..8u32 {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await;
    }
    // Everyone in one fight, so the bolts are actually in each other's view.
    for seat in 0..8 {
      state.space.ships[seat].at = Vec3::new(seat as f32 * 12.0, 0.0, 0.0);
    }
    for id in 0..8u32 {
      run(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(id),
        ops: vec![SpaceOp::Fly(Fly {
          thrust: 0,
          yaw: 0.0,
          pitch: 0.0,
          firing: true,
          launching: false,
        })],
      })
      .await;
    }

    let (mut ship_bytes, mut bolt_bytes, mut ships, mut bolts, mut counted) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for _ in 0..600 {
      let ops = tick(&mut state).await;
      for update in frames(&ops) {
        ships += update.ships.len();
        bolts += update.bolts.len();
        counted += 1;
      }
      for seat in 0..8 {
        ship_bytes += state.last_bytes[seat];
        bolt_bytes += state.last_bolt_bytes[seat];
      }
    }

    let per = counted.max(1) as f32;
    println!("\n  eight ships in one fight, ten seconds, per frame per client:\n");
    println!("    ships  {:>6.1} at {:>7.1} bytes", ships as f32 / per, ship_bytes as f32 / per);
    println!("    bolts  {:>6.1} at {:>7.1} bytes", bolts as f32 / per, bolt_bytes as f32 / per);
    println!(
      "\n  {} spawned and {} expired over the run, so the transient half of\n  the world turned over {:.0} times while the standing half sat still.\n",
      state.space.spawned,
      state.space.expired,
      state.space.expired as f32 / 8.0
    );

    assert!(bolts > 0, "the fight has to actually produce bolts");
    assert!(state.space.expired > 0, "and they have to expire");
    // The claim worth pinning: a bolt is cheaper than a ship, or transient
    // entities would be unaffordable at the rate they are created.
    let bolt_each = bolt_bytes as f32 / bolts.max(1) as f32;
    let ship_each = ship_bytes as f32 / ships.max(1) as f32;
    assert!(
      bolt_each < ship_each,
      "a bolt should cost less than a ship: {bolt_each:.1} against {ship_each:.1}"
    );
  }

  /// What a populated volume does to the panel, which is why bots exist.
  ///
  /// With one ship in flight every strategy returns the same answer, so the
  /// dial moves and nothing else does. This is the measurement made watchable.
  #[tokio::test]
  async fn the_strategy_dial_only_says_anything_in_a_populated_volume() {
    let mut seen = Vec::new();
    for strategy in [Strategy::Flat, Strategy::FlatBand] {
      for bots in [0usize, 300] {
        let mut state = SpaceState::with(strategy);
        state.bots = bots;
        state.space.set_bots(bots);
        run(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_human(7),
        })
        .await;
        state.space.ships[0].at = Vec3::ZERO;

        let mut worst = 0usize;
        for _ in 0..30 {
          let ops = tick(&mut state).await;
          for update in frames(&ops) {
            worst = worst.max(update.ships.len());
          }
        }
        seen.push((strategy, bots, worst));
      }
    }

    println!("\n  ships in view, by strategy and population:\n");
    for (strategy, bots, ships) in &seen {
      println!("    {:<16} {bots:>4} bots  {ships:>4} in view", strategy.name());
    }

    let empty_flat = seen.iter().find(|(s, b, _)| *s == Strategy::Flat && *b == 0).unwrap().2;
    let empty_band = seen.iter().find(|(s, b, _)| *s == Strategy::FlatBand && *b == 0).unwrap().2;
    let full_flat = seen.iter().find(|(s, b, _)| *s == Strategy::Flat && *b == 300).unwrap().2;
    let full_band = seen.iter().find(|(s, b, _)| *s == Strategy::FlatBand && *b == 300).unwrap().2;

    assert_eq!(empty_flat, empty_band, "an empty volume cannot tell the strategies apart");
    assert!(
      full_flat > full_band,
      "a populated one must: {full_flat} against {full_band}"
    );
    println!(
      "\n  empty, both say {empty_flat}. populated, {full_flat} against {full_band}.\n  the dial is only a demonstration when there is something to see.\n"
    );
  }

  /// What it costs to send a path that could have been derived.
  ///
  /// The two weapons differ by one field and have opposite wire profiles, and
  /// this is the number that says so rather than the paragraph.
  #[tokio::test]
  async fn a_straight_shot_need_not_have_its_path_sent_and_a_homing_one_must() {
    let mut rows = Vec::new();
    for stream in [true, false] {
      let mut state = SpaceState::new();
      state.stream_bolts = stream;
      for id in 0..6u32 {
        run(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_human(id),
        })
        .await;
      }
      // Strung out along +Z, which is where a ship at yaw zero is looking, so
      // each has the next one inside its lock cone. Lined up across the nose
      // instead, nothing acquires a target, no missile ever launches, and the
      // comparison quietly loses the half it exists to make.
      for seat in 0..6 {
        state.space.ships[seat].at = Vec3::new(0.0, seat as f32 * 3.0, seat as f32 * 55.0);
      }
      for id in 0..6u32 {
        run(&mut state, LogicInput::AgentOps {
          source: Agent::new_human(id),
          ops: vec![SpaceOp::Fly(Fly {
            thrust: 0,
            yaw: 0.0,
            pitch: 0.0,
            firing: true,
            launching: id.is_multiple_of(2),
          })],
        })
        .await;
      }

      let (mut carried, mut counted, mut homing) = (0usize, 0usize, 0usize);
      for _ in 0..600 {
        let ops = tick(&mut state).await;
        for update in frames(&ops) {
          carried += update.bolts.len();
          homing += update.bolts.iter().filter(|b| b.homing).count();
          counted += 1;
        }
      }
      // Asserted rather than assumed. The first version of this scene lined the
      // ships up across the nose, so nothing acquired a target, no missile ever
      // launched, and the comparison read 83x while measuring only half of
      // itself.
      assert!(homing > 0, "the scene has to actually produce homing shots");
      rows.push((stream, carried as f32 / counted.max(1) as f32));
    }

    let streamed = rows.iter().find(|(s, _)| *s).unwrap().1;
    let spawned = rows.iter().find(|(s, _)| !*s).unwrap().1;
    println!("\n  shots carried per frame, per client:\n");
    println!("    every path sent        {streamed:>6.1}");
    println!("    spawns and homing only {spawned:>6.1}");
    println!("\n  {:.1}x, and the difference is entirely shots whose whole", streamed / spawned.max(0.01));
    println!("  future was already implied by where they started.\n");

    assert!(
      spawned * 3.0 < streamed,
      "sending a derivable path should cost several times as much: {streamed} against {spawned}"
    );
    assert!(spawned > 0.0, "and homing shots still have to cross every frame");
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
    state.space.ships[1].at = Vec3::new(crate::default_view() * 5.0, 0.0, 0.0);

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
        yaw: 0.0,
        pitch: 0.0,
        firing: false,
        launching: false,
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
      // Clear of the view and still inside the volume. A multiple of the radius
      // wrapped at the boundary once the radius grew, putting the ship back
      // *inside* the view from the other side and quietly inverting the test.
      state.space.ships[1].at = Vec3::new(0.0, crate::sim::VOLUME * 0.95, 0.0);

      let ops = tick(&mut state).await;
      let mine = frames(&ops).into_iter().find(|f| f.yours == Some(0)).unwrap();
      assert_eq!(mine.ships.len(), expect, "{} sent {:?}", strategy.name(), mine.ships);
    }
  }

  #[tokio::test]
  async fn the_relative_path_carries_the_same_world_as_the_absolute_one() {
    // Both dials, one scene, through the real tick. Two encodings that disagree
    // about where a ship is would be a defect no test of either alone finds.
    let mut places = Vec::new();
    for relative in [false, true] {
      let mut state = SpaceState::new();
      state.packed = true;
      state.relative = relative;
      for id in [7, 8] {
        run(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_human(id),
        })
        .await;
      }
      state.space.ships[0].at = Vec3::new(120.0, -40.0, 60.0);
      state.space.ships[1].at = Vec3::new(140.0, -30.0, 75.0);

      let ops = tick(&mut state).await;
      let mine = frames(&ops).into_iter().find(|f| f.yours == Some(0)).unwrap();
      let other = mine.ships.iter().find(|s| s.seat == 1).unwrap();
      places.push(other.pos);
    }

    let (a, b) = (places[0], places[1]);
    for axis in 0..3 {
      assert!(
        (a[axis] - b[axis]).abs() < pack::position_error() + pack::relative_error(),
        "axis {axis}: absolute says {}, relative says {}",
        a[axis],
        b[axis]
      );
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
