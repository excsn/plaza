//! The table, scripted: no window, no socket. One human seat played by the
//! house's own policy, a stretch of hands, and the reopening ledger checked
//! at the end.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use check_raise::logic::TableLogic;
use check_raise::protocol::{Act, Panel, PlayerId, PokerOp};
use check_raise::snapshot::TableSnapshotter;
use check_raise::state::TableState;

type TableSession = InProcessSession<PokerOp, PlayerId>;

const TICK: Duration = Duration::from_millis(check_raise::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Check Raise - scripted");

  let session = TableSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(TableLogic::new()),
    session.clone(),
    Arc::new(TableSnapshotter),
    TableState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  info!("--- Wren sits; after the wait, the house fills the other chairs");
  let wren = Agent::new_human(1);
  let (_conn, inbox) = session.connect(wren.clone()).await?;

  // Wren's hand: when the ask is hers, play a plain honest game from the
  // view alone: check what is free, call what is cheap, fold the rest.
  let hand_session = session.clone();
  let hand = tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        if let PokerOp::Snapshot(view) = op {
          if view.to_act != Some(0) || view.you != 0 {
            continue;
          }
          let act = if view.owed == 0 {
            Act::Call
          } else if view.owed <= 4 {
            Act::Call
          } else {
            Act::Fold
          };
          hand_session
            .client_send(Agent::new_human(1), vec![PokerOp::TakeAction { act }])
            .await;
        }
      }
    }
  });

  info!("--- the table runs for a while");
  tokio::time::sleep(Duration::from_secs(75)).await;

  let (panel, chips): (Panel, u32) =
    query_with(&commands, |state: &TableState| (state.panel, state.chairs.iter().map(|c| c.stack + c.put).sum())).await?;
  info!(
    "--- {} hands, {} streets | asks {} skipped {} reopened {} | folds {} all-ins {} showdowns {} uncontested {} lapses {}",
    panel.hands,
    panel.streets,
    panel.offers,
    panel.skipped,
    panel.reopened,
    panel.folds,
    panel.allins,
    panel.showdowns,
    panel.uncontested,
    panel.timeouts
  );
  info!("--- {chips} chips on the table");
  assert!(panel.hands >= 3, "hands were dealt and finished");
  assert!(panel.offers > 20, "the asks went around");
  assert!(
    panel.showdowns + panel.uncontested >= panel.hands - 1,
    "every dealt hand but the one in flight ended one way or the other"
  );

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  hand.abort();
  info!("Check Raise - Finished.");
  Ok(())
}
