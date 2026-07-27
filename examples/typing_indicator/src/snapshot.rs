use crate::types::{AppOp, AppView, AppState, UserId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
};

#[derive(Debug, Default)]
pub struct TypingSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, AppState, AppOp> for TypingSnapshotter {
  async fn create_snapshot(
    &self,
    state: &AppState,
    _target_agent: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<AppOp>, SnapshotError<UserId>> {
    Ok(Some(AppOp::Snapshot(Box::new(AppView {
      users_presence: state.users_presence.clone(),
      version: state.version,
    }))))
  }
}
