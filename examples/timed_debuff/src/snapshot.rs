use crate::types::{DebuffSnapshotPayload, GameState, PlayerId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotData, SnapshotError, SnapshotProvider},
};

#[derive(Debug, Default)]
pub struct DebuffSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GameState, DebuffSnapshotPayload> for DebuffSnapshotter {
  async fn create_snapshot_data(
    &self,
    state: &GameState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<DebuffSnapshotPayload>, SnapshotError<PlayerId>> {
    Ok(SnapshotData { payload: state.clone() })
  }
}
