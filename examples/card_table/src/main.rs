//! A turn-based, hidden-information game, which is what `flow_control` is built
//! for.
//!
//! This example covers:
//!
//! - `Phased` holding the phase, so a change cannot reach the server without
//!   reaching clients too
//! - `Epoch` letting a scheduled turn timeout notice that its round already
//!   ended, without anything having to cancel it
//! - `RoundRobinTurnManager` and `SequentialRoundManager` running turn order and
//!   a best-of-three
//! - per-recipient snapshots, so each player sees their own cards and only the
//!   *count* of everyone else's
//!
//! The rules are deliberately trivial (highest card played wins the round) and
//! the deal is shuffled from a fixed seed, so the run is reproducible and the
//! log shows the plaza wiring.
//!
//! This binary is the scripted run: three players, a fixed seed and one
//! scenario per round. To play it yourself, `cargo run -p
//! plaza_example_card_table --bin serve` and open three browser tabs.

use plaza_example_card_table::logic::TableLogic;
use plaza_example_card_table::snapshot::TableSnapshotter;
use plaza_example_card_table::types::{deal_seed, shuffled_hands, Card, CardOp, PlayerId, TableState};

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

type TableSession = InProcessSession<CardOp, PlayerId>;

const TICK: Duration = Duration::from_millis(20);
/// Fixed so the scripted run deals the same shuffled hands every time.
const SEED: u64 = 0x5EED;

/// Logs what one player receives, so the hidden-information split is visible in
/// the output: each player's snapshot shows a different hand.
fn spawn_player_listener(name: &'static str, inbox: ClientInbox<CardOp, PlayerId>) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      // One message kind: the per-recipient view arrives as an op alongside
      // everything else, so there is a single loop rather than a match on
      // ops-versus-snapshot.
      for op in msg.ops {
        match op {
          CardOp::Snapshot(v) => info!(
            "[{name}] my hand {:?}, opponents hold {:?}, scores {:?}",
            v.my_hand, v.opponents, v.scores
          ),
          CardOp::CardPlayed { player, card } => info!("[{name}] saw {player} play {card}"),
          CardOp::PlayedForYou { player, card } => info!("[{name}] {player} timed out, table played {card}"),
          CardOp::TrickWon { player, card } => info!("[{name}] {player} took the trick with {card}"),
          CardOp::PhaseChanged(n) => info!("[{name}] phase -> {:?}", n.new_phase),
          CardOp::RoundStarted(n) => info!("[{name}] round {} of {:?} begins", n.round_number, n.total_rounds),
          CardOp::RoundEnded(n) => info!("[{name}] round {} ended: {:?}", n.round_number, n.summary_data),
          CardOp::TurnChanged(n) => info!("[{name}] turn -> {:?}", n.new_turn_actor),
          CardOp::YouAre(id) => info!("[{name}] seated as {id}"),
          CardOp::PlayCard(_) => {}
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
        .add_directive("plaza_example_card_table=debug".parse()?),
    )
    .init();

  info!("Plaza Card Table Example - Starting");

  let session = TableSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(TableLogic),
    session.clone(),
    Arc::new(TableSnapshotter),
    TableState::new().with_seed(SEED),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  // Ticks drive the turn timeouts. Without them nothing would ever time out.
  let tick_tx = controller_tx.clone();
  let ticker = tokio::spawn(async move { TickDriver::new(TICK).run(tick_tx).await });

  let alice = Agent::new_human(PlayerId(1));
  let bob = Agent::new_human(PlayerId(2));
  let carol = Agent::new_human(PlayerId(3));

  info!("--- players arriving; the table starts once three are seated");
  let (_a_conn, alice_inbox) = session.connect(alice.clone()).await?;
  let alice_task = spawn_player_listener("Alice", alice_inbox);
  let (_b_conn, bob_inbox) = session.connect(bob.clone()).await?;
  let bob_task = spawn_player_listener("Bob", bob_inbox);
  let (carol_conn, carol_inbox) = session.connect(carol.clone()).await?;
  let carol_task = spawn_player_listener("Carol", carol_inbox);
  settle().await;

  // Every round deals afresh to all three, Carol included until she leaves
  // after the third deal.
  let seats = [PlayerId(1), PlayerId(2), PlayerId(3)];
  let lowest = |deal: u64, who: &Agent<PlayerId>| -> Card { shuffled_hands(deal_seed(SEED, deal), &seats)[who.id().expect("a player")][0] };

  // Round 1: everyone plays promptly, in turn order.
  info!("--- round 1: all three play in time");
  play(&session, &alice, lowest(1, &alice)).await;
  play(&session, &bob, lowest(1, &bob)).await;
  play(&session, &carol, lowest(1, &carol)).await;
  settle().await;

  // Round 2: Alice and Bob play, Carol sits on her turn. Her timeout fires and
  // the table chooses for her, ending the round. The timeouts still pending for
  // this round are now stale, because resolving it moved the phase. Nothing
  // cancels them; their epoch no longer matches.
  info!("--- round 2: Carol stalls, so the table plays for her");
  play(&session, &alice, lowest(2, &alice)).await;
  play(&session, &bob, lowest(2, &bob)).await;
  tokio::time::sleep(TICK * 16).await;

  // Round 3: Carol drops out. `remove_actor` closes the gap in the turn order,
  // so play continues with two rather than stalling on someone who left.
  info!("--- round 3: Carol disconnects, the remaining two play it out");
  session.disconnect(&PlayerId(3), carol_conn).await;
  settle().await;
  play(&session, &alice, lowest(3, &alice)).await;
  play(&session, &bob, lowest(3, &bob)).await;
  settle().await;

  info!("--- shutting down");
  controller_tx.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  alice_task.abort();
  bob_task.abort();
  carol_task.abort();

  info!("Card Table Example - Finished.");
  Ok(())
}

async fn play(session: &TableSession, who: &Agent<PlayerId>, card: Card) {
  session.client_send(who.clone(), vec![CardOp::PlayCard(card)]).await;
  settle().await;
}

/// Long enough for the controller to process and broadcast, short enough not to
/// trip a turn timeout.
async fn settle() {
  tokio::time::sleep(TICK * 2).await;
}
