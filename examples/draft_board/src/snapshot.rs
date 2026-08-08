//! The board, built once and sent to everyone.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::types::{BoardView, DraftOp, DraftState, PlayerId};

/// Uniform, because a draft has nothing to hide.
///
/// The contrast with `card_table` is worth noticing: there a per-recipient
/// provider is what keeps a hand secret, and the cost is one build and one
/// encode per recipient. Every drafter here is entitled to the whole board, so
/// paying that cost would buy nothing.
#[derive(Debug, Default)]
pub struct BoardSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, DraftState, DraftOp> for BoardSnapshotter {
  async fn create_snapshot(
    &self,
    state: &DraftState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<DraftOp>, SnapshotError<PlayerId>> {
    Ok(Some(DraftOp::Snapshot(Box::new(state.view()))))
  }
}

/// The view a client would render, for tests and for the scripted run.
pub fn board(state: &DraftState) -> BoardView {
  state.view()
}
