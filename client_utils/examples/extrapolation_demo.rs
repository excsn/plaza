//! Remote Entity State Extrapolation Example.
//!
//! This example simulates a client receiving state updates (including velocity)
//! for a remote entity. It uses `ExtrapolationBase` and the `Extrapolatable`
//! trait to predict the entity's state for a short duration beyond the last
//! server update, helping to mask latency.
//! It runs entirely locally without actual networking.

use plaza_client_utils::{
  extrapolation::{Extrapolatable, ExtrapolationBase},
  types::ClientTimeMs, // Using this as our Timestamp for client-side timing
};
use std::{thread, time::Duration as StdDuration}; // Renamed to avoid conflict if we use Duration for TimeDelta
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// --- Application-Specific Types for this Example ---

#[derive(Debug, Clone, PartialEq)]
struct MovingObjectState {
  position_x: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct MovingObjectVelocity {
  speed_x: f32, // units per second
}

// Implement Extrapolatable for our MovingObjectState
// TimeDelta will be f32 (seconds) for this example's physics
impl Extrapolatable<MovingObjectVelocity, f32> for MovingObjectState {
  fn extrapolate_with_velocity(&self, velocity: &MovingObjectVelocity, delta_time_secs: f32) -> Self {
    MovingObjectState {
      position_x: self.position_x + velocity.speed_x * delta_time_secs,
    }
  }
}

// --- Simulation Settings ---
const SERVER_UPDATE_INTERVAL_MS: u64 = 200; // Server sends updates less frequently (every 200ms)
const CLIENT_RENDER_FPS: u32 = 60;
const CLIENT_RENDER_INTERVAL_MS: u64 = 1000 / CLIENT_RENDER_FPS as u64; // Approx 16ms
const MAX_EXTRAPOLATION_DURATION_MS: u64 = 150; // Max time to extrapolate forward
const SIMULATION_DURATION_MS: u64 = 3000; // Run simulation for 3 seconds

// --- Simulation ---

fn main() {
  let subscriber = FmtSubscriber::builder()
    .with_max_level(Level::INFO) // Adjust log level (TRACE for verbose extrapolation steps)
    .finish();
  tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

  // --- Client Initialization ---
  // Client starts with no information about the remote entity initially
  let mut extrapolation_base_opt: Option<
    ExtrapolationBase<MovingObjectState, MovingObjectVelocity, ClientTimeMs>, // ServerTimestamp is ClientTimeMs for simplicity here
  > = None;

  let mut client_current_time_ms: ClientTimeMs = 0;

  // --- Simulated Server State & Logic ---
  let mut server_current_time_ms: ClientTimeMs = 0;
  let mut server_entity_position_x: f32 = 0.0;
  let entity_actual_speed_x: f32 = 30.0; // Entity's true speed on the server

  tracing::info!(
    "Simulation started. Server updates every {}ms. Client renders at ~{} FPS. Max extrapolation: {}ms.",
    SERVER_UPDATE_INTERVAL_MS,
    CLIENT_RENDER_FPS,
    MAX_EXTRAPOLATION_DURATION_MS
  );
  tracing::info!("Client Time (ms) | Server Update @ | Extrapolated Pos X | Actual Server Pos X (for comparison)");
  tracing::info!("-----------------|-----------------|--------------------|--------------------------------------");

  // --- Main Simulation Loop (simulates passing of client time) ---
  while client_current_time_ms <= SIMULATION_DURATION_MS {
    // --- Simulate Server Sending an Update (if it's time) ---
    if client_current_time_ms >= server_current_time_ms {
      // Calculate server entity's state at server_current_time_ms
      // For this demo, we advance server state only when sending an update.
      // In reality, server state advances continuously.
      if server_current_time_ms > 0 {
        // Don't advance for the very first "update" at time 0
        let time_elapsed_on_server_ms = SERVER_UPDATE_INTERVAL_MS;
        server_entity_position_x += entity_actual_speed_x * (time_elapsed_on_server_ms as f32 / 1000.0);
      }

      let current_server_state = MovingObjectState {
        position_x: server_entity_position_x,
      };
      let current_server_velocity = MovingObjectVelocity {
        speed_x: entity_actual_speed_x,
      };

      tracing::debug!(
        "Client @ {}ms: Received SERVER update for server_time {}ms: State={:?}, Vel={:?}",
        client_current_time_ms,
        server_current_time_ms,
        current_server_state,
        current_server_velocity
      );

      // Client updates its extrapolation base
      extrapolation_base_opt = Some(ExtrapolationBase::new(
        current_server_state,
        current_server_velocity,
        server_current_time_ms, // Server's timestamp for this state
        client_current_time_ms, // Client's local time when this update was processed
      ));

      server_current_time_ms += SERVER_UPDATE_INTERVAL_MS; // Schedule next server update
    }

    // --- Client Renders Frame: Get Extrapolated State ---
    let mut display_position_x = 0.0; // Default if no base yet

    if let Some(ref extrap_base) = extrapolation_base_opt {
      // The convert_ms_to_time_delta function for this example: u64 ms -> f32 seconds
      let convert_to_f32_secs = |ms: u64| ms as f32 / 1000.0;

      if let Some(extrapolated_state) = extrap_base.get_extrapolated_state(
        client_current_time_ms, // Client's current render time
        MAX_EXTRAPOLATION_DURATION_MS,
        convert_to_f32_secs,
      ) {
        display_position_x = extrapolated_state.position_x;
      } else {
        // This case (None from get_extrapolated_state) might occur if, for example,
        // target_client_render_time_ms was somehow before extrap_base.client_receipt_time_ms
        // AND the clamping logic inside decided to return None (current impl clamps).
        // For this demo, it usually returns the clamped base state.
        display_position_x = extrap_base.state.position_x; // Fallback to last known if None
        tracing::warn!("Extrapolation returned None, falling back to last authoritative state.");
      }
    } else {
      tracing::info!(
        "{:16} | {:15} | {:18} | (Waiting for first server update)",
        client_current_time_ms,
        "N/A",
        "N/A"
      );
    }

    // For comparison, calculate the "actual" server position at client_current_time_ms
    // This is what an ideal, zero-latency client would see.
    let actual_server_pos_at_client_time = entity_actual_speed_x * (client_current_time_ms as f32 / 1000.0);

    if let Some(base) = &extrapolation_base_opt {
      tracing::info!(
        "{:16} | {:15} | {:18.2} | {:28.2}",
        client_current_time_ms,
        base.server_timestamp,
        display_position_x,
        actual_server_pos_at_client_time
      );
    }

    // Advance client time
    client_current_time_ms += CLIENT_RENDER_INTERVAL_MS;
    if client_current_time_ms <= SIMULATION_DURATION_MS {
      thread::sleep(StdDuration::from_millis(CLIENT_RENDER_INTERVAL_MS / 2)); // Simulate render loop delay
    }
  }

  tracing::info!("Extrapolation demo finished.");
}
