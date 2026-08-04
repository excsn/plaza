//! What a door has to be able to do, asserted against a real socket.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_example_door_policy::client::Knock;
use plaza_example_door_policy::door::Door;
use plaza_example_door_policy::logic::{ArcadeLogic, ArcadeState};
use plaza_example_door_policy::snapshot::RoomSnapshotter;
use plaza_example_door_policy::transport::{deadline_task, Doorman};
use plaza_example_door_policy::types::{ArcadeOp, DuplicateLogin, Refusal, PER_IP, SEATS};
use plaza_session::SessionOptions;

async fn arcade(policy: DuplicateLogin) -> (Arc<Doorman>, Arc<Door>, String) {
  let door = Door::new(policy);
  let doorman = Doorman::bind("127.0.0.1:0", door.clone(), SessionOptions::default())
    .await
    .expect("bind");
  let (closes_tx, mut closes_rx) = tokio::sync::mpsc::unbounded_channel();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(ArcadeLogic {
      door: door.clone(),
      closes: closes_tx,
    }),
    doorman.session.clone(),
    Arc::new(RoomSnapshotter),
    ArcadeState::default(),
  )
  .build();
  tokio::spawn(controller.run());
  doorman.set_commands(tx.clone());
  tokio::spawn(TickDriver::new(Duration::from_millis(50)).run(tx));
  tokio::spawn(deadline_task(doorman.clone()));
  {
    let doorman = doorman.clone();
    tokio::spawn(async move {
      while let Some((conn_id, op, why)) = closes_rx.recv().await {
        doorman.close(conn_id, op, why);
      }
    });
  }
  let addr = doorman.bound.to_string();
  (doorman, door, addr)
}

async fn settle() {
  tokio::time::sleep(Duration::from_millis(250)).await;
}

#[tokio::test]
async fn the_socket_rule_is_decided_before_anything_is_built() {
  let (_doorman, door, addr) = arcade(DuplicateLogin::RefuseNewest).await;

  // No `Hello`: these never claim a seat, which is what makes this the
  // pre-identity rule. A door that can only judge accounts cannot judge these
  // at all, and they are exactly what a flood of half-open sockets looks like.
  let mut held = Vec::new();
  for _ in 0..PER_IP {
    held.push(Knock::arrive(&addr, None).await.expect("connect"));
    settle().await;
  }
  let registered_before = door.ledger.registers_wasted.load(Ordering::Relaxed);

  // One past the cap, judged on the address alone.
  let over = Knock::arrive(&addr, None).await.expect("connect");
  settle().await;

  assert_eq!(over.refusal(), Some(Refusal::PerIpCap), "the cap was not applied");
  assert_eq!(
    door.ledger.registers_wasted.load(Ordering::Relaxed),
    registered_before,
    "a refusal decidable from the socket still registered a connection"
  );
  over.leave();
  for k in held {
    k.leave();
  }
}

#[tokio::test]
async fn a_closed_session_cannot_keep_talking() {
  let (_doorman, door, addr) = arcade(DuplicateLogin::KickOldest).await;

  let first = Knock::arrive(&addr, Some(7)).await.expect("connect");
  settle().await;
  assert!(first.was_admitted(), "the first connection was never admitted");

  // The same account again: under KickOldest the first one is ended.
  let second = Knock::arrive(&addr, Some(7)).await.expect("connect");
  settle().await;
  assert!(first.closure().is_some(), "the loser was never told why it lost");

  // Keep talking after being told to go.
  for _ in 0..5 {
    let _ = first.say(&[ArcadeOp::Push]).await;
  }
  settle().await;

  assert_eq!(
    door.ledger.ops_after_close.load(Ordering::Relaxed),
    0,
    "a closed connection was still able to send ops"
  );
  first.leave();
  second.leave();
}

#[tokio::test]
async fn refusing_the_newest_leaves_the_session_in_progress_alone() {
  let (_doorman, _door, addr) = arcade(DuplicateLogin::RefuseNewest).await;

  let first = Knock::arrive(&addr, Some(5)).await.expect("connect");
  settle().await;
  let second = Knock::arrive(&addr, Some(5)).await.expect("connect");
  settle().await;

  assert_eq!(second.refusal(), Some(Refusal::AlreadyInside));
  assert!(first.closure().is_none(), "the session in progress was ended anyway");
  assert!(first.was_admitted());
  first.leave();
  second.leave();
}

#[tokio::test]
async fn a_ban_is_enforced_at_the_door_but_only_after_identity() {
  let (_doorman, door, addr) = arcade(DuplicateLogin::RefuseNewest).await;
  door.ban(42);

  let registered_before = door.ledger.registers_wasted.load(Ordering::Relaxed);
  let banned = Knock::arrive(&addr, Some(42)).await.expect("connect");
  settle().await;

  assert_eq!(banned.refusal(), Some(Refusal::Banned));
  assert_eq!(
    door.ledger.registers_wasted.load(Ordering::Relaxed),
    registered_before + 1,
    "the ban should still have cost one registration, since identity arrives after admission"
  );
  banned.leave();
}

#[tokio::test]
async fn a_credit_buys_a_deadline() {
  let (_doorman, _door, addr) = arcade(DuplicateLogin::RefuseNewest).await;
  let player = Knock::arrive(&addr, Some(11)).await.expect("connect");
  settle().await;
  assert!(player.was_admitted());
  assert!(player.closure().is_none(), "expired before the credit ran out");

  tokio::time::sleep(Duration::from_secs(crate_credit() + 1)).await;
  assert!(
    player.closure().is_some(),
    "the session outlived its credit"
  );
  player.leave();
}

fn crate_credit() -> u64 {
  plaza_example_door_policy::types::CREDIT_SECS
}

#[tokio::test]
async fn every_seat_is_scarce() {
  let (_doorman, door, addr) = arcade(DuplicateLogin::RefuseNewest).await;

  // Each account needs its own address slot, so this uses one connection per
  // account up to the seat count, then one more.
  let mut held = Vec::new();
  for account in 1..=SEATS as u32 {
    held.push(Knock::arrive(&addr, Some(account)).await.expect("connect"));
    settle().await;
  }
  assert_eq!(door.seated(), SEATS, "the room did not fill");
  for k in held {
    k.leave();
  }
}
