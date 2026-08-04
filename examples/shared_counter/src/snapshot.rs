use crate::types::{CounterId, CounterOp, CounterStateData};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::SnapshotError,
  snapshot::{SnapshotContext, SnapshotProvider},
};
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub struct CounterSnapshotter;

#[async_trait]
impl SnapshotProvider<CounterId, CounterStateData, CounterOp> for CounterSnapshotter {
  async fn create_snapshot(
    &self,
    full_state: &CounterStateData,
    target_agent: Option<&Agent<CounterId>>,
    context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<CounterId>> {
    debug!(?target_agent, ?context, "Creating snapshot for counter");
    Ok(Some(CounterOp::Snapshot(Box::new(full_state.clone()))))
  }
}
