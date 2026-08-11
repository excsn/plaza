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

use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, StateLogic};
use plaza_ws::scripted::ScriptedSocket;
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use spacemo::logic::SpaceLogic;
use spacemo::net::client::NetClient;
use spacemo::protocol::{Fly, PlayerId, SpaceOp};
use spacemo::state::SpaceState;

/// Hands the server's own ops to a real client, through the real decode path.
///
/// The point of doing it this way rather than keeping a second copy of what the
/// client does: a test that reimplements the code under test passes while the
/// code drifts away from it, which is the same trap as any other pair of
/// derivations of one fact.
fn deliver(socket: &ScriptedSocket, ops: &[SpaceOp]) {
  let mut bytes = Vec::new();
  frame::begin(frame::Kind::Ops, &mut bytes);
  MsgPackCodec.encode_into(&ops.to_vec(), &mut bytes).unwrap();
  socket.feed_message(bytes);
}

/// Everything one seat is sent this tick.
fn ops_for(out: &plaza::state_logic::LogicOutput<SpaceOp, PlayerId>, seat: u16) -> Vec<SpaceOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      SpaceOp::Frame(update) => update.yours == Some(seat),
      _ => true,
    })
    .cloned()
    .collect()
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

  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  let (mut worst_held, mut worst_real) = (0usize, 0usize);
  let mut now_ms = 0u64;
  for _ in 0..TICKS {
    let out = logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(16),
      })
      .await
      .unwrap();
    deliver(&socket, &ops_for(&out, 0));
    now_ms += 16;
    // The real thing: decode, apply, and run whatever the client does each
    // frame to decide what it is still holding.
    client.poll(now_ms);
    client.predict(1.0 / 60.0);
    worst_held = worst_held.max(client.bolts.len());
    worst_real = worst_real.max(state.space.bolts.len());
  }

  println!("\n  {TICKS} ticks, four pilots and sixty bots, all firing:\n");
  println!("    shots the server had, at most      {worst_real}");
  println!("    shots this client held, at most    {worst_held}");
  println!("    shots it let go of                 {}\n", client.stale_bolts);

  assert!(worst_real > 0, "the fight has to actually produce shots");
  assert!(client.stale_bolts > 0, "and the client has to let go of some");

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
    client.ships.len() <= state.space.ships.len(),
    "the client held {} ships against {} in the world",
    client.ships.len(),
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

/// Every kill a client is part of has to reach it, however far away the other
/// half was.
///
/// The only **event** on this wire, and the only thing whose delivery matters:
/// a state is described again next frame and an event is not. The filter that
/// decides who hears one is hand-written and reads visible-or-about-you, which
/// is a rule with two halves and no test until now. Losing the second half
/// would look like being killed by nothing, which is a worse bug than the
/// bandwidth it saves.
#[tokio::test]
async fn every_kill_a_client_is_part_of_reaches_it() {
  let logic = SpaceLogic::new();
  let mut state = SpaceState::new();
  state.bots = 40;
  state.space.set_bots(40);
  for id in 0..3u32 {
    seat(&logic, &mut state, id).await;
  }
  for id in 0..3u32 {
    hold(&logic, &mut state, id, Fly {
      thrust: 1,
      yaw: 0.4,
      pitch: 0.1,
      firing: true,
      launching: true,
    })
    .await;
  }

  let mut mine_on_the_wire = 0usize;
  let mut mine_in_the_world = 0usize;
  for _ in 0..2500u64 {
    let out = logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(16),
      })
      .await
      .unwrap();

    // What happened this tick that seat zero is part of.
    let owed: Vec<_> = state
      .space
      .kills
      .iter()
      .filter(|k| k.killer == 0 || k.victim == 0)
      .copied()
      .collect();
    mine_in_the_world += owed.len();

    for targeted in &out.ops {
      for op in &targeted.ops {
        let SpaceOp::Frame(update) = op else { continue };
        if update.yours != Some(0) {
          continue;
        }
        for kill in &update.kills {
          if kill.killer == 0 || kill.victim == 0 {
            mine_on_the_wire += 1;
          }
        }
        // And what it was told must actually have happened.
        for kill in &update.kills {
          assert!(
            state.space.kills.iter().any(|k| k.killer == kill.killer && k.victim == kill.victim),
            "a kill was announced that the world does not have: {kill:?}"
          );
        }
      }
    }
  }

  println!("
  seat zero was part of {mine_in_the_world} kills and was told about {mine_on_the_wire}
");
  assert!(mine_in_the_world > 0, "the fight has to involve this seat at all");
  assert_eq!(
    mine_on_the_wire, mine_in_the_world,
    "a client has to hear about every kill it is part of, wherever the other half was standing"
  );
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

  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  let mut now_ms = 0u64;
  let tick = async |state: &mut SpaceState, client: &mut NetClient, now_ms: &mut u64| {
    let out = logic
      .process_input(state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(16),
      })
      .await
      .unwrap();
    deliver(&socket, &ops_for(&out, 0));
    *now_ms += 16;
    client.poll(*now_ms);
    client.predict(1.0 / 60.0);
  };

  for _ in 0..10 {
    tick(&mut state, &mut client, &mut now_ms).await;
  }
  assert!(client.ships.contains_key(&1), "it should have been told about the other ship");

  // Out of range, and never mentioned again.
  state.space.ships[1].at = plaza_client_utils::math::Vec3::new(0.0, 0.0, spacemo::sim::VOLUME * 0.9);
  for _ in 0..10 {
    tick(&mut state, &mut client, &mut now_ms).await;
  }
  assert!(
    client.ships.contains_key(&1),
    "still held while the silence is short, or the edge of the world strobes"
  );

  // Long enough for the client's own rule to fire. Asserted on the client
  // letting go rather than on the server going quiet, because a server that
  // stops sending and a client that never drops is exactly the shape the
  // missiles had.
  for _ in 0..40 {
    tick(&mut state, &mut client, &mut now_ms).await;
  }
  assert!(!client.ships.contains_key(&1), "the client should have let go of it");
  assert_eq!(client.forgotten, 1);
  assert!(client.ships.contains_key(&0), "and never of its own");
}
