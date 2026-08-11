//! What a client is holding after a long fight, against what the server has.
//!
//! Every other test here drives one side. The bug that got through them was on
//! the seam: the server was correct, the client was correct about everything it
//! was told, and the thing neither of them owned was **what to do about silence**.
//! Missiles are streamed while they exist and simply stop being sent when they
//! end, so a client that never treated absence as an ending accumulated every
//! one it had ever seen, drawn where it was last seen, for ever.
//!
//! Nothing short of running both sides for a while and looking at what the
//! client is left holding can see that. It is not a decode error, not a
//! divergence, and not a leak in either half on its own.
//!
//! ```sh
//! cargo test -p spacemo --test mirror -- --nocapture
//! ```

#![cfg(feature = "server")]

use std::collections::HashMap;

use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, StateLogic};
use spacemo::logic::SpaceLogic;
use spacemo::net::client::{forget_quiet_bolts, forget_the_quiet, Known, Shot};
use spacemo::protocol::{Fly, PlayerId, SpaceOp};
use spacemo::state::SpaceState;

/// The client's half, reduced to the part this is about: what it holds, and
/// what makes it let go.
#[derive(Default)]
struct Mirror {
  bolts: HashMap<u32, Shot>,
  ships: HashMap<u16, Known>,
  frame: u64,
  dropped: u64,
  dropped_ships: u64,
}

impl Mirror {
  fn receive(&mut self, update: &spacemo::protocol::FrameUpdate) {
    self.frame = update.frame;
    for ship in &update.ships {
      self.ships.insert(
        ship.seat,
        Known {
          state: *ship,
          seen: update.frame,
        },
      );
    }
    // The client's own rule, run here rather than asserted about the server:
    // what the server stops sending is only half of it, and the half that was
    // broken for missiles was this one.
    self.dropped_ships += forget_the_quiet(&mut self.ships, update.frame, update.yours) as u64;
    for bolt in &update.bolts {
      let streamed = self.bolts.contains_key(&bolt.id);
      self.bolts.insert(
        bolt.id,
        Shot {
          state: *bolt,
          seen: update.frame,
          streamed,
        },
      );
    }
    self.dropped += forget_quiet_bolts(&mut self.bolts, update.frame) as u64;
    // The other half of what the real client does each tick: a shot sent once
    // is carried forward and ends when its life runs out.
    self.bolts.retain(|_, bolt| {
      if bolt.streamed {
        return true;
      }
      bolt.state.life = bolt.state.life.saturating_sub(1);
      bolt.state.life > 0
    });
  }
}

