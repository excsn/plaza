//! What a door has to be able to do, asserted against a real socket.

use std::sync::atomic::Ordering;
use std::time::Duration;

use plaza_example_door_policy::client::Knock;
use plaza_example_door_policy::types::{ArcadeOp, DuplicateLogin, Refusal, CREDENTIAL_WAIT, CREDIT_SECS, CREDIT_SPENT, SEATS, STARTING_CREDITS};
use plaza_example_door_policy::{arcade, arcade_with};

async fn settle() {
  tokio::time::sleep(Duration::from_millis(250)).await;
}

#[tokio::test]
async fn an_address_over_its_cap_is_refused_before_anything_is_built() {
  let (session, door) = arcade_with(DuplicateLogin::RefuseNewest, 2).await;
  let addr = session.local_addr().to_string();

  let mut held = Vec::new();
  for account in [101, 102] {
    held.push(Knock::arrive(&addr, Some(account)).await.expect("connect"));
    settle().await;
  }
  let registered_before = door.ledger.registered.load(Ordering::Relaxed);

  let over = Knock::arrive(&addr, Some(103)).await.expect("connect");
  settle().await;

  assert_eq!(over.refusal(), Some(Refusal::PerIpCap), "the cap was not applied");
  assert_eq!(
    door.ledger.registered.load(Ordering::Relaxed),
    registered_before,
    "a refusal registered a connection"
  );
  assert_eq!(session.manager().stats().refused(), 1, "the transport counted it too");
  assert_eq!(session.manager().connection_count(), 2);
  over.leave();
  for k in held {
    k.leave();
  }
}

#[tokio::test]
async fn a_closed_session_cannot_keep_talking() {
  let (session, door) = arcade(DuplicateLogin::KickOldest).await;
  let addr = session.local_addr().to_string();

  let first = Knock::arrive(&addr, Some(7)).await.expect("connect");
  settle().await;
  assert!(first.was_admitted(), "the first connection was never admitted");

  // The same account again: under KickOldest the first one is ended.
  let second = Knock::arrive(&addr, Some(7)).await.expect("connect");
  settle().await;
  assert_eq!(
    first.closure().as_deref(),
    Some("signed in from somewhere else"),
    "the loser was never told why it lost"
  );
  assert!(second.was_admitted());

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
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();

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
async fn a_ban_is_enforced_at_the_door_before_anything_is_built() {
  let (session, door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  door.ban(42);

  let banned = Knock::arrive(&addr, Some(42)).await.expect("connect");
  settle().await;

  assert_eq!(banned.refusal(), Some(Refusal::Banned));
  assert_eq!(
    door.ledger.registered.load(Ordering::Relaxed),
    0,
    "the ban cost a registration, so identity was judged after admission"
  );
  assert_eq!(session.manager().connection_count(), 0);
  banned.leave();
}

#[tokio::test]
async fn a_socket_that_presents_nothing_is_closed_when_the_wait_runs_out() {
  let (session, door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();

  let silent = Knock::arrive(&addr, None).await.expect("connect");
  tokio::time::sleep(CREDENTIAL_WAIT / 2).await;
  assert!(!silent.timed_out(), "closed before the wait ran out");
  assert_eq!(session.manager().stats().pending(), 1);

  tokio::time::sleep(CREDENTIAL_WAIT).await;
  assert!(silent.timed_out(), "the session outlived its wait");
  assert_eq!(session.manager().stats().timed_out(), 1);
  assert_eq!(session.manager().stats().pending(), 0);
  assert_eq!(door.ledger.registered.load(Ordering::Relaxed), 0);
  silent.leave();
}

#[tokio::test]
async fn a_credit_buys_a_deadline() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let player = Knock::arrive(&addr, Some(11)).await.expect("connect");
  settle().await;
  assert!(player.was_admitted());
  assert!(player.closure().is_none(), "expired before the credit ran out");

  tokio::time::sleep(Duration::from_secs(CREDIT_SECS + 1)).await;
  assert_eq!(player.closure().as_deref(), Some("your credit ran out"), "the session outlived its credit");
  player.leave();
}

/// The room's clock for this account, from the latest snapshot it was sent.
fn seconds_left(player: &Knock, account: u32) -> Option<u64> {
  player.heard.lock().iter().rev().find_map(|op| match op {
    ArcadeOp::Snapshot(room) => room.seats.iter().find(|s| s.account == account).map(|s| s.seconds_left),
    _ => None,
  })
}

#[tokio::test]
async fn a_coin_adds_to_the_time_left_rather_than_restarting_it() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let player = Knock::arrive(&addr, Some(12)).await.expect("connect");
  settle().await;
  player.say(&[ArcadeOp::InsertCoin, ArcadeOp::InsertCoin]).await.expect("send");
  settle().await;
  assert_eq!(seconds_left(&player, 12), Some(3 * CREDIT_SECS), "two coins on top of the free credit");

  tokio::time::sleep(Duration::from_secs(CREDIT_SECS + 1)).await;
  assert!(player.closure().is_none(), "the coins restarted the clock instead of adding to it");
  player.leave();
}

#[tokio::test]
async fn a_coin_with_no_credit_left_is_answered() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let player = Knock::arrive(&addr, Some(14)).await.expect("connect");
  settle().await;
  // Arrival spent one of the three, so the third coin has nothing to spend.
  player
    .say(&[ArcadeOp::InsertCoin, ArcadeOp::InsertCoin, ArcadeOp::InsertCoin])
    .await
    .expect("send");
  settle().await;
  let refused = player
    .heard
    .lock()
    .iter()
    .filter(|op| matches!(op, ArcadeOp::NoCredit { account: 14 }))
    .count();
  assert_eq!(refused, 1, "the coin past the last credit went unanswered");
  player.leave();
}

