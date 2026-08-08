//! A snake draft, written to find out whether `TurnManager` is a seam.
//!
//! `RoundRobinTurnManager` had been the trait's only implementation, so nothing
//! had ever tested whether the trait describes a category or describes its one
//! member. [`SnakeTurnManager`](plaza_example_draft_board::snake) is the second,
//! and its module records what fit and what did not.
//!
//! The short version: both trait methods carried a reversing order without
//! complaint, including the part that looks illegal, where the next actor is the
//! *same* one that just played. What did not carry is everything around them.
//! `begin`, `restart`, `add_actor` and `remove_actor` live on the concrete
//! round-robin type and not on the trait, so this manager had to declare its own
//! and nothing checks that the two agree.
//!
//! This binary is the scripted run. To draft by hand, `cargo run -p
//! plaza_example_draft_board --bin serve` and open three browser tabs.

use plaza_example_draft_board::logic::DraftLogic;
use plaza_example_draft_board::snapshot::BoardSnapshotter;
use plaza_example_draft_board::types::{DraftOp, DraftState, PlayerId};

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

type BoardSession = InProcessSession<DraftOp, PlayerId>;

const TICK: Duration = Duration::from_millis(20);

/// Logs what one drafter receives. The board is public, so all three see the
/// same thing, which is the contrast with `card_table` worth noticing.
fn spawn_drafter_listener(name: &'static str, inbox: ClientInbox<DraftOp, PlayerId>) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          DraftOp::Taken {
            player,
            prospect,
            on_their_behalf,
          } => {
            let how = if on_their_behalf { " (clock ran out)" } else { "" };
            info!("[{name}] {player} took {prospect}{how}");
          }
          DraftOp::TurnChanged(p) => {
            if let Some(next) = p.new_turn_actor {
              info!("[{name}] on the clock: {next}");
            }
          }
          DraftOp::DraftOver { standings } => info!("[{name}] final: {standings:?}"),
          DraftOp::Refused(why) => info!("[{name}] refused: {why:?}"),
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
        .add_directive("plaza_example_draft_board=debug".parse()?),
    )
    .init();

  info!("Plaza Draft Board Example - Starting");

  let session = BoardSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(DraftLogic),
    session.clone(),
    Arc::new(BoardSnapshotter),
    DraftState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  // Ticks drive the pick clock. Without them nothing would ever time out.
  let tick_tx = controller_tx.clone();
  let ticker = tokio::spawn(async move { TickDriver::new(TICK).run(tick_tx).await });

  let ada = Agent::new_human(1);
  let bo = Agent::new_human(2);
  let cy = Agent::new_human(3);

  info!("--- drafters arriving; the board opens once three are seated");
  let (_a, ada_inbox) = session.connect(ada.clone()).await?;
  let ada_task = spawn_drafter_listener("Ada", ada_inbox);
  let (_b, bo_inbox) = session.connect(bo.clone()).await?;
  let bo_task = spawn_drafter_listener("Bo", bo_inbox);
  let (_c, cy_inbox) = session.connect(cy.clone()).await?;
  let cy_task = spawn_drafter_listener("Cy", cy_inbox);
  settle().await;

  // Pass one runs down the order. The pool is racked most valuable first, so
  // picking third is a real cost here, which is the thing the snake pays back.
  info!("--- pass 1: Ada, Bo, Cy");
  take(&session, &ada, 0).await;
  take(&session, &bo, 1).await;
  take(&session, &cy, 2).await;
  settle().await;

  // The reversal. Cy just picked last and now picks first, which a wrapping
  // manager cannot express: round-robin would hand the turn back to Ada.
  info!("--- pass 2 reverses: Cy picks again, then Bo, then Ada");
  take(&session, &cy, 3).await;
  take(&session, &bo, 4).await;
  take(&session, &ada, 5).await;
  settle().await;

  // Pass three reverses again, and Ada sits on the clock rather than picking,
  // so the board takes the best remaining prospect for her.
  info!("--- pass 3 reverses back: Ada stalls and the board picks for her");
  tokio::time::sleep(TICK * 40).await;
  take(&session, &bo, 7).await;
  take(&session, &cy, 8).await;
  settle().await;

  info!("--- shutting down");
  controller_tx.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  ada_task.abort();
  bo_task.abort();
  cy_task.abort();

  info!("Draft Board Example - Finished.");
  Ok(())
}

async fn take(session: &BoardSession, who: &Agent<PlayerId>, id: u8) {
  session.client_send(who.clone(), vec![DraftOp::Take(id)]).await;
  settle().await;
}

async fn settle() {
  tokio::time::sleep(TICK * 2).await;
}
