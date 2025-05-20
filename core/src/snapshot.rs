// plaza/src/snapshot.rs
use crate::agent::{Agent, AgentId};
use crate::error::SnapshotError; // PlazaError for Result return types
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// Wrapper for the actual snapshot payload data.
/// This allows for future metadata or versioning to be added here if needed,
/// without changing the `SnapshotProvider` trait too much.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotData<SnapshotPayload> {
  pub payload: SnapshotPayload,
}

/// Optional context provided to the `SnapshotProvider` when creating a snapshot.
/// This can be used for delta snapshots or client-specific views.
#[derive(Debug, Clone, Default)]
pub enum SnapshotContext {
  #[default]
  Full, // Request a full snapshot
  DeltaFromVersion(u64), // Request changes since a known version
  ForPerspective(String), // E.g., "player_view", "admin_view"
                         // User can define their own context variants if needed by extending this or using a generic param
}

/// Trait for a component that knows how to generate snapshot data for the application.
#[async_trait]
pub trait SnapshotProvider<ID: AgentId, StateType, SnapshotPayload>: Send + Sync + 'static {
  // Send + Sync + 'static because StateController will hold an Arc<SP>.

  /// Creates snapshot data based on the current full state.
  ///
  /// # Arguments
  /// * `full_state`: A read-only reference to the current, authoritative shared state data.
  /// * `target_agent`: Optionally, the specific agent for whom this snapshot is being generated.
  ///                   This allows for creating agent-specific views of the state.
  /// * `context`: Optional context that might influence how the snapshot is generated
  ///              (e.g., requesting a delta from a known version).
  ///
  /// # Returns
  /// A `Result` containing either:
  /// * `Ok(SnapshotData<SnapshotPayload>)`: The generated snapshot data.
  /// * `Err(SnapshotError)`: An error indicating failure to generate the snapshot.
  async fn create_snapshot_data(
    &self,                  // Takes &self so implementations can have their own configuration
    full_state: &StateType, // Read-only access to the state
    target_agent: Option<&Agent<ID>>,
    context: Option<SnapshotContext>, // Could be generic later: Option<C: UserContext>
  ) -> Result<SnapshotData<SnapshotPayload>, SnapshotError<ID>>;
}
