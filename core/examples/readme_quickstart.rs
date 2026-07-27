//! The program from README.md, kept compilable so the docs cannot rot.
//!
//! `cargo run -p plaza --example readme_quickstart`

use plaza::{
  agent::Agent,
  controller::{query_state, StateControllerBuilder},
  session::{InProcessSession, SessionMessage, TargetedOp},
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type UserId = u64;

#[derive(Clone, Debug, Default)]
struct CounterState { value: i64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterOp { Increment(i64), Changed(i64), Snapshot(i64) }

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
impl SnapshotProvider<UserId, CounterState, CounterOp> for CounterSnapshotter {
  async fn create_snapshot(
    &self,
    state: &CounterState,
    _target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
    // A snapshot is an op. Box the variant if it carries a whole state view;
    // this one is an i64, so it does not need it.
    Ok(Some(CounterOp::Snapshot(state.value)))
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let session = InProcessSession::<CounterOp, UserId>::new();

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
  let alice = Agent::new_human(1u64);
  let (_conn_id, inbox) = session.connect(alice.clone()).await?;

  session.client_send(alice, vec![CounterOp::Increment(5)]).await;

  while let Ok(msg) = inbox.recv().await {
    // A snapshot arrives as an op, so there is one message kind to handle.
    for op in &msg.ops {
      match op {
        CounterOp::Snapshot(value) => println!("snapshot: {value}"),
        other => println!("op: {other:?}"),
      }
    }
    if query_state(&tx).await?.value == 5 {
      break;
    }
  }

  Ok(())
}
