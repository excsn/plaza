//! Two armies on a board: the scripted run.
//!
//! What no other example has, and this one is for: a phase that **contains**
//! several actors. Within `Command(Blue)` the Blue player orders each of three
//! units, in any order they like, each marching at most once and striking at
//! most once, and the phase ends when the set of unspent units is empty or the
//! commander ends it. `flow_control` has no shape for that set, which is the
//! finding: the activation ledger is hand-written, and the README says what
//! that answers about the deferred turn-policy question.
//!
//! Alongside it, the pieces the other examples proved: unbounded rounds (a
//! battle ends when an army is routed, never on a count), a side deadline on
//! `PhasedScheduler` whose staleness needs no hand check, and sides that swap
//! every deployment.
//!
//! To command by hand, `cargo run -p plaza_example_field_orders --bin serve`
//! and open two browser tabs.

use plaza_example_field_orders::logic::BattleLogic;
use plaza_example_field_orders::snapshot::BattleSnapshotter;
use plaza_example_field_orders::types::{BattleOp, BattleState, PlayerId};

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
          } => {
            let fate = if felled { "felled" } else { "wounded" };
            info!("[{name}] unit {unit} struck {target}: {fate}, {hp_left} hp left");
          }
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
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse()?)
        .add_directive("plaza_example_field_orders=debug".parse()?),
    )
    .init();

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
  let tick_tx = controller_tx.clone();
  let ticker = tokio::spawn(async move { TickDriver::new(TICK).run(tick_tx).await });

  info!("--- commanders arriving; game 1 seats Wren as Blue, Roy as Red");
  let wren = Agent::new_human(1);
  let roy = Agent::new_human(2);
  let (_w, wren_inbox) = session.connect(wren.clone()).await?;
  let wren_task = spawn_listener("Wren", wren_inbox);
  let (roy_conn, roy_inbox) = session.connect(roy.clone()).await?;
  let roy_task = spawn_listener("Roy", roy_inbox);
  settle().await;

  info!("--- Roy tries to move in Blue's phase and is refused");
  send(&session, &roy, BattleOp::Move { unit: 4, to: (5, 1) }).await;

  info!("--- round 1: Blue marches unit 1 out and ends the phase; the rest forfeit");
  send(&session, &wren, BattleOp::Move { unit: 1, to: (3, 1) }).await;
  send(&session, &wren, BattleOp::EndPhase).await;

  info!("--- Red closes and draws first blood");
  send(&session, &roy, BattleOp::Move { unit: 4, to: (4, 1) }).await;
  send(&session, &roy, BattleOp::Strike { unit: 4, target: 1 }).await;
  send(&session, &roy, BattleOp::EndPhase).await;

  info!("--- round 2: the duel resolves");
  send(&session, &wren, BattleOp::Strike { unit: 1, target: 4 }).await;
  send(&session, &wren, BattleOp::EndPhase).await;
  send(&session, &roy, BattleOp::Strike { unit: 4, target: 1 }).await;
  send(&session, &roy, BattleOp::EndPhase).await;

  info!("--- round 3: Blue sits on its phase and the deadline ends it");
  tokio::time::sleep(TICK * 65).await;
  settle().await;

  info!("--- Roy quits the field; Blue wins by forfeit");
  session.disconnect(&2, roy_conn).await;
  settle().await;

  info!("--- a new commander takes the empty seat; the redeploy swaps the sides");
  let sable = Agent::new_human(3);
  let (_s, sable_inbox) = session.connect(sable.clone()).await?;
  let sable_task = spawn_listener("Sable", sable_inbox);
  tokio::time::sleep(TICK * 260).await;
  settle().await;

  info!("--- shutting down");
  controller_tx.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  wren_task.abort();
  roy_task.abort();
  sable_task.abort();
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
