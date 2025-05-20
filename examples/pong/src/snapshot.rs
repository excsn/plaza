use crate::types::{PlayerId, PongGameState, PongSnapshotPayload};
use plaza::{
    agent::Agent,
    error::SnapshotError,
    snapshot::{SnapshotContext, SnapshotData, SnapshotProvider},
};
use async_trait::async_trait;
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub struct PongSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, PongGameState, PongSnapshotPayload> for PongSnapshotter {
    async fn create_snapshot_data(
        &self,
        full_state: &PongGameState,
        target_agent: Option<&Agent<PlayerId>>, // The agent this snapshot is for
        context: Option<SnapshotContext>,       // Additional context (e.g., full, delta)
    ) -> Result<SnapshotData<PongSnapshotPayload>, SnapshotError<PlayerId>> {
        let agent_id_str = target_agent.and_then(|a| a.id().map(|id| id.to_string())).unwrap_or_else(|| "N/A".to_string());
        debug!(
            target_agent_id = %agent_id_str,
            ?context,
            game_id = %full_state.game_id,
            phase = ?full_state.phase,
            "Creating Pong snapshot"
        );

        // For Pong, the snapshot payload is a direct clone of the current game state.
        // No special handling for target_agent or context is implemented in this simple version.
        // A more advanced system might filter data based on the target_agent (e.g., fog of war)
        // or provide delta snapshots based on the context.

        let snapshot_payload = full_state.clone();

        Ok(SnapshotData {
            payload: snapshot_payload,
            // version: full_state.version, // If SnapshotData struct had a version field
        })
    }
}