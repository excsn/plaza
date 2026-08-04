use crate::types::{PlayerId, PongGameState, PongOp, PongSnapshotPayload};
use plaza::{
    agent::Agent,
    error::SnapshotError,
    snapshot::{SnapshotContext, SnapshotProvider},
};
use async_trait::async_trait;
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub struct PongSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, PongGameState, PongOp> for PongSnapshotter {
    async fn create_snapshot(
        &self,
        full_state: &PongGameState,
        target_agent: Option<&Agent<PlayerId>>,
        context: Option<SnapshotContext>,
    ) -> Result<Option<PongOp>, SnapshotError<PlayerId>> {
        let agent_id_str = target_agent.and_then(|a| a.id().map(|id| id.to_string())).unwrap_or_else(|| "N/A".to_string());
        debug!(
            target_agent_id = %agent_id_str,
            ?context,
            game_id = %full_state.game_id,
            phase = ?full_state.phase,
            "Creating Pong snapshot"
        );

        // `target_agent` is read only for the log line above: pong has no
        // hidden information, so every recipient gets this same payload, which
        // is what lets the tick pass run as one uniform snapshot.
        Ok(Some(PongOp::Snapshot(Box::new(PongSnapshotPayload::from(full_state)))))
    }
}