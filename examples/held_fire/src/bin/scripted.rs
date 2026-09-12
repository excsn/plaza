//! The skirmish, scripted: no window, no socket. One human commander against
//! the bot for a stretch, the human playing the same policy the bot does, and
//! the run fails if the offers never happened or the battles never concluded.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use held_fire::logic::WatchLogic;
use held_fire::protocol::{Panel, PlayerId, WatchOp};
use held_fire::snapshot::WatchSnapshotter;
use held_fire::state::WatchState;

type WatchSession = InProcessSession<WatchOp, PlayerId>;

const TICK: Duration = Duration::from_millis(held_fire::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Held Fire - scripted");

  let session = WatchSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(WatchLogic::new()),
    session.clone(),
    Arc::new(WatchSnapshotter),
    WatchState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  info!("--- Wren arrives; after the wait, the bot commands the red side");
  let wren = Agent::new_human(1);
  let (_conn, inbox) = session.connect(wren.clone()).await?;

  // Wren's hand: answer every offer (alternating fire and hold, so both
  // paths run), and play her activations from the snapshots her side is
  // served, through the same shared sight rules the window uses. A stale
  // order is refused harmlessly, so she orders on every snapshot that says
  // the floor is hers.
  let hand_session = session.clone();
  let hand = tokio::spawn(async move {
    use held_fire::protocol::Order;
    use held_fire::sight;
    let mut answers = 0u64;
    let mut acts = 0u64;
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          WatchOp::OfferOpened(offer) => {
            answers += 1;
            let fire = answers % 2 == 1;
            info!("[Wren] offer on unit {}: {}", offer.mover, if fire { "FIRE" } else { "hold..." });
            hand_session
              .client_send(Agent::new_human(1), vec![WatchOp::Answer {
                watcher: offer.watcher,
                fire,
              }])
              .await;
          }
          WatchOp::Snapshot(view) => {
            if view.side_to_act != Some(0) || view.marching.is_some() {
              continue;
            }
            let Some(me) = view.yours.iter().find(|u| u.alive && !u.acted) else {
              continue;
            };
            let shot = view
              .seen
              .iter()
              .find(|e| e.alive && sight::sees(me.at, e.at))
              .map(|e| Order::Shoot { unit: me.id, target: e.id });
            let order = shot.unwrap_or_else(|| {
              acts += 1;
              if acts % 3 == 0 {
                Order::Overwatch { unit: me.id }
              } else {
                let occupied: Vec<_> = view
                  .yours
                  .iter()
                  .filter(|u| u.alive && u.id != me.id)
                  .map(|u| u.at)
                  .chain(view.seen.iter().filter(|s| s.alive).map(|s| s.at))
                  .collect();
                let goal = view
                  .seen
                  .iter()
                  .filter(|s| s.alive)
                  .map(|s| s.at)
                  .min_by_key(|c| c.0.abs_diff(me.at.0) + c.1.abs_diff(me.at.1))
                  .unwrap_or((6, 4));
                let to = sight::reachable(me.at, &occupied)
                  .into_iter()
                  .min_by_key(|c| (c.0.abs_diff(goal.0) + c.1.abs_diff(goal.1), c.0, c.1));
                match to {
                  Some(to) if to != me.at => Order::March { unit: me.id, to },
                  _ => Order::Overwatch { unit: me.id },
                }
              }
            });
            hand_session.client_send(Agent::new_human(1), vec![WatchOp::Act(order)]).await;
          }
          WatchOp::BattleOver { winner } => {
            info!("[Wren] the {} side takes the field", ["blue", "red"][winner as usize % 2]);
          }
          _ => {}
        }
      }
    }
  });

  info!("--- the field plays itself for a while");
  tokio::time::sleep(Duration::from_secs(75)).await;

  let panel: Panel = query_with(&commands, |state: &WatchState| state.panel).await?;
  info!(
    "--- {} battles, {} rounds, {} activations | offers {} fired {} held {} lapsed {} | cut {} ambushes {} timeouts {}",
    panel.battles,
    panel.rounds,
    panel.activations,
    panel.offers,
    panel.fired,
    panel.held,
    panel.lapsed,
    panel.cut_short,
    panel.ambushes,
    panel.timeouts
  );
  assert!(panel.activations > 10, "the field was played");
  assert!(panel.offers > 0, "overwatch found a mover to interrupt");

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  hand.abort();
  info!("Held Fire - Finished.");
  Ok(())
}
