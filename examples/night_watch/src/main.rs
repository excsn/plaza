//! A village with a wolf in it: the scripted run.
//!
//! What the other examples cannot show, and this one is for:
//!
//! - `Phased` where the phase is the **rule set**: at night one role may act,
//!   by day everyone may, and the same op is legal or refused depending on
//!   where the sun is.
//! - `SequentialRoundManager::new(None, ..)`, the unbounded mode, driven for
//!   the first time: the game ends when a side wins, never on a count.
//! - Collect-then-resolve: ballots are gathered all day and nothing happens
//!   until dusk, when they resolve at once.
//! - The epoch guarding a phase deadline: a day that closes early because
//!   everyone voted leaves its deadline scheduled, and the deadline discovers
//!   it is stale rather than firing into the night.
//! - Per-recipient secrecy with an inversion: each player sees only their own
//!   role, and **the dead see everything**.
//!
//! To play it yourself, `cargo run -p plaza_example_night_watch --bin serve`
//! and open five browser tabs.

use plaza_example_night_watch::logic::VillageLogic;
use plaza_example_night_watch::snapshot::VillageSnapshotter;
use plaza_example_night_watch::types::{PlayerId, VillageOp, VillageState};

use plaza::{
  agent::Agent,
  controller::{ControllerCommand, StateControllerBuilder},
  session::in_process::ClientInbox,
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

type VillageSession = InProcessSession<VillageOp, PlayerId>;

const TICK: Duration = Duration::from_millis(20);

/// Logs what one villager receives. Run it for the wolf and for a victim and
/// the difference between their snapshots is the example.
fn spawn_listener(name: &'static str, inbox: ClientInbox<VillageOp, PlayerId>) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          VillageOp::Snapshot(view) => {
            if let Some(all) = &view.everyone {
              info!("[{name}] as one of the dead, I can see everything: {all:?}");
            } else if let Some(role) = view.your_role {
              info!("[{name}] I know only my own role: {role:?}");
            }
          }
          VillageOp::PhaseChanged(p) => info!("[{name}] phase: {:?} ({:?})", p.new_phase, p.reason),
          VillageOp::Dawn { victim, role } => info!("[{name}] dawn: {victim} was taken, a {role:?}"),
          VillageOp::VotesTallied { counts, exiled } => info!("[{name}] dusk: {counts:?}, exiled {exiled:?}"),
          VillageOp::Refused(why) => info!("[{name}] refused: {why:?}"),
          VillageOp::GameOver { winner, roles } => info!("[{name}] {winner:?} wins; the reveal: {roles:?}"),
          _ => {}
        }
      }
    }
  })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse()?)
        .add_directive("plaza_example_night_watch=debug".parse()?),
    )
    .init();

  info!("Plaza Night Watch Example - Starting");

  let session = VillageSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(VillageLogic),
    session.clone(),
    Arc::new(VillageSnapshotter),
    VillageState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let tick_tx = controller_tx.clone();
  let ticker = tokio::spawn(async move { TickDriver::new(TICK).run(tick_tx).await });

  // Five villagers. The first to be seated is the first game's wolf, which the
  // village knows and they do not.
  let names = ["Ada", "Bea", "Cal", "Dee", "Eve"];
  let mut tasks = Vec::new();
  info!("--- villagers arriving; the deal happens at five");
  for (i, name) in names.iter().enumerate() {
    let agent = Agent::new_human(i as PlayerId + 1);
    let (_conn, inbox) = session.connect(agent).await?;
    tasks.push(spawn_listener(name, inbox));
  }
  settle().await;

  info!("--- night 1: Bea (a villager) tries to hunt and is refused");
  send(&session, 2, VillageOp::Hunt(3)).await;

  info!("--- night 1: Ada, the wolf, takes Cal");
  send(&session, 1, VillageOp::Hunt(3)).await;

  info!("--- day 1: the living vote, and dusk falls early once all four are in");
  send(&session, 2, VillageOp::Vote(4)).await;
  send(&session, 4, VillageOp::Vote(2)).await;
  send(&session, 5, VillageOp::Vote(4)).await;
  send(&session, 1, VillageOp::Vote(4)).await;
  settle().await;

  info!("--- night 2: the wolf oversleeps, and the night chooses for it");
  tokio::time::sleep(TICK * 45).await;
  settle().await;

  info!("--- the wolf reached parity; the reveal is up, then the village deals again");
  tokio::time::sleep(TICK * 260).await;
  settle().await;

  info!("--- shutting down");
  controller_tx.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  for task in tasks {
    task.abort();
  }
  info!("Night Watch Example - Finished.");
  Ok(())
}

async fn send(session: &VillageSession, who: PlayerId, op: VillageOp) {
  session.client_send(Agent::new_human(who), vec![op]).await;
  settle().await;
}

async fn settle() {
  tokio::time::sleep(TICK * 2).await;
}
