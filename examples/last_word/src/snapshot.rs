//! The duel, built once and sent to everyone: open information, one uniform
//! view. The snapshot carries `server_now_ms` so every arrival also feeds the
//! client's clock estimate.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{DuelOp, PlayerId};
use crate::state::WordState;

#[derive(Debug, Default)]
pub struct WordSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, WordState, DuelOp> for WordSnapshotter {
  async fn create_snapshot(
    &self,
    state: &WordState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<DuelOp>, SnapshotError<PlayerId>> {
    Ok(Some(DuelOp::Snapshot(Box::new(state.view()))))
  }
}
