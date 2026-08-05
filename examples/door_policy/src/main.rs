//! What it costs a server to say no, now that it is allowed to.
//!
//! `cargo run -p plaza_example_door_policy`

use std::sync::atomic::Ordering;
use std::time::Duration;

use tracing::Level;

use plaza_example_door_policy::arcade;
use plaza_example_door_policy::client::Knock;
use plaza_example_door_policy::types::{DuplicateLogin, CREDIT_SECS, PER_IP, SEATS};

#[tokio::main]
async fn main() {
  tracing_subscriber::fmt().with_max_level(Level::WARN).init();
  println!("# door_policy\n");
  println!("Seats {SEATS}, per-address cap {PER_IP}, a credit buys {CREDIT_SECS}s.\n");
  scenario_refusals().await;
  scenario_duplicate(DuplicateLogin::RefuseNewest).await;
  scenario_duplicate(DuplicateLogin::KickOldest).await;
}

/// Fills the room, then knocks with everything the door can refuse.
async fn scenario_refusals() {
  let (session, door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = session.local_addr().to_string();

  // Sockets that never say who they are. Nothing about them is judgeable by a
  // rule keyed on an account, which is what makes the address cap the only one
  // a door can apply before identity exists.
  let mut inside = Vec::new();
  for _ in 0..PER_IP {
    inside.push(Knock::arrive(&addr, None).await.expect("connect"));
    tokio::time::sleep(Duration::from_millis(80)).await;
  }
  let capped = Knock::arrive(&addr, None).await.expect("connect");
  tokio::time::sleep(Duration::from_millis(150)).await;

  // A fresh door, so the ban is what refuses rather than the address cap: the
  // rules fire in the order they become knowable, and the socket is first.
  let (ban_session, ban_door) = arcade(DuplicateLogin::RefuseNewest).await;
  ban_door.ban(99);
  let banned = Knock::arrive(&ban_session.local_addr().to_string(), Some(99)).await;
  tokio::time::sleep(Duration::from_millis(300)).await;
  let ban_cost = ban_door.ledger.registered.load(Ordering::Relaxed);

  println!("## refusals\n");
  println!("| knock | refused | where the rule could be judged |");
  println!("|---|---|---|");
  println!(
    "| one connection past the address cap | {} | the socket, before anything exists |",
    capped.refusal().map(|r| r.as_str()).unwrap_or("no")
  );
  if let Ok(banned) = &banned {
    println!(
      "| a banned account | {} | only after identity arrived |",
      banned.refusal().map(|r| r.as_str()).unwrap_or("no")
    );
  }
  println!(
    "\nThe ban cost {} registration(s) before it could be applied, because the door never sees an account.",
    ban_cost
  );

  println!("\n## what the refusals cost\n");
  println!("| | count |");
  println!("|---|---|");
  println!("| refusals total | {} |", door.ledger.total() + ban_door.ledger.total());
  println!(
    "| refused by the factory, per the transport's own counter | {} |",
    session.manager().stats().refused()
  );
  println!(
    "| connections registered in total | {} |",
    door.ledger.registered.load(Ordering::Relaxed) + ban_cost
  );
  println!(
    "| reasons sent ahead of a close | {} |",
    door.ledger.reasons_sent.load(Ordering::Relaxed) + ban_door.ledger.reasons_sent.load(Ordering::Relaxed)
  );
  println!(
    "| ops accepted after a close | {} |",
    door.ledger.ops_after_close.load(Ordering::Relaxed)
  );
  println!();

  capped.leave();
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
