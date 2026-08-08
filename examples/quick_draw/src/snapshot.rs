//! The duel, built once and sent to everyone: open information, one uniform
//! view. The snapshot carries `server_now_ms` so every arrival also feeds the
//! client's clock estimate, which is what its sub-tick claims aim with.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{DrawOp, PlayerId};
use crate::state::DuelState;

#[derive(Debug, Default)]
pub struct DuelSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, DuelState, DrawOp> for DuelSnapshotter {
  async fn create_snapshot(
    &self,
    state: &DuelState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<DrawOp>, SnapshotError<PlayerId>> {
    Ok(Some(DrawOp::Snapshot(Box::new(state.view()))))
  }
}
