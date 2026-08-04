//! What it costs a server to be allowed to say no.
//!
//! `cargo run -p plaza_example_door_policy`

use std::sync::Arc;
use std::time::Duration;

use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_session::SessionOptions;
use tracing::Level;

use plaza_example_door_policy::client::Knock;
use plaza_example_door_policy::door::Door;
use plaza_example_door_policy::logic::{ArcadeLogic, ArcadeState};
use plaza_example_door_policy::snapshot::RoomSnapshotter;
use plaza_example_door_policy::transport::{deadline_task, Doorman};
use plaza_example_door_policy::types::{DuplicateLogin, CREDIT_SECS, PER_IP, SEATS};

async fn arcade(policy: DuplicateLogin) -> (Arc<Doorman>, Arc<Door>) {
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
  (doorman, door)
}

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
  let (doorman, door) = arcade(DuplicateLogin::RefuseNewest).await;
  let addr = doorman.bound.to_string();

  // Sockets that never say who they are. Nothing about them is judgeable by a
  // rule keyed on an account, which is what makes the address cap the only one
  // a door could apply before identity exists.
  let mut inside = Vec::new();
  for _ in 0..PER_IP {
    inside.push(Knock::arrive(&addr, None).await.expect("connect"));
    tokio::time::sleep(Duration::from_millis(80)).await;
  }
  let capped = Knock::arrive(&addr, None).await.expect("connect");
  tokio::time::sleep(Duration::from_millis(150)).await;

  // A fresh door, so the ban is what refuses rather than the address cap: the
  // rules fire in the order they become knowable, and the socket is first.
  let (ban_door, ban_ledger) = arcade(DuplicateLogin::RefuseNewest).await;
  ban_ledger.ban(99);
  let banned = Knock::arrive(&ban_door.bound.to_string(), Some(99)).await;
  tokio::time::sleep(Duration::from_millis(300)).await;
  let ban_cost = ban_ledger
    .ledger
    .registers_wasted
    .load(std::sync::atomic::Ordering::Relaxed);

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

  let ledger = &door.ledger;
  println!("\n## what the refusals cost\n");
  println!("| | count |");
  println!("|---|---|");
  println!("| refusals total | {} |", ledger.total());
  println!(
    "| connections registered in total | {} |",
    ledger.registers_wasted.load(std::sync::atomic::Ordering::Relaxed)
  );
  println!(
    "| reasons that reached the client | {} |",
    ledger.reasons_delivered.load(std::sync::atomic::Ordering::Relaxed)
  );
  println!(
    "| silent closes | {} |",
    ledger.silent_closes.load(std::sync::atomic::Ordering::Relaxed)
  );
  println!(
    "| ops accepted after a close | {} |",
    ledger.ops_after_close.load(std::sync::atomic::Ordering::Relaxed)
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
  let (doorman, _door) = arcade(policy).await;
  let addr = doorman.bound.to_string();

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
  let _ = doorman;
}
