//! What survives a dropped connection, with both sides running.
//!
//! Every other test here drives one side. This example's whole reason to exist
//! is the half of multiplayer nothing else in the tree exercises, delivery,
//! ordering and reconnection, and the failure its plan named is one no
//! single-sided test can see: **an operation applied twice because a reconnect
//! re-sent it.**
//!
//! A turn-based game is the one place where a disconnection genuinely costs
//! nothing if you wait, since nothing decays and a battle is exactly as valid a
//! minute later. That makes the resend the only real hazard: the client that
//! comes back is a **new connection with a new id**, so the only thing linking
//! it to what it was doing is a token it kept, and the only thing stopping its
//! last choice from landing a second time is that the choice is addressed to
//! the turn it was for.
//!
//! ```sh
//! cargo test -p poketo --test reconnect -- --nocapture
//! ```

#![cfg(all(feature = "server", feature = "client", feature = "websocket"))]

use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use plaza_ws::scripted::ScriptedSocket;
use poketo::battle::Choice;
use poketo::grid::Facing;
use poketo::world::STEP_TICKS;
use poketo::logic::PoketoLogic;
use poketo::net::client::NetClient;
use poketo::protocol::{PlayerId, PoketoOp};
use poketo::state::PoketoState;

fn deliver(socket: &ScriptedSocket, ops: &[PoketoOp]) {
  let mut bytes = Vec::new();
  frame::begin(frame::Kind::Ops, &mut bytes);
  MsgPackCodec.encode_into(&ops.to_vec(), &mut bytes).unwrap();
  socket.feed_message(bytes);
}

/// Everything one seat is sent this tick.
fn ops_for(out: &LogicOutput<PoketoOp, PlayerId>, seat: u16) -> Vec<PoketoOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      PoketoOp::World(world) => world.yours == Some(seat),
      _ => true,
    })
    .cloned()
    .collect()
}

