//! The duel, scripted: no window, no socket. Wren faces the bot for a few
//! contests, a false start and a clean draw both get ruled, and the mill's
//! numbers are printed at the end, cheat engaged, so the floored count moves.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use quick_draw::logic::DuelLogic;
use quick_draw::protocol::{Controls, DrawOp, PlayerId, Ruling, TICK_US};
use quick_draw::snapshot::DuelSnapshotter;
use quick_draw::state::DuelState;

type DuelSession = InProcessSession<DrawOp, PlayerId>;

const TICK: Duration = Duration::from_millis(quick_draw::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Quick Draw - scripted");

  let session = DuelSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(DuelLogic::new()),
    session.clone(),
    Arc::new(DuelSnapshotter),
    DuelState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  info!("--- Wren arrives; the bot takes the other seat at once");
  let wren = Agent::new_human(1);
  let (_conn, inbox) = session.connect(wren.clone()).await?;

  // Wren's hand: fires ~170ms after each signal, claiming the moment the
  // signal's own stamp names. The second contest jumps the gun on purpose.
  let hand_session = session.clone();
  let hand = tokio::spawn(async move {
    let mut contests_seen = 0u32;
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          DrawOp::Steady { contest } => {
            contests_seen += 1;
            if contests_seen == 2 {
              info!("[Wren] contest {contest}: too eager");
              tokio::time::sleep(Duration::from_millis(300)).await;
              hand_session.client_send(Agent::new_human(1), vec![DrawOp::Fire { tick: 0, offset_us: 0 }]).await;
            }
          }
          DrawOp::Signal { at_ms, .. } => {
            tokio::time::sleep(Duration::from_millis(170)).await;
            let claim_us = (at_ms + 170) * 1000;
            hand_session
              .client_send(Agent::new_human(1), vec![DrawOp::Fire {
                tick: claim_us / TICK_US,
                offset_us: (claim_us % TICK_US) as u32,
              }])
              .await;
          }
          DrawOp::Ruled(verdict) => {
            let winner = verdict
              .winner_subtick
              .map(|w| if w == quick_draw::protocol::BOT { "the bot".to_owned() } else { format!("P{w}") })
              .unwrap_or_else(|| "nobody".to_owned());
            info!("[Wren] contest {} ruled {:?}: {winner} takes it", verdict.contest, verdict.ruling);
            for shot in &verdict.shots {
              if let Some(us) = shot.reaction_us {
                info!("[Wren]   P{} at {}ms{}", shot.player, us / 1000, if shot.floored { " (floored)" } else { "" });
              }
            }
            if verdict.ruling == Ruling::FalseStart {
              info!("[Wren] that one is on me");
            }
          }
          _ => {}
        }
      }
    }
  });

  info!("--- the mill runs with A claiming 80ms early, so the floor has work");
  session
    .client_send(wren.clone(), vec![DrawOp::SetControls(Controls {
      a_claims_early_ms: 80,
      contests_per_sec: 200,
      ..Controls::default()
    })])
    .await;

  tokio::time::sleep(Duration::from_secs(14)).await;

  let (harness, live) = query_with(&commands, |state: &DuelState| (state.harness, state.live_contests)).await?;
  info!(
    "--- the mill: {} contests, {} same-tick, {} disagreed, A wins arrival {} vs declared {}, {} claims floored",
    harness.contests, harness.same_tick, harness.disagreed, harness.a_wins_arrival, harness.a_wins_subtick, harness.floored
  );
  info!("--- {live} live contests ruled");

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  hand.abort();
  info!("Quick Draw - Finished.");
  Ok(())
}