async fn seat(logic: &SpaceLogic, state: &mut SpaceState, id: PlayerId) {
  logic
    .process_input(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
    .unwrap();
}

async fn hold(logic: &SpaceLogic, state: &mut SpaceState, id: PlayerId, fly: Fly) {
  logic
    .process_input(state, LogicInput::AgentOps {
      source: Agent::new_human(id),
      ops: vec![SpaceOp::Fly(fly)],
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_client_holds_no_more_than_the_server_has_after_a_long_fight() {
  const TICKS: u64 = 3000;
  let logic = SpaceLogic::new();
  let mut state = SpaceState::new();
  state.bots = 60;
  state.space.set_bots(60);

  for id in 0..4u32 {
    seat(&logic, &mut state, id).await;
  }
  // Everyone in one place, firing both weapons, so shots are constantly being
  // created and constantly ending.
  for seat in 0..4 {
    state.space.ships[seat].at = plaza_client_utils::math::Vec3::new(0.0, seat as f32 * 4.0, seat as f32 * 30.0);
  }
  for id in 0..4u32 {
    hold(&logic, &mut state, id, Fly {
      thrust: 1,
      yaw: 0.0,
      pitch: 0.0,
      firing: true,
      launching: true,
    })
    .await;
  }

  let mut mirror = Mirror::default();
  let (mut worst_held, mut worst_real) = (0usize, 0usize);
  for _ in 0..TICKS {
    let out = logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(16),
      })
      .await
      .unwrap();
    for targeted in &out.ops {
      for op in &targeted.ops {
        if let SpaceOp::Frame(update) = op {
          // One client's view: the first seat's frames only.
          if update.yours == Some(0) {
            mirror.receive(update);
          }
        }
      }
    }
    worst_held = worst_held.max(mirror.bolts.len());
    worst_real = worst_real.max(state.space.bolts.len());
  }

  println!("\n  {TICKS} ticks, four pilots and sixty bots, all firing:\n");
  println!("    shots the server had, at most      {worst_real}");
  println!("    shots this client held, at most    {worst_held}");
  println!("    shots it let go of                 {}\n", mirror.dropped);

  assert!(worst_real > 0, "the fight has to actually produce shots");
  assert!(mirror.dropped > 0, "and the client has to let go of some");

  // The claim: a client cannot end up holding more than exist. It legitimately
  // holds fewer, since it is only told about what it can see. Holding *more* is
  // the failure, and it grows without bound: every missile ever seen, kept for
  // ever, because nothing on the client ends one.
  assert!(
    worst_held <= worst_real,
    "the client held {worst_held} shots against {worst_real} in the world, so it is keeping ones that ended"
  );

  // Ships too, which is the same claim about the other collection: a client
  // cannot end a long fight holding more of them than exist.
  assert!(
    mirror.ships.len() <= state.space.ships.len(),
    "the client held {} ships against {} in the world",
    mirror.ships.len(),
    state.space.ships.len()
  );
}

/// Where the client thinks things are, against where they are, under every
/// combination of the dials that changes how a position crosses.
///
/// A bound that stops covering the world clamps rather than errors, and a
/// relative frame decoded against the wrong anchor lands somewhere plausible.
/// Neither raises anything, and both look exactly like this test not existing.
#[tokio::test]
async fn a_client_lands_where_the_server_is_under_every_dial() {
  for packed in [false, true] {
    for relative in [false, true] {
      let logic = SpaceLogic::new();
      let mut state = SpaceState::new();
      state.packed = packed;
      state.relative = relative;
      state.bots = 40;
      state.space.set_bots(40);
      for id in 0..2u32 {
        seat(&logic, &mut state, id).await;
      }
      for id in 0..2u32 {
        hold(&logic, &mut state, id, Fly {
          thrust: 1,
          yaw: 0.7,
          pitch: -0.3,
          firing: true,
          launching: true,
        })
        .await;
      }

      let mut worst = 0.0f32;
      let mut checked = 0usize;
      for _ in 0..600u64 {
        let out = logic
          .process_input(&mut state, LogicInput::TimeStep {
            delta_time: std::time::Duration::from_millis(16),
          })
          .await
          .unwrap();
        for targeted in &out.ops {
          for op in &targeted.ops {
            let SpaceOp::Frame(update) = op else { continue };
            // Scored against the ships this frame actually names, at the tick
            // it names them, so this measures decoding rather than staleness.
            for ship in &update.ships {
              let truth = &state.space.ships[ship.seat as usize];
              let dx = ship.pos[0] - truth.at.x;
              let dy = ship.pos[1] - truth.at.y;
              let dz = ship.pos[2] - truth.at.z;
              worst = worst.max((dx * dx + dy * dy + dz * dz).sqrt());
              assert_eq!(
                ship.health, truth.health,
                "health is a state and must arrive exactly, packed {packed} relative {relative}"
              );
              checked += 1;
            }
          }
        }
      }

      // Full width is exact; the packed paths carry their own rounding, and a
      // relative offset composes the anchor's with its own.
      let tolerance = if !packed {
        0.001
      } else if relative {
        spacemo::pack::relative_error() * 2.0
      } else {
        spacemo::pack::position_error() * 2.0
      };
      println!("  packed {packed:<5} relative {relative:<5} worst {worst:.4}u over {checked} ships");
      assert!(checked > 1000, "the run has to carry ships: {checked}");
      assert!(
        worst < tolerance,
        "packed {packed} relative {relative}: {worst} is past {tolerance}"
      );
    }
  }
}

#[tokio::test]
async fn a_client_stops_hearing_about_a_ship_that_leaves_and_lets_go_of_it() {
  // The same shape one entity along, and the half that already worked: ships
  // are dropped on silence too, which is where the rule for shots came from.
  let logic = SpaceLogic::new();
  let mut state = SpaceState::new();
  for id in 0..2u32 {
    seat(&logic, &mut state, id).await;
  }
  state.space.ships[0].at = plaza_client_utils::math::Vec3::ZERO;
  state.space.ships[1].at = plaza_client_utils::math::Vec3::new(0.0, 0.0, 40.0);

  let mut mirror = Mirror::default();
  let tick = async |state: &mut SpaceState, mirror: &mut Mirror| {
    let out = logic
      .process_input(state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(16),
      })
      .await
      .unwrap();
    for targeted in &out.ops {
      for op in &targeted.ops {
        if let SpaceOp::Frame(update) = op
          && update.yours == Some(0)
        {
          mirror.receive(update);
        }
      }
    }
  };

  for _ in 0..10 {
    tick(&mut state, &mut mirror).await;
  }
  assert!(mirror.ships.contains_key(&1), "it should have been told about the other ship");

  // Out of range, and never mentioned again.
  state.space.ships[1].at = plaza_client_utils::math::Vec3::new(0.0, 0.0, spacemo::sim::VOLUME * 0.9);
  for _ in 0..10 {
    tick(&mut state, &mut mirror).await;
  }
  assert!(
    mirror.ships.contains_key(&1),
    "still held while the silence is short, or the edge of the world strobes"
  );

  // Long enough for the client's own rule to fire. Asserted on the client
  // letting go rather than on the server going quiet, because a server that
  // stops sending and a client that never drops is exactly the shape the
  // missiles had.
  for _ in 0..40 {
    tick(&mut state, &mut mirror).await;
  }
  assert!(!mirror.ships.contains_key(&1), "the client should have let go of it");
  assert_eq!(mirror.dropped_ships, 1);
  assert!(mirror.ships.contains_key(&0), "and never of its own");
}