/// The credits the room last showed for this account.
fn credits(player: &Knock, account: u32) -> Option<u32> {
  player.heard.lock().iter().rev().find_map(|op| match op {
    ArcadeOp::Snapshot(room) => room.seats.iter().find(|s| s.account == account).map(|s| s.credits),
    _ => None,
  })
}

#[tokio::test]
async fn arriving_spends_a_credit() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let player = Knock::arrive(&addr, Some(15)).await.expect("connect");
  settle().await;
  assert_eq!(credits(&player, 15), Some(STARTING_CREDITS - 1));
  player.leave();
}

#[tokio::test]
async fn an_account_with_no_credit_is_closed_on_arrival() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let first = Knock::arrive(&addr, Some(16)).await.expect("connect");
  settle().await;
  first.say(&[ArcadeOp::InsertCoin, ArcadeOp::InsertCoin]).await.expect("send");
  settle().await;
  assert_eq!(credits(&first, 16), Some(0));
  first.leave();
  settle().await;

  let again = Knock::arrive(&addr, Some(16)).await.expect("connect");
  settle().await;
  let goodbye = again.goodbye().expect("a spent account played for free");
  assert_eq!(goodbye.code, CREDIT_SPENT);
  assert_eq!(goodbye.detail.as_deref(), Some(&b"no credit left"[..]));
  assert!(!again.was_admitted(), "a seat was given before the wallet was read");
  again.leave();
}

#[tokio::test]
async fn the_rooms_clock_counts_down() {
  let (session, _door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();
  let player = Knock::arrive(&addr, Some(13)).await.expect("connect");
  settle().await;
  let early = seconds_left(&player, 13).expect("a snapshot");
  tokio::time::sleep(Duration::from_secs(3)).await;
  player.say(&[ArcadeOp::Push]).await.expect("send");
  settle().await;
  let later = seconds_left(&player, 13).expect("a snapshot");
  assert!(later + 2 <= early, "{early}s then {later}s after three seconds");
  player.leave();
}

#[tokio::test]
async fn every_seat_is_scarce() {
  let (session, door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();

  let mut held = Vec::new();
  for account in 1..=SEATS as u32 {
    held.push(Knock::arrive(&addr, Some(account)).await.expect("connect"));
    settle().await;
  }
  assert_eq!(door.seated(), SEATS, "the room did not fill");

  let over = Knock::arrive(&addr, Some(90)).await.expect("connect");
  settle().await;
  assert_eq!(over.refusal(), Some(Refusal::OverCapacity), "a fourth seat appeared");
  over.leave();
  for k in held {
    k.leave();
  }
}
