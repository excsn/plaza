// examples/shared-counter/src/main.rs
mod in_process_session;
mod logic;
mod snapshot;
mod types; // Our new session impl

use in_process_session::InProcessCounterSession;
use logic::CounterLogic;
use snapshot::CounterSnapshotter;
use types::{CounterId, CounterOp, CounterSnapshotPayload, CounterStateData, CounterUser};

use plaza::{
  agent::Agent,
  controller::{ControllerCommand, StateController, StateControllerBuilder},
  session::{Session, SessionMessage}, // For matching received messages
};

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, Level};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG) // More verbose for example
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse()?)
        .add_directive("plaza_example_shared_counter=debug".parse()?)
        .add_directive("plaza=debug".parse()?),
    )
    .init();

  info!("Plaza Shared Counter Example - Starting");

  // 1. Initial State & Logic Handlers
  let initial_state = CounterStateData::default();
  let counter_logic = Arc::new(CounterLogic::default());
  let counter_snapshotter = Arc::new(CounterSnapshotter::default());

  // 2. Session
  // Our InProcessCounterSession is Arc internally from its new()
  let session = InProcessCounterSession::new();

  // 3. StateController
  // The controller takes ownership of its command receiver.
  let (controller_tx, controller) = StateControllerBuilder::new()
    .op_handler(counter_logic) // Infers Op, ID, StateType from CounterLogic
    .initial_state(initial_state)
    .session(session.clone()) // Infers SnapshotPayload, checks Op/ID compatibility
    .snapshot_provider(counter_snapshotter) // Checks ID, StateType, SnapshotPayload compatibility
     .command_buffer(64)
    // .with_query_response_type::<MyQueryResponseType>() // Example if QR type was different
     .build()
     .expect("Failed to build StateController");

  // Run the StateController in its own task
  tokio::spawn(async move {
    info!("StateController task starting...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  // Allow some time for controller to start up and subscribe to session events
  tokio::time::sleep(Duration::from_millis(50)).await;

  // 4. Simulate Clients
  let alice_id = CounterUser(1);
  let alice_agent = Agent::new_human(alice_id.clone(), "Alice");
  let bob_id = CounterUser(2);
  let bob_agent = Agent::new_human(bob_id.clone(), "Bob");

  // Simulate clients subscribing to messages from the server
  let mut alice_client_rx = session.simulate_client_subscribe();
  let mut bob_client_rx = session.simulate_client_subscribe();

  // Spawn tasks for each client to listen for messages
  let alice_handle = tokio::spawn(async move {
    info!("[Alice] Client task started, listening for messages.");
    while let Ok(msg) = alice_client_rx.recv().await {
      match msg {
        SessionMessage::Ops { from, ops } => {
          info!("[Alice] Received ops from {}: {:?}", from.label(), ops);
        }
        SessionMessage::StateData { from, data } => {
          info!("[Alice] Received snapshot from {}: {:?}", from.label(), data.payload);
        }
      }
    }
    info!("[Alice] Client task finished.");
  });

  let bob_handle = tokio::spawn(async move {
    info!("[Bob] Client task started, listening for messages.");
    while let Ok(msg) = bob_client_rx.recv().await {
      match msg {
        SessionMessage::Ops { from, ops } => {
          info!("[Bob] Received ops from {}: {:?}", from.label(), ops);
        }
        SessionMessage::StateData { from, data } => {
          info!("[Bob] Received snapshot from {}: {:?}", from.label(), data.payload);
        }
      }
    }
    info!("[Bob] Client task finished.");
  });

  // Simulate Alice joining the session
  // The InProcessSession::agent_join will publish to its on_agent_joined channel,
  // which the StateController is subscribed to.
  info!("Alice joining...");
  let _alice_conn_id = session.agent_join(alice_agent.clone()).await?;
  // No need to send HandleAgentJoined command manually if controller subscribes to session events.

  // Give controller time to process join and send snapshot
  tokio::time::sleep(Duration::from_millis(100)).await;

  // Simulate Bob joining the session
  info!("Bob joining...");
  let _bob_conn_id = session.agent_join(bob_agent.clone()).await?;
  tokio::time::sleep(Duration::from_millis(100)).await;

  // 5. Simulate Alice sending an operation
  info!("Alice sending Increment(5)");
  session.simulate_client_op_send(alice_agent.clone(), vec![CounterOp::Increment(5)]);
  // The InProcessSession::simulate_client_op_send publishes to its incoming_message_tx,
  // which the StateController is subscribed to.

  tokio::time::sleep(Duration::from_millis(100)).await; // Allow processing

  // 6. Simulate Bob sending an operation
  info!("Bob sending Set(42)");
  session.simulate_client_op_send(bob_agent.clone(), vec![CounterOp::Set(42)]);

  tokio::time::sleep(Duration::from_millis(100)).await; // Allow processing

  // 7. Query final state (optional)
  let (response_tx, response_rx) = tokio::sync::oneshot::channel();
  info!("Querying final state from controller...");
  controller_tx
    .send(ControllerCommand::QueryCurrentState { response_tx })
    .await?;
  match response_rx.await {
    Ok(final_state) => {
      info!("Final Counter State: {:?}", final_state);
    }
    Err(e) => {
      error!("Failed to query final state: {}", e);
    }
  }

  // Simulate Alice leaving
  info!("Alice leaving...");
  session.agent_leave(&alice_id, _alice_conn_id).await?; // Assuming conn_id is tracked
  tokio::time::sleep(Duration::from_millis(50)).await;

  // Send shutdown to controller (optional, or let it run until main exits)
  // controller_tx.send(ControllerCommand::Shutdown).await?;

  // Wait a bit for client tasks to see any final messages if shutdown is not immediate
  tokio::time::sleep(Duration::from_millis(200)).await;
  info!("Shared Counter Example - Main task finishing.");

  // Dropping controller_tx will eventually cause controller's command_rx.recv() to return None,
  // leading to shutdown if not handled by a specific Shutdown command.
  // Client tasks will end when their rx channels are closed (when session drops sender).

  // For clean exit, can abort tasks or use more sophisticated shutdown signals
  alice_handle.abort();
  bob_handle.abort();

  Ok(())
}
