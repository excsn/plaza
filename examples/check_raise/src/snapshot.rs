//! The table cut per recipient: your hole cards appear only in your view and
//! a spectator sees backs everywhere until a showdown turns the live hands
//! face up for everyone.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{PlayerId, PokerOp};
use crate::state::TableState;

#[derive(Debug, Default)]
pub struct TableSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, TableState, PokerOp> for TableSnapshotter {
  async fn create_snapshot(
    &self,
    state: &TableState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<PokerOp>, SnapshotError<PlayerId>> {
    let seat = target_agent
      .and_then(|agent| agent.id_cloned())
      .map(|player| state.seat_of(player))
      .unwrap_or(255);
    Ok(Some(PokerOp::Snapshot(Box::new(state.view_for(seat)))))
  }
}
