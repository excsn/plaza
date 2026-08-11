//! The smallest useful Plaza app: a counter two clients share.
//!
//! Shows the whole loop end to end: clients join and get a snapshot, send ops,
//! and see each other's changes broadcast back: using the library's
//! `InProcessSession` so there is no networking to set up.

mod logic;
mod snapshot;
mod types;

use logic::CounterLogic;
use snapshot::CounterSnapshotter;
use types::{CounterOp, CounterStateData, CounterUser};

use plaza::{
  agent::Agent,
  controller::{query_state, ControllerCommand, StateControllerBuilder},
  session::in_process::ClientInbox,
  session::InProcessSession,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

type CounterSession = InProcessSession<CounterOp, CounterUser>;

/// Logs what one client receives. The session already filtered by target, so
/// everything arriving here was addressed to this client.
fn spawn_client_listener(
  name: &'static str,
  inbox: ClientInbox<CounterOp, CounterUser>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    while let Ok(msg) = inbox.recv().await {
      // One message kind: a snapshot arrives as an op.
      info!("[{}] from {}: {:?}", name, msg.from, msg.ops);
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
        .add_directive("plaza_example_shared_counter=debug".parse()?),
    )
    .init();

  info!("Plaza Shared Counter Example - Starting");

  let session = CounterSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterStateData::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  let alice_id = CounterUser(1);
  let alice = Agent::new_human(alice_id.clone());
  let bob_id = CounterUser(2);
  let bob = Agent::new_human(bob_id.clone());

  // Connecting triggers the controller to send that agent a snapshot, which
  // arrives on the inbox returned here.
  info!("Alice joining...");
  let (alice_conn, alice_inbox) = session.connect(alice.clone()).await?;
  let alice_task = spawn_client_listener("Alice", alice_inbox);
  tokio::time::sleep(Duration::from_millis(50)).await;

  info!("Bob joining...");
  let (_bob_conn, bob_inbox) = session.connect(bob.clone()).await?;
  let bob_task = spawn_client_listener("Bob", bob_inbox);
  tokio::time::sleep(Duration::from_millis(50)).await;

  info!("Alice sending Increment(5)");
  session.client_send(alice.clone(), vec![CounterOp::Increment(5)]).await;
  tokio::time::sleep(Duration::from_millis(50)).await;

  info!("Bob sending Set(42)");
  session.client_send(bob.clone(), vec![CounterOp::Set(42)]).await;
  tokio::time::sleep(Duration::from_millis(50)).await;

  info!("Final counter state: {:?}", query_state(&controller_tx).await?);

  info!("Alice leaving...");
  session.disconnect(&alice_id, alice_conn).await;
  tokio::time::sleep(Duration::from_millis(50)).await;

  controller_tx.send(ControllerCommand::Shutdown).await?;
  alice_task.abort();
  bob_task.abort();

  info!("Shared Counter Example - Finished.");
  Ok(())
}
