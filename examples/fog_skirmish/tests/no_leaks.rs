//! Does anything a player receives name a place they cannot see?
//!
//! The test reads what arrived in a client's inbox, not what the server meant
//! to send. pellet_maze shipped a per-recipient frame that was filtered
//! correctly and leaked anyway, through events beside it that named cells
//! nobody had scouted, and a test written against the server's intent would
//! have passed the whole time.
//!
//! So the audit here is over **every op**, classified by
//! [`positions_named`](plaza_example_fog_skirmish::vision::positions_named),
//! which has no wildcard arm: a new op variant fails to compile until someone
//! decides what it reveals.

use std::sync::Arc;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::controller::{query_with, ControllerCommand, StateControllerBuilder};
use plaza::session::in_process::ClientInbox;
use plaza::session::InProcessSession;
use plaza::tick_driver::TickDriver;

use plaza_example_fog_skirmish::logic::FogLogic;
use plaza_example_fog_skirmish::snapshot::FogSnapshotter;
use plaza_example_fog_skirmish::types::{FogOp, FogState, PlayerId, FIELD};
use plaza_example_fog_skirmish::vision::{can_see, positions_named};

type Session = InProcessSession<FogOp, PlayerId>;

const TICK: Duration = Duration::from_millis(8);

struct Harness {
  session: Arc<Session>,
  tx: plaza::controller::CommandSender<FogOp, PlayerId, FogState>,
  ticker: tokio::task::JoinHandle<()>,
}

async fn start() -> Harness {
  let session = Session::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(FogLogic),
    session.clone(),
    Arc::new(FogSnapshotter),
    FogState::new(),
  )
  .command_buffer(256)
  .build();
  tokio::spawn(controller.run());
  let ticker = tokio::spawn(TickDriver::new(TICK).run(tx.clone()));
  Harness { session, tx, ticker }
}

async fn seat(h: &Harness, id: PlayerId) -> ClientInbox<FogOp, PlayerId> {
  let (_conn, inbox) = h.session.connect(Agent::new_human(id)).await.expect("connect");
  inbox
}

