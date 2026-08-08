//! The board, built once and sent to everyone.
//!
//! Uniform on purpose: a battle is open information, so a per-recipient build
//! would cost a view per commander and buy nothing. Which army is *yours* is
//! public knowledge carried in the view's `commanders` list; a client matches
//! it against the id `YouAre` gave it.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::types::{BattleOp, BattleState, BattleView, PlayerId};

#[derive(Debug, Default)]
pub struct BattleSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, BattleState, BattleOp> for BattleSnapshotter {
  async fn create_snapshot(
    &self,
    state: &BattleState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<BattleOp>, SnapshotError<PlayerId>> {
    Ok(Some(BattleOp::Snapshot(Box::new(state.view()))))
  }
}

/// The view a client would render, for tests and the scripted run.
pub fn battle_view(state: &BattleState) -> BattleView {
  state.view()
}
