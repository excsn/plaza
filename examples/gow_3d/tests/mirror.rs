//! What a client is holding after a while, against what the server has.
//!
//! spacemo's worst bug lived exactly here and neither half could see it: the
//! server was correct, the client was correct about everything it was told, and
//! what neither owned was **what to do about silence**. This example has two
//! kinds of silence rather than one, so it is worth running both sides and
//! looking at what the client is left with.
//!
//! - A character walks out of view. Absence means gone, and the client must
//!   drop them or it accumulates everyone it has ever stood near.
//! - A **party member** walks out of view. Absence means nothing of the kind,
//!   and the client must keep them or the interface that exists to track people
//!   across a zone stops doing it.
//!
//! The two are the same wire event, and only `Because` separates them.
//!
//! ```sh
//! cargo test -p gow_3d --test mirror -- --nocapture
//! ```

#![cfg(all(feature = "server", feature = "client", feature = "websocket"))]

use gow_3d::logic::GowLogic;
use gow_3d::net::client::NetClient;
use gow_3d::protocol::{Because, GowOp, PlayerId};
use gow_3d::state::GowState;
use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use plaza_ws::scripted::ScriptedSocket;

fn deliver(socket: &ScriptedSocket, ops: &[GowOp]) {
  let mut bytes = Vec::new();
  frame::begin(frame::Kind::Ops, &mut bytes);
  MsgPackCodec.encode_into(&ops.to_vec(), &mut bytes).unwrap();
  socket.feed_message(bytes);
}

/// Everything one seat is sent this tick.
fn ops_for(out: &LogicOutput<GowOp, PlayerId>, seat: u16) -> Vec<GowOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      GowOp::World(frame) => frame.yours == Some(seat),
      _ => true,
    })
    .cloned()
    .collect()
}

async fn seat(logic: &GowLogic, state: &mut GowState, id: PlayerId) {
  logic
    .process_input(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
    .unwrap();
}

async fn send(logic: &GowLogic, state: &mut GowState, id: PlayerId, op: GowOp) -> LogicOutput<GowOp, PlayerId> {
  logic
    .process_input(state, LogicInput::AgentOps {
      source: Agent::new_human(id),
      ops: vec![op],
    })
    .await
    .unwrap()
}

async fn tick(logic: &GowLogic, state: &mut GowState) -> LogicOutput<GowOp, PlayerId> {
  logic
    .process_input(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(33),
    })
    .await
    .unwrap()
}

/// Stands both sides up with two seated players and a connected client for the
/// first.
async fn both_sides() -> (GowLogic, GowState, NetClient, ScriptedSocket) {
  let logic = GowLogic::new();
  let mut state = GowState::new();
  seat(&logic, &mut state, 1).await;
  seat(&logic, &mut state, 2).await;

  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  client.poll(0);
  deliver(&socket, &[GowOp::Seated { seat: 0 }]);
  client.poll(0);
  (logic, state, client, socket)
}