async fn join(logic: &PoketoLogic, state: &mut PoketoState, id: PlayerId) -> LogicOutput<PoketoOp, PlayerId> {
  logic
    .process_input(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
    .unwrap()
}

async fn leave(logic: &PoketoLogic, state: &mut PoketoState, id: PlayerId) {
  logic
    .process_input(state, LogicInput::AgentLeft { agent_id: id })
    .await
    .unwrap();
}

async fn send(
  logic: &PoketoLogic,
  state: &mut PoketoState,
  id: PlayerId,
  op: PoketoOp,
) -> LogicOutput<PoketoOp, PlayerId> {
  logic
    .process_input(state, LogicInput::AgentOps {
      source: Agent::new_human(id),
      ops: vec![op],
    })
    .await
    .unwrap()
}

async fn tick(logic: &PoketoLogic, state: &mut PoketoState) -> LogicOutput<PoketoOp, PlayerId> {
  logic
    .process_input(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await
    .unwrap()
}

/// Walks a seat back and forth through tall grass until it is in a battle.
///
/// The seat is *put* in the grass rather than left to find some. Nothing
/// begins outside it, and the map is a function of the tile, so where the grass
/// is can be looked up rather than wandered into.
const RUN: u32 = 10;

async fn walk_into_a_battle(logic: &PoketoLogic, state: &mut PoketoState, id: PlayerId, seat: u16) -> bool {
  let start = poketo::terrain::grass_run(poketo::world::TOWN_CENTRE, RUN).expect("a patch of tall grass somewhere");
  state.world.seat(seat as usize, start);

  for leg in 0..40u32 {
    let facing = if leg % 2 == 0 { Facing::East } else { Facing::West };
    send(logic, state, id, PoketoOp::Walk(Some(facing))).await;
    for _ in 0..(RUN - 1) * u32::from(STEP_TICKS) {
      tick(logic, state).await;
      if state.battles.contains_key(&seat) {
        return true;
      }
    }
  }
  false
}

#[tokio::test]
async fn a_choice_resent_after_a_reconnect_does_not_play_twice() {
  // The failure this example's plan named, and the one a single-sided test
  // cannot see: the client is correct to resend (it never heard an answer),
  // the server is correct to accept a choice, and what neither owns is
  // whether this choice is the same one.
  let logic = PoketoLogic::new();
  let mut state = PoketoState::new();
  join(&logic, &mut state, 1).await;

  let seat = state.seat_of(1).expect("seated") as u16;
  assert!(
    walk_into_a_battle(&logic, &mut state, 1, seat).await,
    "pacing a patch of tall grass has to start something"
  );

  let turn = state.battles[&seat].turn;
  send(&logic, &mut state, 1, PoketoOp::Choose {
    turn,
    choice: Choice::First,
  })
  .await;

  // The turn resolved, because the wild side answers as soon as the player
  // has. This is the state the resend must not disturb, and taking the reading
  // *after* the first choice is what makes the comparison mean anything.
  let settled_turn = state.battles[&seat].turn;
  let settled: Vec<u8> = state.battles[&seat].sides.iter().map(|s| s.creature.health).collect();
  assert!(settled_turn > turn, "the first choice actually played: {settled_turn} against {turn}");
  assert!(
    settled.iter().any(|health| *health < 100),
    "and something took damage, or there is nothing for a second application to do: {settled:?}"
  );

  // The connection drops before any answer could arrive, which is exactly when
  // a client resends: it has no way to know whether the choice landed.
  let token = state.tokens.get(&1).copied().expect("a token to come back with");
  leave(&logic, &mut state, 1).await;
  assert!(state.seat_of(1).is_none(), "the seat is parked, not still connected");

  // A new connection with a new id, holding the token the old one kept.
  join(&logic, &mut state, 2).await;
  send(&logic, &mut state, 2, PoketoOp::Resume { token }).await;
  let resumed = state.seat_of(2).expect("resumed into the same seat") as u16;
  assert_eq!(resumed, seat, "the token is what links the two connections");
  assert!(state.battles.contains_key(&seat), "and the battle survived the gap");

  // The resend, addressed to the turn it was for.
  send(&logic, &mut state, 2, PoketoOp::Choose {
    turn,
    choice: Choice::First,
  })
  .await;

  let after: Vec<u8> = state.battles[&seat].sides.iter().map(|s| s.creature.health).collect();
  assert_eq!(after, settled, "the resend changed nothing: {after:?} against {settled:?}");
  assert_eq!(
    state.battles[&seat].turn, settled_turn,
    "and did not advance the battle a second time"
  );

  println!("\n  a choice for turn {turn}, resent on a new connection after the old");
  println!("  one dropped: health {settled:?} before and {after:?} after, turn");
  println!("  {settled_turn} both times. The turn number on the choice is the whole");
  println!("  mechanism, and without it this is a move played twice.\n");
}

#[tokio::test]
async fn a_client_that_reconnects_is_told_where_it_is_without_asking() {
  // The other half, driven through the real client so the decode path is the
  // one that ships: a resumed connection has nothing in it, and the first
  // ordinary frame has to be a complete description or the client is blind
  // until something happens to it.
  let logic = PoketoLogic::new();
  let mut state = PoketoState::new();
  join(&logic, &mut state, 1).await;
  let seat = state.seat_of(1).expect("seated") as u16;

  for _ in 0..20 {
    send(&logic, &mut state, 1, PoketoOp::Walk(Some(Facing::East))).await;
    tick(&logic, &mut state).await;
  }
  let token = state.tokens.get(&1).copied().expect("a token");
  let was_at = state.world.walkers[seat as usize].trainer.at;
  leave(&logic, &mut state, 1).await;

  // A fresh client on a fresh socket, exactly as the browser would come back.
  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  client.token = Some(token);
  client.poll(0);

  // Both halves reach the client: the join seats it and the resume puts it
  // back. Dropping the first is how a client ends up connected, walking, and
  // unable to say which trainer is its own.
  let seated = join(&logic, &mut state, 2).await;
  deliver(&socket, &ops_for(&seated, seat));
  let out = send(&logic, &mut state, 2, PoketoOp::Resume { token }).await;
  deliver(&socket, &ops_for(&out, seat));
  client.poll(16);

  let out = tick(&logic, &mut state).await;
  deliver(&socket, &ops_for(&out, seat));
  client.poll(32);

  assert_eq!(client.seat, Some(seat), "the client knows which trainer is its own");
  let mine = client.mine().expect("and is described in the first ordinary frame");
  assert_eq!(mine.at, was_at, "standing where it left off, not back at the door");
}

#[tokio::test]
async fn a_token_that_aged_out_is_a_first_join_rather_than_an_error() {
  // Nothing to tell the client: a resume that fails and a first join are the
  // same situation, and inventing an error for it would make a client handle a
  // case that has no different answer.
  let logic = PoketoLogic::new();
  let mut state = PoketoState::new();
  join(&logic, &mut state, 1).await;
  let seat = state.seat_of(1).expect("seated") as u16;
  let token = state.tokens.get(&1).copied().expect("a token");
  leave(&logic, &mut state, 1).await;

  // Aged past the window rather than removed, which is what a real expiry is.
  state.tick += poketo::state::PARK_TICKS + 1;
  for _ in 0..2 {
    tick(&logic, &mut state).await;
  }

  join(&logic, &mut state, 2).await;
  send(&logic, &mut state, 2, PoketoOp::Resume { token }).await;
  let fresh = state.seat_of(2).expect("seated anyway") as u16;
  assert_eq!(fresh, seat, "the seat was freed and handed out again");
  assert!(state.parked.is_empty(), "and nothing is still being held for a token nobody has");
}
