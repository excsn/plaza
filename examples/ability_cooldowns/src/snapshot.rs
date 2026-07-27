use crate::types::{GameOp, GameView, GameState, PlayerId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
};

/// Sends the whole game state; small enough here that a tailored view isn't worth it.
#[derive(Debug, Default)]
pub struct CooldownSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GameState, GameOp> for CooldownSnapshotter {
  async fn create_snapshot(
    &self,
    state: &GameState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<GameOp>, SnapshotError<PlayerId>> {
    Ok(Some(GameOp::Snapshot(Box::new(GameView {
      players: state.players.clone(),
      current_tick: state.current_tick,
      version: state.version,
    }))))
  }
}
