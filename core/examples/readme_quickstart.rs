//! The program from README.md, kept compilable so the docs cannot rot.
//!
//! `cargo run -p plaza --example readme_quickstart`

use plaza::{
  agent::Agent,
  controller::{query_state, StateControllerBuilder},
  session::{InProcessSession, SessionMessage, TargetedOp},
  snapshot::{SnapshotContext, SnapshotData, SnapshotError, SnapshotProvider},
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type UserId = u64;

#[derive(Clone, Debug, Default)]
struct CounterState { value: i64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterOp { Increment(i64), Changed(i64) }

// The rules. The only place state is mutated.
#[derive(Debug, Default)]
struct CounterLogic;

#[async_trait]
impl StateLogic<CounterOp, UserId, CounterState> for CounterLogic {
  async fn process_input(
    &self,
    state: &mut CounterState,
    input: LogicInput<CounterOp, UserId>,
  ) -> Result<LogicOutput<CounterOp, UserId>, StateLogicError> {
    let mut ops = Vec::new();

    if let LogicInput::AgentOps { ops: incoming, .. } = input {
      for op in incoming {
        if let CounterOp::Increment(by) = op {
          state.value += by;
          ops.push(TargetedOp::new_system_all(vec![CounterOp::Changed(state.value)]));
        }
      }
    }

    Ok(ops.into())
  }
}

// What a joining client is sent.
#[derive(Debug, Default)]
struct CounterSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, CounterState, i64> for CounterSnapshotter {
  async fn create_snapshot_data(
    &self,
    state: &CounterState,
    _target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<i64>, SnapshotError<UserId>> {
    Ok(SnapshotData { payload: state.value })
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let session = InProcessSession::<CounterOp, UserId, i64>::new();

  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(controller.run());

  // Connecting yields an inbox; the join snapshot arrives on it.
  let alice = Agent::new_human(1u64, "Alice");
  let (_conn_id, inbox) = session.connect(alice.clone()).await?;

  session.client_send(alice, vec![CounterOp::Increment(5)]).await;

  while let Ok(msg) = inbox.recv().await {
    match msg {
      SessionMessage::StateData { data, .. } => println!("snapshot: {}", data.payload),
      SessionMessage::Ops { ops, .. } => println!("ops: {ops:?}"),
    }
    if query_state(&tx).await?.value == 5 {
      break;
    }
  }

  Ok(())
}
