use crate::types::{GameOp, GameState, PlayerId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
};

#[derive(Debug, Default)]
pub struct DebuffSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GameState, GameOp> for DebuffSnapshotter {
  async fn create_snapshot(
    &self,
    state: &GameState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<GameOp>, SnapshotError<PlayerId>> {
    Ok(Some(GameOp::Snapshot(Box::new(state.clone()))))
  }
}
