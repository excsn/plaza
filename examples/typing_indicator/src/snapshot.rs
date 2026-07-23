use crate::types::{AppState, TypingIndicatorSnapshotPayload, UserId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotData, SnapshotError, SnapshotProvider},
};

#[derive(Debug, Default)]
pub struct TypingSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, AppState, TypingIndicatorSnapshotPayload> for TypingSnapshotter {
  async fn create_snapshot_data(
    &self,
    state: &AppState,
    _target_agent: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<TypingIndicatorSnapshotPayload>, SnapshotError<UserId>> {
    Ok(SnapshotData { payload: state.clone() })
  }
}
