use crate::types::{MoleGameState, MoleSnapshotPayload, PlayerId};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotData, SnapshotError, SnapshotProvider},
};
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Default)]
pub struct MoleSnapshotProvider;

#[async_trait]
impl SnapshotProvider<PlayerId, MoleGameState, MoleSnapshotPayload> for MoleSnapshotProvider {
  async fn create_snapshot_data(
    &self,
    state: &MoleGameState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<MoleSnapshotPayload>, SnapshotError<PlayerId>> {
    debug!(tick = state.current_tick, "Creating Whack-a-Mole snapshot.");
    let scores_snapshot: HashMap<PlayerId, u32> =
      state.player_info.iter().map(|(id, info)| (*id, info.score)).collect();
    let player_names_snapshot: HashMap<PlayerId, String> = state
      .player_info
      .iter()
      .map(|(id, info)| (*id, info.name.clone()))
      .collect();

    let payload = MoleSnapshotPayload {
      current_mole_slot: state.current_mole_slot,
      scores: scores_snapshot,
      player_names: player_names_snapshot,
      server_tick: state.current_tick,
    };
    Ok(SnapshotData { payload })
  }
}
