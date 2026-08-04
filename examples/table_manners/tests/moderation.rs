//! Every claim in the entry, as a count.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_example_table_manners::client::Guest;
use plaza_example_table_manners::logic::{PartyLogic, PartyState};
use plaza_example_table_manners::moderation::Host;
use plaza_example_table_manners::snapshot::TableSnapshotter;
use plaza_example_table_manners::transport::{steward, Doorman};
use plaza_example_table_manners::types::{Parting, PartyOp, AFK_SECS, FLOOD_OPS};
use plaza_session::SessionOptions;

async fn party(afk: Duration) -> (Arc<Doorman>, Arc<Host>, String) {
  let host = Host::new();
  let doorman = Doorman::bind("127.0.0.1:0", host.clone(), SessionOptions::default())
    .await
    .expect("bind");
  let (closes_tx, mut closes_rx) = tokio::sync::mpsc::unbounded_channel();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(PartyLogic {
      host: host.clone(),
      closes: closes_tx,
      host_key: Default::default(),
    }),
    doorman.session.clone(),
    Arc::new(TableSnapshotter { host: host.clone() }),
    PartyState::default(),
  )
  .build();
  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::new(Duration::from_millis(50)).run(tx));
  tokio::spawn(steward(doorman.clone(), afk));
  {
    let doorman = doorman.clone();
    tokio::spawn(async move {
      while let Some((conn_id, reason, detail)) = closes_rx.recv().await {
        doorman.close(conn_id, reason, detail);
      }
    });
  }
  let addr = doorman.bound.to_string();
  (doorman, host, addr)
}

async fn settle() {
  tokio::time::sleep(Duration::from_millis(300)).await;
}

/// A long AFK timeout, for tests that are not about AFK.
fn patient() -> Duration {
  Duration::from_secs(600)
}

#[tokio::test]
async fn a_kick_says_why_and_the_reason_wins_the_race() {
  let (doorman, host, addr) = party(patient()).await;
  let host_guest = Guest::arrive(&addr, Some(0)).await.expect("connect");
  let victim = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;

  host_guest.say(&[PartyOp::Kick { seat: 1 }]).await.expect("kick");
  settle().await;

  assert_eq!(victim.farewell(), Some(Parting::Kicked), "the victim was never told why");
  assert_eq!(
    host.meters.reasons_delivered.load(Ordering::Relaxed),
    host.meters.kicks.load(Ordering::Relaxed),
    "delivered reasons must equal kicks"
  );
  assert_eq!(host.meters.silent_closes.load(Ordering::Relaxed), 0);
  let _ = doorman;
  host_guest.leave();
  victim.leave();
}

#[tokio::test]
async fn a_kick_is_not_a_netdrop() {
  let (_doorman, host, addr) = party(patient()).await;
  let host_guest = Guest::arrive(&addr, Some(0)).await.expect("connect");
  let dropper = Guest::arrive(&addr, Some(1)).await.expect("connect");
  let kicked = Guest::arrive(&addr, Some(2)).await.expect("connect");
  settle().await;

  // One leaves by having its socket cut, the other by being removed.
  dropper.yank();
  settle().await;
  host_guest.say(&[PartyOp::Kick { seat: 2 }]).await.expect("kick");
  settle().await;

  assert!(host.is_held(1), "a drop must keep the seat warm");
  assert!(!host.is_held(2), "a kick must not");
  assert_eq!(host.meters.seats_held.load(Ordering::Relaxed), 1);
  assert_eq!(host.meters.seats_cleared.load(Ordering::Relaxed), 1);

  // Coming back to a seat that was taken away is refused at the door.
  let rejoin = Guest::arrive(&addr, Some(2)).await.expect("connect");
  settle().await;
  assert!(!rejoin.was_seated(), "a kicked seat was handed back");
  assert_eq!(host.meters.rejoins_refused.load(Ordering::Relaxed), 1);

  host_guest.leave();
  kicked.leave();
  rejoin.leave();
}

#[tokio::test]
async fn silence_removes_you_and_probes_do_not_count_as_talking() {
  let (_doorman, host, addr) = party(Duration::from_secs(AFK_SECS)).await;
  let quiet = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;
  assert!(quiet.was_seated());

  // Say nothing at all. Probes still cross the link both ways.
  tokio::time::sleep(Duration::from_secs(AFK_SECS + 1)).await;

  assert_eq!(
    quiet.farewell(),
    Some(Parting::Afk),
    "an idle guest was not removed, so probes were counted as activity"
  );
  assert_eq!(host.meters.afk_removals.load(Ordering::Relaxed), 1);
  quiet.leave();
}

#[tokio::test]
async fn talking_resets_the_clock() {
  let (_doorman, host, addr) = party(Duration::from_secs(AFK_SECS)).await;
  let chatty = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;

  // Keep talking across more than the timeout.
  for _ in 0..((AFK_SECS + 1) * 2) {
    chatty.say(&[PartyOp::Say("still here".into())]).await.expect("say");
    tokio::time::sleep(Duration::from_millis(500)).await;
  }

  assert_eq!(chatty.farewell(), None, "a talking guest was removed anyway");
  assert_eq!(host.meters.afk_removals.load(Ordering::Relaxed), 0);
  chatty.leave();
}

#[tokio::test]
async fn a_flood_degrades_the_flooder_before_anyone_else() {
  let (doorman, host, addr) = party(patient()).await;
  let bystander = Guest::arrive(&addr, Some(0)).await.expect("connect");
  let griefer = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;

  // Session-wide, so any drop here is an op somebody lost. The flooder's own
  // ops never reach it: they are shed on the connection that sent them.
  let dropped_before = doorman.manager().stats().inbound_dropped();

  for _ in 0..(FLOOD_OPS * 3) {
    let _ = griefer.say(&[PartyOp::Say("spam".into())]).await;
  }
  settle().await;

  assert!(
    host.meters.flooder_shed.load(Ordering::Relaxed) > 0,
    "the flood was absorbed rather than shed on the flooder"
  );
  assert_eq!(
    doorman.manager().stats().inbound_dropped(),
    dropped_before,
    "the shared inbound queue lost an op while one connection flooded"
  );

  // The bystander is still at the table and still being told about it.
  bystander.say(&[PartyOp::Say("hello".into())]).await.expect("say");
  settle().await;
  assert_eq!(bystander.farewell(), None, "a bystander was removed by someone else's flood");
  assert!(bystander.table().is_some(), "a bystander stopped receiving the table");

  bystander.leave();
  griefer.leave();
}

#[tokio::test]
async fn draining_tells_everyone_before_closing_them() {
  let (doorman, host, addr) = party(patient()).await;
  let mut guests = Vec::new();
  for seat in 0..3u32 {
    guests.push(Guest::arrive(&addr, Some(seat)).await.expect("connect"));
    settle().await;
  }

  doorman.drain(Parting::Drained);
  settle().await;

  for guest in &guests {
    assert_eq!(
      guest.farewell(),
      Some(Parting::Drained),
      "somebody's goodbye was cut off by the close"
    );
  }
  assert_eq!(
    host.meters.reasons_delivered.load(Ordering::Relaxed) as usize,
    guests.len(),
    "delivered farewells must equal the number drained"
  );
  assert_eq!(host.meters.silent_closes.load(Ordering::Relaxed), 0);

  for g in guests {
    g.leave();
  }
}
