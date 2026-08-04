use std::sync::Arc;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::SnapshotError,
  snapshot::{SnapshotContext, SnapshotProvider},
};

use crate::logic::PartyState;
use crate::moderation::Host;
use crate::types::PartyOp;

/// The table, including the moderation panel, the same for everyone.
///
/// The panel is public on purpose: who is quiet and who is flooding is not a
/// secret from the table, and a host tool that only the host can see cannot be
/// checked by anyone the host removes.
#[derive(Debug)]
pub struct TableSnapshotter {
  pub host: Arc<Host>,
}

#[async_trait]
impl SnapshotProvider<u64, PartyState, PartyOp> for TableSnapshotter {
  async fn create_snapshot(
    &self,
    state: &PartyState,
    _target_agent: Option<&Agent<u64>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<PartyOp>, SnapshotError<u64>> {
    Ok(Some(PartyOp::Snapshot(Box::new(state.table(&self.host)))))
  }
}
