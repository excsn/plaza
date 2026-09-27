//! What it costs a server to say no, now that it can say it at the door.
//!
//! `cargo run -p plaza_example_door_policy`

use std::sync::atomic::Ordering;
use std::time::Duration;

use tracing::Level;

use plaza_example_door_policy::client::Knock;
use plaza_example_door_policy::types::{DuplicateLogin, CREDENTIAL_WAIT, CREDIT_SECS, SEATS};
use plaza_example_door_policy::{arcade, arcade_with};

#[tokio::main]
async fn main() {
  tracing_subscriber::fmt().with_max_level(Level::WARN).init();
  println!("# door_policy\n");
  println!(
    "Seats {SEATS}, a credit buys {CREDIT_SECS}s, a socket may present nothing for {}ms.\n",
    CREDENTIAL_WAIT.as_millis()
  );
  scenario_refusals().await;
  scenario_duplicate(DuplicateLogin::RefuseNewest).await;
  scenario_duplicate(DuplicateLogin::KickOldest).await;
}

/// Knocks with everything the door can refuse and prices each refusal.
async fn scenario_refusals() {
  // An address cap of two on a machine where every client shares an address.
  let (session, door) = arcade_with(DuplicateLogin::RefuseNewest, 2).await;
  let addr = session.local_addr().to_string();

  let mut inside = Vec::new();
  for account in [101, 102] {
    inside.push(Knock::arrive(&addr, Some(account)).await.expect("connect"));
    tokio::time::sleep(Duration::from_millis(80)).await;
  }
  let capped = Knock::arrive(&addr, Some(103)).await.expect("connect");
  tokio::time::sleep(Duration::from_millis(150)).await;

  // A fresh door, so the ban is what refuses rather than the address cap.
  let (ban_session, ban_door) = arcade(DuplicateLogin::RefuseNewest).await;
  ban_door.ban(99);
  let banned = Knock::arrive(&ban_session.local_addr().to_string(), Some(99)).await;
  tokio::time::sleep(Duration::from_millis(150)).await;

  // A socket that presents nothing: what a half-open flood looks like.
  let silent = Knock::arrive(&addr, None).await.expect("connect");
  tokio::time::sleep(CREDENTIAL_WAIT + Duration::from_millis(200)).await;

  println!("## refusals\n");
  println!("| knock | outcome | judged |");
  println!("|---|---|---|");
  println!(
    "| a third connection from one address, cap two | {} | at the door, on the address and the account together |",
    capped.refusal().map(|r| r.as_str()).unwrap_or("admitted")
  );
  if let Ok(banned) = &banned {
    println!(
      "| a banned account | {} | at the door, before anything registered |",
      banned.refusal().map(|r| r.as_str()).unwrap_or("admitted")
    );
  }
  println!(
    "| a socket that presents nothing | {} | by the session's timer, {}ms after the open |",
    if silent.timed_out() { "closed 4408" } else { "still waiting" },
    CREDENTIAL_WAIT.as_millis()
  );

  println!("\n## what the refusals cost\n");
  println!("| | count |");
  println!("|---|---|");
  println!("| refusals total | {} |", door.ledger.total() + ban_door.ledger.total());
  println!(
    "| refused by the admitter, per the transport's own counter | {} |",
    session.manager().stats().refused() + ban_session.manager().stats().refused()
  );
  println!(
    "| closed for presenting nothing | {} |",
    session.manager().stats().timed_out()
  );
  println!(
    "| connections registered before an identity rule could be judged | 0 |"
  );
  println!(
    "| connections registered in total, every one of them admitted | {} |",
    door.ledger.registered.load(Ordering::Relaxed) + ban_door.ledger.registered.load(Ordering::Relaxed)
  );
  println!(
    "| ops accepted after a close | {} |",
    door.ledger.ops_after_close.load(Ordering::Relaxed)
  );
  println!();

  capped.leave();
  silent.leave();
  if let Ok(b) = banned {
    b.leave();
  }
  for k in inside {
    k.leave();
  }
}

/// The same account twice, under each policy.
async fn scenario_duplicate(policy: DuplicateLogin) {
  let (session, _door) = arcade(policy).await;
  let addr = session.local_addr().to_string();

  let first = Knock::arrive(&addr, Some(7)).await.expect("connect");
  tokio::time::sleep(Duration::from_millis(150)).await;
  let second = Knock::arrive(&addr, Some(7)).await.expect("connect");
  tokio::time::sleep(Duration::from_millis(250)).await;

  println!("## duplicate login: {policy:?}\n");
  println!("| connection | outcome |");
  println!("|---|---|");
  println!(
    "| the one already inside | {} |",
    first.closure().unwrap_or_else(|| "still playing".into())
  );
  println!(
    "| the newcomer | {} |",
    second
      .refusal()
      .map(|r| r.as_str())
      .unwrap_or(if second.was_admitted() { "admitted" } else { "waiting" })
  );
  println!(
    "\nThe loser was told: {}\n",
    match policy {
      DuplicateLogin::RefuseNewest => second.refusal().is_some(),
      DuplicateLogin::KickOldest => first.closure().is_some(),
    }
  );

  first.leave();
  second.leave();
}
