// plaza/src/state_logic.rs
use crate::agent::{Agent, AgentId};
use crate::error::StateLogicError; // Assuming you might want to return a specific error type
use crate::session::TargetedOp; // We'll define this in session.rs soon
use async_trait::async_trait;
use std::fmt::Debug;
use std::time::Duration;

/// Represents the different kinds of inputs that can drive state changes.
#[derive(Debug, Clone)] // Op and ID must be Clone + Debug
pub enum LogicInput<Op, ID: AgentId> {
  /// Operations initiated by a specific agent.
  AgentOps { source: Agent<ID>, ops: Vec<Op> },
  /// An indication that a discrete step in time has occurred.
  TimeStep { delta_time: Duration },
  // Potentially other system-level inputs in the future:
  // SystemSignal { name: String, data: serde_json::Value },
}

/// Trait for the application's core state manipulation logic.
///
/// This is where the rules of your application/game reside. It defines how
/// `StateType` is mutated in response to operations (`Op`) or time progression.
#[async_trait]
pub trait StateLogic<Op, ID: AgentId, StateType>: Send + Sync + 'static {
  // It's Send + Sync + 'static because the StateController will hold it,
  // potentially an Arc<SL>, and operate in an async context.

  /// Processes a given input, mutates the state, and returns operations to be broadcast.
  /// This single method is the entry point for all state changes managed by Plaza.
  ///
  /// # Arguments
  /// * `current_state`: A mutable reference to the application's shared state data.
  ///                    Implementations will modify this directly.
  /// * `input`: The stimulus for this processing cycle, either agent-initiated operations
  ///            or a time step.
  ///
  /// # Returns
  /// A `Result` containing either:
  /// * `Ok(Vec<TargetedOp<Op, ID>>)`: A vector of operations that should be
  ///   broadcast to clients as a result of processing this input. An empty vector
  ///   means the input was processed successfully but no operations need to be broadcast.
  /// * `Err(StateLogicError)`: An error indicating that the input could not be processed
  ///   successfully (e.g., invalid operation, precondition failed). The `StateController`
  ///   will typically log this error and may choose to notify the originating agent if applicable.
  async fn process_input(
    &self, // Takes &self so implementations can have their own configuration/dependencies
    current_state: &mut StateType,
    input: LogicInput<Op, ID>,
  ) -> Result<Vec<TargetedOp<Op, ID>>, StateLogicError>;
}
