//! The battle, scripted: no window, no socket. One human commander against the
//! bot, a stretch under each regime, and the projection audit running the
//! whole time through the same [`OrderMirror`] the windows use. The run fails
//! loudly if the mirror ever disagrees with a turn the server opened.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use turn_gauge::logic::{auto_move, GaugeLogic};
use turn_gauge::mirror::{Observed, OrderMirror};
use turn_gauge::protocol::{GaugeOp, Panel, PlayerId, Regime};
use turn_gauge::snapshot::GaugeSnapshotter;
use turn_gauge::state::GaugeState;

type GaugeSession = InProcessSession<GaugeOp, PlayerId>;

const TICK: Duration = Duration::from_millis(turn_gauge::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Turn Gauge - scripted");

  let session = GaugeSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(GaugeLogic::new()),
    session.clone(),
    Arc::new(GaugeSnapshotter),
    GaugeState::new(),
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

  // Wren's hand: the mirror audits every op, and when a turn opens on a unit
  // of team 0, the same policy the bot runs answers for her. The counters are
  // shared atomics because the inbox never closes while the session Arc lives
  // in this task, so it is aborted rather than joined.
  let audit_checked = Arc::new(std::sync::atomic::AtomicU64::new(0));
  let audit_diverged = Arc::new(std::sync::atomic::AtomicU64::new(0));
  let hand_session = session.clone();
  let hand = tokio::spawn({
    let (checked, diverged) = (audit_checked.clone(), audit_diverged.clone());
    async move {
      let mut mirror = OrderMirror::new();
      let mut turn_seed = 0u64;
      while let Ok(msg) = inbox.recv().await {
        for op in msg.ops {
          match mirror.observe(&op) {
            Observed::Agreed(unit) => {
              if mirror.units.iter().any(|u| u.id == unit && u.team == 0) {
                turn_seed += 1;
                let mv = auto_move(&mirror.units, unit, turn_gauge::order::rng(turn_seed));
                hand_session
                  .client_send(Agent::new_human(1), vec![GaugeOp::Act { unit, mv }])
                  .await;
              }
            }
            Observed::Diverged { server, mine } => {
              error!("PROJECTION MISSED: server {server:?}, mine {mine:?}");
              std::process::exit(1);
            }
            Observed::Noted => {}
          }
          if let GaugeOp::BattleEnded { victor, series, series_over } = op {
            let side = ["blue", "red"][victor as usize % 2];
            if series_over {
              info!("[Wren] {side} takes the series {}-{}", series[0], series[1]);
            } else {
              info!("[Wren] {side} takes it, {}-{}", series[0], series[1]);
            }
          }
        }
        checked.store(mirror.checked, std::sync::atomic::Ordering::Relaxed);
        diverged.store(mirror.diverged, std::sync::atomic::Ordering::Relaxed);
      }
    }
  });

  info!("--- a stretch under initiative");
  tokio::time::sleep(Duration::from_secs(14)).await;

  info!("--- the dial turns to the delay queue; the battle deals again");
  session.client_send(wren.clone(), vec![GaugeOp::SetRegime(Regime::Ctb)]).await;
  tokio::time::sleep(Duration::from_secs(14)).await;

  let (panel, regime): (Panel, Regime) = query_with(&commands, |state: &GaugeState| (state.panel, state.regime)).await?;
  info!(
    "--- {} battles, {} rounds, {} turns, {} chairs timed out, ending under {:?}",
    panel.battles, panel.rounds, panel.turns, panel.timeouts, regime
  );
  assert!(panel.battles >= 2, "both regimes should have concluded a battle");
  assert!(panel.turns > 40, "the battles were played, not idled");

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  hand.abort();
  let checked = audit_checked.load(std::sync::atomic::Ordering::Relaxed);
  let diverged = audit_diverged.load(std::sync::atomic::Ordering::Relaxed);
  info!("--- the audit: {checked} turns checked, {diverged} diverged");
  assert!(checked > 40, "the audit saw the battles");
  assert_eq!(diverged, 0, "the projection must never miss");
  info!("Turn Gauge - Finished.");
  Ok(())
}
