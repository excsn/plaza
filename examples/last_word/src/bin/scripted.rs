//! The duel, scripted: no window, no socket. One human seat played by the
//! bot's own policy, a stretch of duels, and the window accounting checked at
//! the end.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use last_word::logic::WordLogic;
use last_word::protocol::{DuelOp, Panel, PlayerId};
use last_word::snapshot::WordSnapshotter;
use last_word::state::WordState;

type WordSession = InProcessSession<DuelOp, PlayerId>;

const TICK: Duration = Duration::from_millis(last_word::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Last Word - scripted");

  let session = WordSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(WordLogic::new()),
    session.clone(),
    Arc::new(WordSnapshotter),
    WordState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  info!("--- Wren arrives; after the wait, the bot takes the other seat");
  let wren = Agent::new_human(1);
  let (_conn, inbox) = session.connect(wren.clone()).await?;

  // Wren's hand: on every snapshot that says the window is hers, speak with
  // the same policy the bot runs, or pass. Duplicates are refused harmlessly.
  let hand_session = session.clone();
  let hand = tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          DuelOp::Snapshot(view) => {
            if view.priority != Some(0) {
              continue;
            }
            // Rebuild just enough state for the shared policy to read.
            let mut probe = WordState::new();
            probe.duel = view.duel;
            probe.turn = view.turn;
            probe.active = view.active;
            probe.priority = view.priority;
            probe.tempo = view.tempo;
            probe.life = view.life;
            probe.stack = view.stack.clone();
            probe.panel = view.panel;
            let op = match last_word::logic::bot_choice(&probe, 0) {
              Some(spell) => DuelOp::Cast { spell },
              None => DuelOp::Pass,
            };
            hand_session.client_send(Agent::new_human(1), vec![op]).await;
          }
          DuelOp::DuelOver { winner } => {
            info!("[Wren] {} has the last word", ["blue", "red"][winner as usize % 2]);
          }
          _ => {}
        }
      }
    }
  });

  info!("--- the hall runs for a while");
  tokio::time::sleep(Duration::from_secs(60)).await;

  let panel: Panel = query_with(&commands, |state: &WordState| state.panel).await?;
  info!(
    "--- {} duels, {} turns | casts {} resolutions {} countered {} | windows {} deepest {} lapses {}",
    panel.duels, panel.turns, panel.casts, panel.resolutions, panel.countered, panel.windows, panel.max_depth, panel.timeouts
  );
  assert!(panel.duels >= 2, "duels concluded and dealt again");
  assert!(panel.casts > 10, "spells were spoken");
  assert!(
    panel.windows >= panel.casts + panel.resolutions,
    "every cast and every resolution opened a window"
  );

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  hand.abort();
  info!("Last Word - Finished.");
  Ok(())
}
