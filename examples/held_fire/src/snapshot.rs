//! The battle cut per recipient: a commander gets their side's view, and an
//! enemy outside their sight is absent from the payload rather than flagged
//! in it. Spectators get the whole board.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{PlayerId, WatchOp};
use crate::state::WatchState;

#[derive(Debug, Default)]
pub struct WatchSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, WatchState, WatchOp> for WatchSnapshotter {
  async fn create_snapshot(
    &self,
    state: &WatchState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<WatchOp>, SnapshotError<PlayerId>> {
    let side = target_agent
      .and_then(|agent| agent.id_cloned())
      .map(|player| state.side_of(player))
      .unwrap_or(255);
    Ok(Some(WatchOp::Snapshot(Box::new(state.view_for(side)))))
  }
}
