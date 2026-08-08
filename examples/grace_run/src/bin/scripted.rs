//! The delve, scripted: no window, no socket. The whole session argument in
//! one arc: a duplicate suppressed, a seat held and resumed with its loot, a
//! key burned with the dedup off, and a window that ran out freeing the party.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use grace_run::logic::RunLogic;
use grace_run::protocol::{PlayerId, RunOp};
use grace_run::snapshot::RunSnapshotter;
use grace_run::state::RunState;

type RunSession = InProcessSession<RunOp, PlayerId>;

const TICK: Duration = Duration::from_millis(grace_run::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Grace Run - scripted");

  let session = RunSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(RunLogic),
    session.clone(),
    Arc::new(RunSnapshotter),
    RunState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  let wren = Agent::new_human(1);
  let roy = Agent::new_human(2);

  info!("--- Wren and Roy delve in; a two-second window so the arc fits a scripted run");
  let (_wc, _wi) = session.connect(wren.clone()).await?;
  let (roy_conn, _ri) = session.connect(roy.clone()).await?;
  send(&session, &wren, RunOp::SetGraceMs(2_000)).await;

  info!("--- room 1: coins, keys, and a resend that is recognised for what it is");
  send(&session, &wren, RunOp::GrabCoins { seq: 1 }).await;
  send(&session, &wren, RunOp::GrabKey { seq: 2 }).await;
  send(&session, &wren, RunOp::GrabKey { seq: 2 }).await;
  send(&session, &roy, RunOp::GrabKey { seq: 1 }).await;

  info!("--- Roy's link drops; his seat is held, keys and all");
  session.disconnect(&2, roy_conn).await;
  settle().await;

  info!("--- Wren opens the door; the party stands in it, waiting on the held seat");
  send(&session, &wren, RunOp::Unlock { seq: 3 }).await;
  tokio::time::sleep(Duration::from_millis(600)).await;

  info!("--- Roy returns inside the window: seat, key and all; the party walks on");
  let (roy_conn, _ri2) = session.connect(roy.clone()).await?;
  settle().await;

  info!("--- room 2, and the dedup goes off to show what it was worth");
  send(&session, &wren, RunOp::SetDedup(false)).await;
  send(&session, &wren, RunOp::GrabKey { seq: 4 }).await;
  send(&session, &wren, RunOp::GrabKey { seq: 5 }).await;
  session.disconnect(&2, roy_conn).await;
  settle().await;
  send(&session, &wren, RunOp::Unlock { seq: 6 }).await;
  send(&session, &wren, RunOp::Unlock { seq: 6 }).await;

  info!("--- this time Roy stays away; the window runs out and the run stops waiting");
  tokio::time::sleep(Duration::from_millis(2_600)).await;

  let (view, meters) = query_with(&commands, |state: &RunState| (state.view(), state.meters)).await?;
  info!(
    "--- room {}: {} resume(s), {} expiry(ies), {:.1}s waited, {} duplicate(s) suppressed, {} applied, {} key(s) burned",
    view.room,
    meters.resumes,
    meters.expiries,
    meters.waited_ms as f32 / 1000.0,
    meters.dups_suppressed,
    meters.dups_applied,
    meters.keys_burned
  );
  assert_eq!(meters.dups_suppressed, 1);
  assert_eq!(meters.keys_burned, 1);
  assert_eq!(meters.resumes, 1);
  assert_eq!(meters.expiries, 1);

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  info!("Grace Run - Finished.");
  Ok(())
}

async fn send(session: &RunSession, who: &Agent<PlayerId>, op: RunOp) {
  session.client_send(who.clone(), vec![op]).await;
  settle().await;
}

async fn settle() {
  tokio::time::sleep(TICK * 3).await;
}
