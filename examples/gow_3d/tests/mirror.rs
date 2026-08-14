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

use gow_3d::controls::{Authority, Controls};
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
      GowOp::World(frame) => frame.you.map(|you| you.seat) == Some(seat),
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
  // Far enough to leave the *cell* window, not just the view disc: relevance
  // is cell-granular, so a body can be described up to a cell width past the
  // radius before absence begins.
  let per_tick = gow_3d::movement::RUN_SPEED * 33.0 / 1000.0;
  let from = state.zone.characters[&1].tracked.at;
  let mut now = 33;
  for step in 1..=450u32 {
    let x = from.0 + step as f32 * per_tick;
    // Following the ground, because the validator refuses a claim hanging in
    // the air and terrain is the one thing both ends derive rather than send.
    let at = (x, gow_3d::terrain::ground_at(x, from.2), from.2);
    send(&logic, &mut state, 2, GowOp::Moved { at, yaw: 0.0 }).await;
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
    client.moved_to(at, 0.0);
    send(&logic, &mut state, 1, GowOp::Moved { at, yaw: 0.0 }).await;
    let out = tick(&logic, &mut state).await;
    now += 33;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert_eq!(client.refused, 0, "an honest walk is never refused");
  assert_eq!(state.zone.refusals, 0);

  // A claim across the zone in one tick.
  let out = send(&logic, &mut state, 1, GowOp::Moved { at: (900.0, 0.0, 0.0), yaw: 0.0 }).await;
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
  let mut walked = 0.0f32;
  let mut last = began_at;
  for _ in 0..600 {
    // Twenty units ahead of wherever it actually is, which is inside what the
    // budget banks and so a jump that really does land every couple of
    // seconds. A jump bigger than the cap is refused for ever and proves
    // nothing: the average has to be tested against a cheat that works.
    let here = state.zone.characters[&0].tracked.at;
    walked += gow_3d::movement::ground_distance(last, here);
    last = here;
    // Around a circle rather than off in one direction, or the world's own
    // edge stops the cheat before the validator does and the test measures
    // the wrong bound.
    let angle = (state.zone.now_ms as f32 / 4000.0) % std::f32::consts::TAU;
    let (nx, nz) = (here.0 + angle.cos() * 20.0, here.2 + angle.sin() * 20.0);
    send(&logic, &mut state, 1, GowOp::Moved {
      yaw: 0.0,
      at: (nx, gow_3d::terrain::ground_at(nx, nz), nz),
    })
    .await;
    tick(&logic, &mut state).await;
  }
  let elapsed = (state.zone.now_ms - started) as f32 / 1000.0;
  walked += gow_3d::movement::ground_distance(last, state.zone.characters[&0].tracked.at);
  let gained = walked;
  let honest = gow_3d::movement::RUN_SPEED * gow_3d::movement::TOLERANCE * elapsed;
  println!(
    "\n  hammering a teleport for {elapsed:.0}s: {gained:.0} units walked against\n  {honest:.0} an honest runner would cover, and {} refusals logged.\n",
    state.zone.refusals - before
  );
  assert!(
    gained > honest * 0.4,
    "the cheat has to land jumps or the cap is untested: {gained} against {honest}"
  );
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
  client.moved_to((1.0, 0.0, 0.0), 0.0);
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
    ability: 1,
    cast_ms: 1500,
  })
  .await;

  let mut now = 0;
  let mut saw_bar = 0;
  let mut announcements = 0;
  for _ in 0..60 {
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

#[tokio::test]
async fn a_landing_is_drawable_for_longer_than_the_frame_that_carried_it() {
  // The client-side half of what makes an event different from a state. The
  // server mentions a landing on exactly one frame and never again, so a
  // client that does not remember it can draw it for one frame at most, which
  // at 30Hz is 33 milliseconds and invisible.
  let (logic, mut state, mut client, socket) = both_sides().await;
  send(&logic, &mut state, 2, GowOp::Cast { ability: 0, cast_ms: 60 }).await;

  let mut now = 0u64;
  let mut landed_at = None;
  for _ in 0..6 {
    let out = tick(&logic, &mut state).await;
    now += 33;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
    if !client.landed.is_empty() {
      landed_at = Some(now);
    }
  }
  let landed_at = landed_at.expect("the cast landed at all");

  // The frame after is already silent about it, which is the whole point.
  let out = tick(&logic, &mut state).await;
  now += 33;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(now);
  assert!(client.landed.is_empty(), "no later frame mentions it");

  // And it is still drawable, from the client's own memory rather than from
  // anything on the wire.
  assert!(
    client.flashing(now).contains(&1),
    "still on screen {}ms after the one frame that carried it",
    now - landed_at
  );

  // Aged out rather than kept for ever, or the map grows for the session.
  let much_later = landed_at + NetClient::FLASH_MS + 1;
  assert!(client.flashing(much_later).is_empty());
  client.forget_old_flashes(much_later);
  assert!(client.flashes.is_empty(), "and nothing is left holding memory");
}

#[tokio::test]
async fn what_each_authority_mode_costs() {
  // The comparison this example was planned around, and the reason both modes
  // live in one build: two builds and two sessions compare two memories of how
  // something felt.
  //
  // Same walk, same speed constant, same send rate, driven through the real
  // wire both ways. What differs is who decides.
  let dial = Controls::default().shared();
  let logic = GowLogic::new().with_dial(dial.clone());

  println!("\n  a straight walk, 60 ticks, by who decides where you are:\n");
  println!("{:>16} {:>14} {:>14} {:>12}", "authority", "gap now", "worst gap", "refusals");

  let mut rows = Vec::new();
  for mode in [Authority::Client, Authority::Server] {
    dial.lock().authority = mode;

    let mut state = GowState::new();
    seat(&logic, &mut state, 1).await;
    seat(&logic, &mut state, 2).await;
    let socket = ScriptedSocket::new();
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    deliver(&socket, &[GowOp::Seated { seat: 0 }]);
    client.poll(0);
    let out = tick(&logic, &mut state).await;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(0);

    let mut now = 0u64;
    for _ in 0..60 {
      // The client walks forward at the honest speed, exactly as `walk` does.
      let step = gow_3d::movement::RUN_SPEED * 33.0 / 1000.0;
      match client.authority {
        gow_3d::protocol::Authority::Client => {
          let at = (client.at.0, client.at.1, client.at.2 + step);
          client.moved_to(at, 0.0);
          send(&logic, &mut state, 1, GowOp::Moved { at, yaw: 0.0 }).await;
        }
        gow_3d::protocol::Authority::Server => {
          client.intend(0.0, 1);
          send(&logic, &mut state, 1, GowOp::Intent { yaw: 0.0, forward: 1 }).await;
        }
      }
      let out = tick(&logic, &mut state).await;
      now += 33;
      deliver(&socket, &ops_for(&out, 0));
      client.poll(now);
    }

    println!(
      "{:>16} {:>13.2}u {:>13.2}u {:>12}",
      mode.label(),
      client.gap,
      client.worst_gap,
      state.zone.refusals
    );
    rows.push((mode, client.worst_gap, state.zone.refusals, client.at));
  }

  let (_, client_worst, client_refusals, client_at) = rows[0];
  let (_, server_worst, server_refusals, server_at) = rows[1];

  println!("\n  no network delay is simulated here at all, and the two still");
  println!("  differ: server authority starts a tick behind because nothing");
  println!("  local moves until the answer arrives. Latency adds to both, but");
  println!("  only one of them is starting from zero.\n");

  // Both modes actually moved the character, or the comparison is between two
  // things standing still.
  assert!(client_at.2 > 10.0, "the client-authority walk went somewhere: {client_at:?}");
  assert!(server_at.2 > 10.0, "and so did the server-authority one: {server_at:?}");

  // Neither mode produced a refusal on an honest walk. Under server authority
  // that is because no position was ever claimed, which is the security half
  // of what the mode buys.
  assert_eq!(client_refusals, 0, "an honest client-authority walk is never refused");
  assert_eq!(server_refusals, 0, "and a server-authority one has nothing to refuse");

  // The gap is what the two modes actually trade, and this harness delivers
  // every packet instantly, which is what makes the reading worth having: it
  // is the floor, not a measurement of some particular connection.
  assert_eq!(client_worst, 0.0, "the client's own position cannot disagree with itself");

  // One tick of travel, before a single millisecond of network delay exists.
  // That is the cost of asking rather than telling, and everything a real
  // connection adds is on top of it.
  let one_tick = gow_3d::movement::RUN_SPEED * 33.0 / 1000.0;
  assert!(
    server_worst >= one_tick * 0.9,
    "server authority is at least a tick behind: {server_worst} against {one_tick}"
  );
  // Loose upward, because a driven step also climbs whatever the ground does
  // under it, and that rise is part of the distance.
  assert!(
    server_worst < one_tick * 3.0,
    "and not more than a step's worth of it: {server_worst}"
  );
}

#[tokio::test]
async fn a_position_from_a_client_that_does_not_own_one_is_refused() {
  // A client that has not noticed the mode changed is not a cheat, but taking
  // its word would be: under server authority a claimed position is not a
  // claim to check, it is a packet from a client running the other game.
  let dial = Controls::default().shared();
  dial.lock().authority = Authority::Server;
  let logic = GowLogic::new().with_dial(dial);
  let mut state = GowState::new();
  seat(&logic, &mut state, 1).await;
  tick(&logic, &mut state).await;

  let before = state.zone.characters[&0].tracked.at;
  send(&logic, &mut state, 1, GowOp::Moved { at: (900.0, 0.0, 0.0), yaw: 0.0 }).await;
  assert_eq!(state.zone.characters[&0].tracked.at, before, "the server kept its own");

  // And it is **not** counted as a refusal, which is the part worth pinning.
  // The refusal count is the only evidence this design has that somebody is
  // cheating, and a packet that merely crossed a mode change is not evidence
  // of anything. Counting it would make the number jump every time the dial
  // moves, which is exactly when somebody is looking at it.
  assert_eq!(state.zone.refusals, 0, "a mode change is not a cheat");
}
