use crate::types::{CounterId, CounterSnapshotPayload, CounterStateData};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::SnapshotError,
  snapshot::{SnapshotContext, SnapshotData, SnapshotProvider},
};
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub struct CounterSnapshotter;

#[async_trait]
impl SnapshotProvider<CounterId, CounterStateData, CounterSnapshotPayload> for CounterSnapshotter {
  async fn create_snapshot_data(
    &self,
    full_state: &CounterStateData,
    target_agent: Option<&Agent<CounterId>>,
    context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<CounterSnapshotPayload>, SnapshotError<CounterId>> {
    debug!(?target_agent, ?context, "Creating snapshot for counter");
    // For the counter, the payload is just a clone of the state.
    // Context and target_agent are ignored for this simple provider.
    Ok(SnapshotData {
      payload: full_state.clone(),
    })
  }
}
