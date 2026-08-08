//! The bench, built once and sent to everyone: the board object, the roster,
//! the locks. Presence deliberately does not ride here; it is a stream about
//! now, relayed as it happens.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{ForgeOp, PlayerId};
use crate::state::ForgeState;

#[derive(Debug, Default)]
pub struct ForgeSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, ForgeState, ForgeOp> for ForgeSnapshotter {
  async fn create_snapshot(
    &self,
    state: &ForgeState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<ForgeOp>, SnapshotError<PlayerId>> {
    Ok(Some(ForgeOp::Snapshot(Box::new(state.view()))))
  }
}
