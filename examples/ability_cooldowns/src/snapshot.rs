use crate::types::{CooldownSnapshotPayload, GameState, PlayerId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotData, SnapshotError, SnapshotProvider},
};

/// Sends the whole game state; small enough here that a tailored view isn't worth it.
#[derive(Debug, Default)]
pub struct CooldownSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GameState, CooldownSnapshotPayload> for CooldownSnapshotter {
  async fn create_snapshot_data(
    &self,
    state: &GameState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<CooldownSnapshotPayload>, SnapshotError<PlayerId>> {
    Ok(SnapshotData { payload: state.clone() })
  }
}
