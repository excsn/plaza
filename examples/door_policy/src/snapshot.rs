use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::SnapshotError,
  snapshot::{SnapshotContext, SnapshotProvider},
};

use crate::logic::ArcadeState;
use crate::types::{AgentKey, ArcadeOp};

/// The room, the same for everyone in it.
///
/// Nobody outside is a recipient, which is the door's doing rather than this
/// provider's: a refused socket never registers, so it is never in the roster
/// the logic names and no view is ever built for it.
#[derive(Debug, Default)]
pub struct RoomSnapshotter;

#[async_trait]
impl SnapshotProvider<AgentKey, ArcadeState, ArcadeOp> for RoomSnapshotter {
  async fn create_snapshot(
    &self,
    state: &ArcadeState,
    _target_agent: Option<&Agent<AgentKey>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<ArcadeOp>, SnapshotError<AgentKey>> {
    Ok(Some(ArcadeOp::Snapshot(Box::new(state.room()))))
  }
}
