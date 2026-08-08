//! Two armies on a board: the scripted run, no window and no socket.
//!
//! What no other example has, and this one is for: a phase that **contains**
//! several actors. Within `Command(Blue)` every commander of that army orders
//! their own squad, in any order they like, each unit marching at most once
//! and acting at most once, and the phase ends when the army's set of unspent
//! units is empty or a commander ends it. `flow_control` has no shape for that
//! set, which is the finding: the activation ledger is hand-written, and the
//! README says what that answers about the deferred turn-policy question.
//!
//! The arc: a muster countdown, a refused out-of-phase order, knights through
//! the forests, a duel with counterstrikes, a healer's mend, a deadline ending
//! an idle phase, a forfeit, and a redeploy against the bot with the sides
//! swapped.
//!
//! To command by hand, `cargo run -p plaza_example_field_orders` opens the
//! desktop window; `./wasm-serve.sh` hosts the browser build.

use plaza_example_field_orders::bots;
use plaza_example_field_orders::logic::BattleLogic;
use plaza_example_field_orders::protocol::{BattleOp, PlayerId};
use plaza_example_field_orders::snapshot::BattleSnapshotter;
use plaza_example_field_orders::state::BattleState;

use plaza::{
  agent::Agent,
  controller::{ControllerCommand, StateControllerBuilder},
  session::in_process::ClientInbox,
  session::InProcessSession,
  tick_driver::TickDriver,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

type BattleSession = InProcessSession<BattleOp, PlayerId>;

const TICK: Duration = Duration::from_millis(20);

fn spawn_listener(name: &'static str, inbox: ClientInbox<BattleOp, PlayerId>) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      for op in msg.ops {
        match op {
          BattleOp::PhaseChanged(p) => {
            if let Some(reason) = p.reason {
              info!("[{name}] {reason}");
            }
          }
          BattleOp::Marched { unit, to } => info!("[{name}] unit {unit} marched to {to:?}"),
          BattleOp::Struck {
            unit,
            target,
            hp_left,
            felled,
            counter,
          } => {
            let verb = if counter { "answered" } else { "struck" };
            let fate = if felled { "felled".to_owned() } else { format!("{hp_left} hp left") };
            info!("[{name}] unit {unit} {verb} {target}: {fate}");
          }
          BattleOp::Healed { unit, target, hp_now } => info!("[{name}] unit {unit} mended {target}: {hp_now} hp"),
          BattleOp::Refused(why) => info!("[{name}] refused: {why:?}"),
          BattleOp::BattleOver { winner } => info!("[{name}] {winner:?} takes the field"),
          _ => {}
        }
      }
    }
  })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();

  info!("Plaza Field Orders Example - Starting");

  let session = BattleSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(BattleLogic),
    session.clone(),
    Arc::new(BattleSnapshotter),
    BattleState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(controller_tx.clone()));
  tokio::spawn(bots::play_the_bots(controller_tx.clone(), Duration::from_millis(150)));

  info!("--- Wren and Roy enter the lobby; Wren hosts, leaves the field on auto, and starts");
  let wren = Agent::new_human(1);
  let roy = Agent::new_human(2);
  let (_w, wren_inbox) = session.connect(wren.clone()).await?;
  let wren_task = spawn_listener("Wren", wren_inbox);
  let (roy_conn, roy_inbox) = session.connect(roy.clone()).await?;
  let roy_task = spawn_listener("Roy", roy_inbox);
  send(&session, &wren, BattleOp::StartMuster).await;
  tokio::time::sleep(Duration::from_millis(1500)).await;

  info!("--- Roy tries to move in Blue's phase and is refused");
  send(&session, &roy, BattleOp::Move { unit: 5, to: (6, 2) }).await;

  info!("--- round 1: the knights advance");
  send(&session, &wren, BattleOp::Move { unit: 1, to: (3, 2) }).await;
  send(&session, &wren, BattleOp::EndPhase).await;
  send(&session, &roy, BattleOp::Move { unit: 5, to: (6, 2) }).await;
  send(&session, &roy, BattleOp::EndPhase).await;

  info!("--- round 2: into the corridor, and first blood is answered");
  send(&session, &wren, BattleOp::Move { unit: 1, to: (4, 3) }).await;
  send(&session, &wren, BattleOp::EndPhase).await;
  send(&session, &roy, BattleOp::Move { unit: 5, to: (5, 3) }).await;
  send(&session, &roy, BattleOp::Strike { unit: 5, target: 1 }).await;
  send(&session, &roy, BattleOp::EndPhase).await;

  info!("--- round 3: the duel resolves, and the healer marches");
  send(&session, &wren, BattleOp::Strike { unit: 1, target: 5 }).await;
  send(&session, &wren, BattleOp::Move { unit: 4, to: (3, 2) }).await;
  send(&session, &wren, BattleOp::EndPhase).await;
  send(&session, &roy, BattleOp::EndPhase).await;

  info!("--- round 4: the mend lands, and Red sits on its phase until the deadline");
  send(&session, &wren, BattleOp::Move { unit: 4, to: (3, 3) }).await;
  send(&session, &wren, BattleOp::Heal { unit: 4, target: 1 }).await;
  send(&session, &wren, BattleOp::EndPhase).await;
  tokio::time::sleep(TICK * 65).await;

  info!("--- Roy quits the field; his squad marches home and Blue takes it");
  session.disconnect(&2, roy_conn).await;
  settle().await;

  info!("--- intermission, then back to the lobby: Wren restarts alone and the bot evens the sides");
  tokio::time::sleep(Duration::from_millis(5_500)).await;
  send(&session, &wren, BattleOp::StartMuster).await;
  tokio::time::sleep(Duration::from_millis(1_500)).await;

  info!("--- game 2: the sides swapped, and the bot squad commands Blue");
  tokio::time::sleep(Duration::from_secs(3)).await;

  info!("--- shutting down");
  controller_tx.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  wren_task.abort();
  roy_task.abort();
  info!("Field Orders Example - Finished.");
  Ok(())
}

async fn send(session: &BattleSession, who: &Agent<PlayerId>, op: BattleOp) {
  session.client_send(who.clone(), vec![op]).await;
  settle().await;
}

async fn settle() {
  tokio::time::sleep(TICK * 2).await;
}
