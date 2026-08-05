//! Every claim in the entry, as a count.
//!
//! Same claims as before the library grew the primitives; the party no longer
//! brings its own transport to make them true. Farewell delivery is asserted
//! from the client's side throughout, which is where delivery is real.

use std::sync::atomic::Ordering;
use std::time::Duration;

use plaza_example_table_manners::client::Guest;
use plaza_example_table_manners::party;
use plaza_example_table_manners::types::{Parting, PartyOp, AFK_SECS, FLOOD_OPS};

async fn settle() {
  tokio::time::sleep(Duration::from_millis(300)).await;
}

/// A long AFK timeout, for tests that are not about AFK.
fn patient() -> Duration {
  Duration::from_secs(600)
}

#[tokio::test]
async fn a_kick_says_why_and_the_reason_wins_the_race() {
  let (session, host) = party(patient()).await;
  let addr = session.local_addr().to_string();
  let host_guest = Guest::arrive(&addr, Some(0)).await.expect("connect");
  let victim = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;

  host_guest.say(&[PartyOp::Kick { seat: 1 }]).await.expect("kick");
  settle().await;

  assert_eq!(victim.farewell(), Some(Parting::Kicked), "the victim was never told why");
  assert_eq!(
    host.meters.reasons_sent.load(Ordering::Relaxed),
    host.meters.kicks.load(Ordering::Relaxed),
    "every kick carried its reason"
  );
  host_guest.leave();
  victim.leave();
}

#[tokio::test]
async fn a_kick_is_not_a_netdrop() {
  let (session, host) = party(patient()).await;
  let addr = session.local_addr().to_string();
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
  let (session, host) = party(Duration::from_secs(AFK_SECS)).await;
  let addr = session.local_addr().to_string();
  let quiet = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;
  assert!(quiet.was_seated());

  // Say nothing at all. The guest answers every probe, so the link is alive
  // and measured the whole time; the seat is still silent.
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
  let (session, host) = party(Duration::from_secs(AFK_SECS)).await;
  let addr = session.local_addr().to_string();
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
async fn a_flood_gets_the_flooder_removed_and_nobody_else() {
  let (session, host) = party(patient()).await;
  let addr = session.local_addr().to_string();
  let bystander = Guest::arrive(&addr, Some(0)).await.expect("connect");
  let griefer = Guest::arrive(&addr, Some(1)).await.expect("connect");
  settle().await;

  // Session-wide, so any drop here is an op somebody lost.
  let dropped_before = session.manager().stats().inbound_dropped();

  for _ in 0..(FLOOD_OPS * 3) {
    let _ = griefer.say(&[PartyOp::Say("spam".into())]).await;
  }
  settle().await;

  assert_eq!(
    griefer.farewell(),
    Some(Parting::Flooding),
    "the flooder was not the one removed"
  );
  assert_eq!(host.meters.flood_removals.load(Ordering::Relaxed), 1);
  assert_eq!(
    session.manager().stats().inbound_dropped(),
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
  let (session, host) = party(patient()).await;
  let addr = session.local_addr().to_string();
  let mut guests = Vec::new();
  for seat in 0..3u32 {
    guests.push(Guest::arrive(&addr, Some(seat)).await.expect("connect"));
    settle().await;
  }

  host.drain(Parting::Drained);
  settle().await;

  for guest in &guests {
    assert_eq!(
      guest.farewell(),
      Some(Parting::Drained),
      "somebody's goodbye was cut off by the close"
    );
  }
  assert_eq!(
    host.meters.reasons_sent.load(Ordering::Relaxed) as usize,
    guests.len(),
    "farewells sent must equal the number drained"
  );
  assert_eq!(host.meters.drained.load(Ordering::Relaxed) as usize, guests.len());

  for g in guests {
    g.leave();
  }
}