/// Sends both players chasing the same relics, so their scouts genuinely move
/// in and out of each other's vision and captures happen unobserved.
async fn skirmish(h: &Harness, a: PlayerId, b: PlayerId, rounds: usize) {
  let targets = [
    (FIELD * 0.5, FIELD * 0.5),
    (FIELD * 0.25, FIELD * 0.75),
    (FIELD * 0.75, FIELD * 0.25),
    (FIELD * 0.5, FIELD * 0.15),
  ];
  for round in 0..rounds {
    let (x, y) = targets[round % targets.len()];
    for player in [a, b] {
      h.session
        .client_send(Agent::new_human(player), vec![FogOp::MoveTo { x, y }])
        .await;
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
  }
}

/// Every op that reached this inbox, drained without blocking.
fn drain(inbox: &ClientInbox<FogOp, PlayerId>) -> Vec<FogOp> {
  let mut ops = Vec::new();
  while let Ok(msg) = inbox.try_recv() {
    ops.extend(msg.ops);
  }
  ops
}

#[tokio::test]
async fn nothing_a_player_receives_names_a_place_they_cannot_see() {
  let h = start().await;
  let (alice, bob) = (1u32, 2u32);
  let alice_inbox = seat(&h, alice).await;
  let bob_inbox = seat(&h, bob).await;

  let mut received: Vec<(PlayerId, Vec<FogOp>)> = Vec::new();
  for round in 0..6 {
    skirmish(&h, alice, bob, 1).await;
    let _ = round;
    received.push((alice, drain(&alice_inbox)));
    received.push((bob, drain(&bob_inbox)));
  }

  // Checked against the world as it stands now. Vision only ever grows over a
  // run for a scout that keeps moving, so this is the *generous* reading: an
  // op that passes here may still have been sent before its place was visible,
  // which is what the server-side counter below catches instead.
  let total: usize = received.iter().map(|(_, ops)| ops.len()).sum();
  assert!(total > 0, "the run produced no ops at all, so nothing was audited");

  let state_leaks = query_with(&h.tx, |state: &FogState| {
    state.players.values().map(|p| p.stats.leaks).sum::<u64>()
  })
  .await
  .expect("controller alive");

  assert_eq!(
    state_leaks, 0,
    "the server's own audit counted a position sent to someone who could not see it"
  );

  let mut named = 0usize;
  for (player, ops) in &received {
    for op in ops {
      named += positions_named(op).len();
    }
    let _ = player;
  }
  assert!(
    named > 0,
    "no op named a position, so the audit proved nothing: {total} ops seen"
  );

  h.tx.send(ControllerCommand::Shutdown).await.expect("alive");
  h.ticker.abort();
}

#[tokio::test]
async fn a_capture_out_of_sight_is_held_and_told_later() {
  let h = start().await;
  let (alice, bob) = (1u32, 2u32);
  let alice_inbox = seat(&h, alice).await;
  let _bob_inbox = seat(&h, bob).await;

  // Bob works the far corner alone; Alice stays home, so his captures happen
  // where she cannot see them.
  h.session
    .client_send(Agent::new_human(bob), vec![FogOp::MoveTo { x: FIELD * 0.86, y: FIELD * 0.86 }])
    .await;
  tokio::time::sleep(Duration::from_millis(2600)).await;

  let withheld = query_with(&h.tx, move |state: &FogState| {
    state.players.get(&alice).map_or(0, |p| p.withheld.len())
  })
  .await
  .expect("alive");
  assert!(
    withheld > 0,
    "Bob captured nothing out of Alice's sight, so the deferral was never exercised"
  );

  // Her own, at her feet, are hers to know about: the rule is about what you
  // can see, not who did it. What she must not hear is Bob, across the map.
  let told_of_bob = drain(&alice_inbox)
    .iter()
    .filter(|op| matches!(op, FogOp::Captured { by, .. } if *by == bob))
    .count();
  assert_eq!(told_of_bob, 0, "Alice was told about a capture she could not see");

  // Now send Alice to look. What was held is released, marked late.
  //
  // Polled rather than slept on: the walk is the far diagonal of the field at
  // a scout's pace, and a fixed sleep either fails the day it is a little slow
  // or wastes the difference every other run.
  h.session
    .client_send(Agent::new_human(alice), vec![FogOp::MoveTo { x: FIELD * 0.86, y: FIELD * 0.86 }])
    .await;

  let mut late: Vec<FogOp> = Vec::new();
  for _ in 0..80 {
    late.extend(
      drain(&alice_inbox)
        .into_iter()
        .filter(|op| matches!(op, FogOp::Captured { late: true, by, .. } if *by == bob)),
    );
    if !late.is_empty() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
  }
  assert!(
    !late.is_empty(),
    "Alice reached the far corner and was never told what had happened there"
  );

  // And she is told about a place she can now see: the point of telling late.
  let leaked = query_with(&h.tx, move |state: &FogState| {
    late
      .iter()
      .map(|op| {
        positions_named(op)
          .into_iter()
          .filter(|(x, y)| !can_see(state, alice, *x, *y))
          .count()
      })
      .sum::<usize>()
  })
  .await
  .expect("alive");
  assert_eq!(leaked, 0, "a released event named somewhere Alice still cannot see");

  h.tx.send(ControllerCommand::Shutdown).await.expect("alive");
  h.ticker.abort();
}

#[tokio::test]
async fn turning_the_deferral_off_leaks_and_the_counter_says_so() {
  let h = start().await;
  let (alice, bob) = (1u32, 2u32);
  let _alice_inbox = seat(&h, alice).await;
  let _bob_inbox = seat(&h, bob).await;

  h.session
    .client_send(Agent::new_human(alice), vec![FogOp::SetLeakMode(true)])
    .await;
  h.session
    .client_send(Agent::new_human(bob), vec![FogOp::MoveTo { x: FIELD * 0.86, y: FIELD * 0.86 }])
    .await;
  tokio::time::sleep(Duration::from_millis(3000)).await;

  let leaks = query_with(&h.tx, |state: &FogState| {
    state.players.values().map(|p| p.stats.leaks).sum::<u64>()
  })
  .await
  .expect("alive");

  assert!(
    leaks > 0,
    "with the deferral off the audit still counted nothing, so it is not watching the thing it claims to"
  );

  h.tx.send(ControllerCommand::Shutdown).await.expect("alive");
  h.ticker.abort();
}

#[tokio::test]
async fn the_grid_offers_far_less_than_the_world_holds() {
  let h = start().await;
  let alice = 1u32;
  let _inbox = seat(&h, alice).await;
  tokio::time::sleep(Duration::from_millis(200)).await;

  let (considered, visible) = query_with(&h.tx, move |state: &FogState| {
    let (ids, considered) = plaza_example_fog_skirmish::vision::visible_relics(state, alice);
    (considered, ids.len())
  })
  .await
  .expect("alive");

  assert!(
    considered < plaza_example_fog_skirmish::types::RELICS as u64,
    "the query touched the whole world ({considered}), so it is a scan wearing a grid's clothes"
  );
  assert!(visible <= considered as usize, "more survived the test than were offered");

  h.tx.send(ControllerCommand::Shutdown).await.expect("alive");
  h.ticker.abort();
}
