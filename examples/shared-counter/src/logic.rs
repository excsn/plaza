use crate::types::{CounterId, CounterOp, CounterStateData};
use async_trait::async_trait;
use plaza::{
  error::StateLogicError,
  session::{MessageTarget, TargetedOp},
  state_logic::{LogicInput, LogicOutput, StateLogic},
};
use tracing::{debug, warn};

#[derive(Clone, Debug, Default)]
pub struct CounterLogic;

#[async_trait]
impl StateLogic<CounterOp, CounterId, CounterStateData> for CounterLogic {
  async fn process_input(
    &self,
    current_state: &mut CounterStateData,
    input: LogicInput<CounterOp, CounterId>,
  ) -> Result<LogicOutput<CounterOp, CounterId>, StateLogicError> {
    let mut ops_to_broadcast: Vec<TargetedOp<CounterOp, CounterId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        debug!(agent = %source, num_ops = ops.len(), "Processing agent ops for counter");
        for op in ops {
          let applied_op = op.clone();
          match op {
            CounterOp::Increment(n) => {
              current_state.value += n;
              debug!(new_value = current_state.value, "Counter incremented");
            }
            CounterOp::Set(n) => {
              current_state.value = n;
              debug!(new_value = current_state.value, "Counter set");
            }
            // Server-originated: the snapshot provider builds these, clients
            // never send one.
            CounterOp::Snapshot(_) => {
              warn!("Ignoring a snapshot op sent by a client.");
              continue;
            }
          }
          current_state.version += 1;

          ops_to_broadcast.push(TargetedOp {
            from_agent: source.clone(),
            target: MessageTarget::All,
            ops: vec![applied_op],
          });
        }
      }
      LogicInput::TimeStep { delta_time } => {
        // Counter is not time-driven, so a TimeStep input does nothing here.
        debug!(?delta_time, "TimeStep received for CounterLogic, no action taken.");
      }
      LogicInput::AgentJoined { agent } => {
        debug!(agent = %agent, "Agent joined; counter state needs no per-agent setup.");
      }
      LogicInput::AgentLeft { agent_id } => {
        debug!(?agent_id, "Agent left; counter state needs no cleanup.");
      }
    }
    Ok(ops_to_broadcast.into())
  }
}
