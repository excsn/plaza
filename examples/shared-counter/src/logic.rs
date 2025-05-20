// examples/shared-counter/src/logic.rs
use crate::types::{CounterId, CounterOp, CounterStateData};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, StateLogic},
};
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub struct CounterLogic;

#[async_trait]
impl StateLogic<CounterOp, CounterId, CounterStateData> for CounterLogic {
  async fn process_input(
    &self,
    current_state: &mut CounterStateData,
    input: LogicInput<CounterOp, CounterId>,
  ) -> Result<Vec<TargetedOp<CounterOp, CounterId>>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<CounterOp, CounterId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        debug!(agent = %source.label(), num_ops = ops.len(), "Processing agent ops for counter");
        for op in ops {
          let applied_op = op.clone(); // The op that was actually applied
          match op {
            CounterOp::Increment(n) => {
              current_state.value += n;
              debug!(new_value = current_state.value, "Counter incremented");
            }
            CounterOp::Set(n) => {
              current_state.value = n;
              debug!(new_value = current_state.value, "Counter set");
            }
          }
          current_state.version += 1;

          // For a counter, every change is broadcast to everyone.
          ops_to_broadcast.push(TargetedOp {
            from_agent: source.clone(), // Attribute to the original source
            target: MessageTarget::All,
            ops: vec![applied_op],
          });
        }
      }
      LogicInput::TimeStep { delta_time } => {
        // Counter is not time-driven, so a TimeStep input does nothing here.
        debug!(?delta_time, "TimeStep received for CounterLogic, no action taken.");
      }
    }
    Ok(ops_to_broadcast)
  }
}
