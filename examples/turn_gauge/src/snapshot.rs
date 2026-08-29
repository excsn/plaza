//! The battle, built once and sent to everyone: both parties are open
//! information, so the view is uniform. The snapshot carries `server_now_ms`
//! so every arrival also feeds the client's clock estimate.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{GaugeOp, PlayerId};
use crate::state::GaugeState;

#[derive(Debug, Default)]
pub struct GaugeSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GaugeState, GaugeOp> for GaugeSnapshotter {
  async fn create_snapshot(
    &self,
    state: &GaugeState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<GaugeOp>, SnapshotError<PlayerId>> {
    Ok(Some(GaugeOp::Snapshot(Box::new(state.view()))))
  }
}