#[tokio::test]
async fn a_character_who_walks_away_is_dropped_and_a_party_member_is_not() {
  // The whole point of `Because` on the wire, and the case a client that
  // treated the frame as a plain visibility list would get wrong in the
  // direction users notice: the party frame going blank when a healer steps
  // round a corner.
  let (logic, mut state, mut client, socket) = both_sides().await;

  let out = tick(&logic, &mut state).await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(33);
  assert_eq!(client.others.len(), 1, "seat 1 is standing nearby");
  assert_eq!(client.because_of(1), Some(Because::Near));

  // They walk to the far side of the zone, honestly, over enough ticks that
  // the validator has no complaint.
  // At the honest run speed and **from where they actually are**: a walk that
  // starts with a jump to the axis is a teleport, and the validator is right
  // to refuse it.
  let per_tick = gow_3d::movement::RUN_SPEED * 33.0 / 1000.0;
  let from = state.zone.characters[&1].tracked.at;
  let mut now = 33;
  for step in 1..=600u32 {
    let x = from.0 + step as f32 * per_tick;
    send(&logic, &mut state, 2, GowOp::Moved { at: (x, from.1, from.2) }).await;
    let out = tick(&logic, &mut state).await;
    now += 33;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert_eq!(state.zone.refusals, 0, "the walk was honest");
  assert!(client.others.is_empty(), "out of view is gone, not remembered for ever");

  // Now party with them and do it again.
  send(&logic, &mut state, 1, GowOp::Party { seat: 1 }).await;
  let out = tick(&logic, &mut state).await;
  now += 33;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(now);

  assert_eq!(client.others.len(), 1, "a party member across the zone is still described");
  assert_eq!(client.because_of(1), Some(Because::Subscribed));
  assert_eq!(client.party().count(), 1);
  assert_eq!(client.in_view().count(), 0, "described, but not close enough to draw a body");
}

#[tokio::test]
async fn a_refusal_snaps_and_an_honest_client_never_sees_one() {
  let (logic, mut state, mut client, socket) = both_sides().await;

  let from = state.zone.characters[&0].tracked.at;
  let mut now = 0;
  for step in 1..=30u32 {
    let at = (from.0 + step as f32 * 0.2, from.1, from.2);
    client.moved_to(at);
    send(&logic, &mut state, 1, GowOp::Moved { at }).await;
    let out = tick(&logic, &mut state).await;
    now += 33;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert_eq!(client.refused, 0, "an honest walk is never refused");
  assert_eq!(state.zone.refusals, 0);

  // A claim across the zone in one tick.
  let out = send(&logic, &mut state, 1, GowOp::Moved { at: (900.0, 0.0, 0.0) }).await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(now);
  assert_eq!(client.refused, 1);
  assert!(client.at.0 < 100.0, "snapped back to where the server had them: {:?}", client.at);

  // The allowance keeps growing while a claim is refused, because a client
  // that was disconnected really did have that long to walk. It looks like a
  // hole and is not one: it accrues at exactly the honest speed, so a client
  // that hammers a teleport still averages no more than a runner. It gets a
  // big jump rarely instead of a small one often, and it is loud the whole
  // time.
  let started = state.zone.now_ms;
  let began_at = state.zone.characters[&0].tracked.at;
  let before = state.zone.refusals;
  for _ in 0..600 {
    // Twenty units ahead of wherever it actually is, which is inside what the
    // budget banks and so a jump that really does land every couple of
    // seconds. A jump bigger than the cap is refused for ever and proves
    // nothing: the average has to be tested against a cheat that works.
    let here = state.zone.characters[&0].tracked.at;
    send(&logic, &mut state, 1, GowOp::Moved {
      at: (here.0 + 20.0, here.1, here.2),
    })
    .await;
    tick(&logic, &mut state).await;
  }
  let elapsed = (state.zone.now_ms - started) as f32 / 1000.0;
  let gained = gow_3d::movement::distance(began_at, state.zone.characters[&0].tracked.at);
  let honest = gow_3d::movement::RUN_SPEED * gow_3d::movement::TOLERANCE * elapsed;
  println!(
    "\n  hammering a teleport for {elapsed:.0}s: {gained:.0} units gained against\n  {honest:.0} an honest runner would cover, and {} refusals logged.\n",
    state.zone.refusals - before
  );
  assert!(gained > 100.0, "the cheat has to land jumps or the cap is untested: {gained}");
  assert!(gained <= honest, "a cheat cannot outrun the allowance it is waiting on");
  assert!(state.zone.refusals - before > 500, "and it is loud throughout");
}

#[tokio::test]
async fn the_client_never_reconciles_against_an_echo_of_itself() {
  // The subtle one. The server repeats this client's own position back in
  // every frame, and a client that applied it would be doing reconciliation in
  // a mode that has nothing to reconcile: a round trip of jitter applied to a
  // position that was already true.
  let (logic, mut state, mut client, socket) = both_sides().await;

  // The first frame is the exception, and the only one: it is where the client
  // learns where the zone put it.
  let out = tick(&logic, &mut state).await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(33);
  assert!(client.seeded, "the spawn comes off the wire rather than being computed twice");
  assert_eq!(client.at, state.zone.characters[&0].tracked.at);

  // Everything after it is an echo of what this client already said.
  client.moved_to((1.0, 0.0, 0.0));
  for _ in 0..5 {
    let out = tick(&logic, &mut state).await;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(66);
  }
  assert!(!client.others.contains_key(&0), "its own seat is not one of the others");
  assert_eq!(client.at, (1.0, 0.0, 0.0), "the local position is untouched by the frame");
}

#[tokio::test]
async fn a_cast_is_described_while_it_runs_and_announced_once_when_it_lands() {
  let (logic, mut state, mut client, socket) = both_sides().await;

  send(&logic, &mut state, 2, GowOp::Cast {
    ability: 0,
    cast_ms: 300,
  })
  .await;

  let mut now = 0;
  let mut saw_bar = 0;
  let mut announcements = 0;
  for _ in 0..20 {
    let out = tick(&logic, &mut state).await;
    now += 33;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
    if client.casting_of(1).is_some() {
      saw_bar += 1;
    }
    announcements += client.landed.len();
  }

  assert!(saw_bar >= 8, "the bar is described every tick it runs: {saw_bar}");
  assert_eq!(announcements, 1, "and the landing is announced exactly once");
}
