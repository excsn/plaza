//! Remote Entity State Interpolation Example.
//!
//! This example simulates a client receiving state updates for a remote entity
//! from a server. It uses `SnapshotBuffer` to store these updates and
//! `Interpolatable` to calculate smooth intermediate states for rendering,
//! effectively hiding network jitter and discrete server updates.
//! It runs entirely locally without actual networking.

use plaza_client_utils::{
  interpolation::{Interpolatable, SnapshotBuffer}, // Import ToF32
  types::ClientTimeMs,                                                    // Using this as our Timestamp for simplicity
};
use std::{thread, time::Duration};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// --- Application-Specific Types for this Example ---

#[derive(Debug, Clone, PartialEq)]
struct RemoteEntityState {
  position_x: f32,
  // Could include rotation, animation state, etc.
}

// Implement Interpolatable for our RemoteEntityState
impl Interpolatable<ClientTimeMs> for RemoteEntityState {
  fn interpolate(
    &self,
    other: &Self,
    t: f32,
    _time_a: ClientTimeMs, // Timestamps provided for context if needed
    _time_b: ClientTimeMs,
  ) -> Self {
    RemoteEntityState {
      position_x: self.position_x + (other.position_x - self.position_x) * t,
    }
  }
}

// --- Simulation Settings ---
const SERVER_UPDATE_INTERVAL_MS: u64 = 100; // Server sends updates every 100ms
const CLIENT_RENDER_FPS: u32 = 60;
const CLIENT_RENDER_INTERVAL_MS: u64 = 1000 / CLIENT_RENDER_FPS as u64; // Approx 16ms
const INTERPOLATION_DELAY_MS: u64 = 150; // Client renders state ~150ms in the "past"
const SNAPSHOT_BUFFER_SIZE: usize = 10; // Store up to 10 recent snapshots
const SIMULATION_DURATION_MS: u64 = 3000; // Run simulation for 3 seconds

// --- Simulation ---

fn main() {
  let subscriber = FmtSubscriber::builder()
    .with_max_level(Level::INFO) // Adjust log level (TRACE for very verbose snapshot buffer activity)
    .finish();
  tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

  // --- Client Initialization ---
  let mut snapshot_buffer = SnapshotBuffer::<ClientTimeMs, RemoteEntityState>::new(SNAPSHOT_BUFFER_SIZE);
  let mut client_current_time_ms: ClientTimeMs = 0;

  // --- Simulated Server State ---
  let mut server_current_time_ms: ClientTimeMs = 0;
  let mut server_entity_position_x: f32 = 0.0;
  let entity_speed_units_per_sec: f32 = 50.0; // Entity moves 50 units per second

  tracing::info!(
    "Simulation started. Server updates every {}ms. Client renders at ~{} FPS. Interpolation delay: {}ms.",
    SERVER_UPDATE_INTERVAL_MS,
    CLIENT_RENDER_FPS,
    INTERPOLATION_DELAY_MS
  );
  tracing::info!("Client Time (ms) | Target Interp Time (ms) | Raw Server Pos @ Target | Interpolated Pos X");
  tracing::info!("-----------------|---------------------------|-------------------------|-------------------");

  // --- Main Simulation Loop (simulates passing of client time) ---
  while client_current_time_ms <= SIMULATION_DURATION_MS {
    // --- Simulate Server Sending Updates (if it's time) ---
    // In a real app, this would be asynchronous network events.
    if client_current_time_ms >= server_current_time_ms {
      // Check if it's time for the next server update batch
      // Simulate server physics for the time up to server_current_time_ms
      // For this demo, server sends updates based on its *own* clock progression.
      // Let's assume server_current_time_ms is what we check against client_current_time_ms
      // and then advance server_current_time_ms.

      let time_since_last_server_update_ms = SERVER_UPDATE_INTERVAL_MS; // Fixed interval
      server_entity_position_x += entity_speed_units_per_sec * (time_since_last_server_update_ms as f32 / 1000.0);

      let current_server_state = RemoteEntityState {
        position_x: server_entity_position_x,
      };
      snapshot_buffer.add_snapshot(server_current_time_ms, current_server_state.clone());
      tracing::debug!(
        "Client @ {}ms: Received SERVER update for server_time {}ms: {:?}",
        client_current_time_ms,
        server_current_time_ms,
        current_server_state
      );
      server_current_time_ms += SERVER_UPDATE_INTERVAL_MS; // Advance server time for next update
    }

    // --- Client Renders Frame ---
    // Calculate the target time on the server's timeline for interpolation
    // This is client's current time, adjusted "backwards" by the interpolation delay.
    // Note: In a real client, sophisticated clock sync or server_time - client_time offset
    // estimation would be used here. For this demo, we assume client_current_time_ms
    // can be directly compared after subtracting delay if clocks were perfectly synced.
    // A more robust target_server_time for interpolation:
    // `latest_server_ts_in_buffer - interpolation_delay` or
    // `client_current_time - estimated_rtt/2 - interpolation_delay`
    // For this example, we'll use a simplified target relative to client time.
    let target_interpolation_time_on_server_timeline = client_current_time_ms.saturating_sub(INTERPOLATION_DELAY_MS);

    if let Some(interpolated_state) =
      snapshot_buffer.get_interpolated_state(target_interpolation_time_on_server_timeline)
    {
      // Find the "raw" server position that would correspond to this target time for comparison
      // This is just for illustrative printing, client wouldn't normally do this.
      let server_pos_at_target_time_approx =
        entity_speed_units_per_sec * (target_interpolation_time_on_server_timeline as f32 / 1000.0);

      tracing::info!(
        "{:16} | {:25} | {:23.2} | {:17.2}",
        client_current_time_ms,
        target_interpolation_time_on_server_timeline,
        server_pos_at_target_time_approx, // Illustrative "true" position
        interpolated_state.position_x
      );
    } else {
      tracing::info!(
        "{:16} | {:25} | {:23} | {:17}",
        client_current_time_ms,
        target_interpolation_time_on_server_timeline,
        "N/A (no raw server)",
        "N/A (buffer empty/insufficient)"
      );
    }

    // Advance client time
    client_current_time_ms += CLIENT_RENDER_INTERVAL_MS;
    if client_current_time_ms <= SIMULATION_DURATION_MS {
      // Avoid sleep for the very last iteration
      thread::sleep(Duration::from_millis(CLIENT_RENDER_INTERVAL_MS / 2)); // Simulate render loop delay, /2 for faster demo
    }
  }

  tracing::info!("Interpolation demo finished.");
}
