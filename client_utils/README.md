# Plaza Client Utilities (`plaza-client-utils`)

**License:** Mozilla Public License 2.0 (MPL-2.0)
**Status:** Experimental

`plaza-client-utils` is a Rust crate offering utilities to help client applications implement common advanced networking patterns when interacting with a server (especially one built with `plaza-core` or similar principles). It focuses on enhancing user experience by managing client-side prediction, server reconciliation, and smooth rendering of remote entities.

This crate provides pure Rust logic and is unopinionated about your specific game engine or rendering library.

## Core Features & Patterns Supported

1.  **Client-Side Prediction (CSP) & Server Reconciliation:**
    *   **Goal:** Provide immediate feedback for player actions by simulating them locally, then correcting (reconciling) with authoritative state from the server.
    *   **Components:**
        *   `ClientInputBuffer`: Stores a history of inputs sent to the server, along with the client's predicted state *before* each input was applied.
        *   `PredictedEntity`: Manages the lifecycle of a client-controlled entity. It applies local inputs for instant prediction and handles the reconciliation process when authoritative server state arrives, replaying unacknowledged inputs to derive a corrected predicted state.
    *   **Server Support Assumed:** Your server should send back authoritative state updates that include the sequence number of the last client input it processed for that state. (`plaza-core`'s `game_common::reconciliation` module provides server-side helpers for this).

2.  **Remote Entity State Interpolation:**
    *   **Goal:** Smoothly render remote entities (those controlled by other players or the server) even when server updates are discrete and arrive with network jitter.
    *   **Components:**
        *   `Interpolatable<Timestamp>` Trait: Implemented by your client's entity state type to define how to interpolate between two state instances.
        *   `ServerSnapshot<Timestamp, StateType>`: A wrapper for state updates received from the server, tagged with the server's authoritative timestamp.
        *   `SnapshotBuffer<Timestamp, StateType>`: Stores a short history of `ServerSnapshot`s for a remote entity and provides methods to get an interpolated state for any given render time (within its buffered window).
    *   **Server Support Assumed:** Your server should send regular state updates for remote entities, each tagged with a server timestamp. (`plaza-core`'s `game_common::reconciliation::op_payloads::RemoteEntitySnapshot` is a suggested payload).

3.  **Remote Entity State Extrapolation (Basic):**
    *   **Goal:** Predict a remote entity's state for a very short duration beyond the last known server update, using its last known velocity, to further mask latency.
    *   **Components:**
        *   `Extrapolatable<VelocityType, TimeDelta>` Trait: Implemented by your client's entity state type to define how to project its state forward.
        *   `ExtrapolationBase<StateType, VelocityType, ServerTimestamp>`: Stores the last authoritative state and velocity for an entity and provides a method to get an extrapolated state.
    *   **Server Support Assumed:** Your server should include velocity information in its state updates for remote entities.

4.  **Basic Math Types (Optional):**
    *   The `math` module provides simple `Vec2`, `Vec3`, and `Quat` structs with `Interpolatable` and `Extrapolatable` implementations if you don't want to bring in a larger math library for these utilities. You are encouraged to use your own math library and implement the traits for its types.

## Philosophy

*   **Client-Side Logic:** Focuses purely on algorithms and data structures for the client.
*   **Generic & Unopinionated:** Designed to work with your application's `StateType`, `Op` types, and chosen rendering/game engine.
*   **Complements Server Authority:** Assumes a server-authoritative model where the client predicts optimistically but always reconciles with server truth.
*   **Transport Agnostic:** These utilities operate on deserialized game state and input types; they don't handle network transport themselves. You integrate them with your client's networking layer (WebSockets, WebRTC, `renet`, etc.).

## Getting Started

1.  **Add `plaza-client-utils` to your client application's `Cargo.toml`:**
    ```toml
    [dependencies]
    plaza-client-utils = "0.1.0" # Or your specific version/path
    # ... your game engine, networking library, etc. ...
    ```

2.  **Define Your Client's State and Input Types:**
    *   `MyPlayerState`: The struct representing your player-controlled entity's state. Must be `Clone + Debug`. If using interpolation/extrapolation, it (or a render-specific version) should implement `Interpolatable`/`Extrapolatable`.
    *   `MyClientOp`: The type representing inputs your client generates and sends to the server (e.g., `MoveInput { dx, dy }`). Must be `Clone + Debug`.

3.  **Client-Side Prediction & Reconciliation Setup:**
    *   Create an instance of `ClientInputBuffer<MyClientOp, MyPlayerState>`.
    *   Create an instance of `PredictedEntity<MyPlayerState, MyClientOp>`.
    *   Implement your client-side simulation logic: `fn apply_op_to_state(state: &mut MyPlayerState, op: &MyClientOp) { ... }`.
    *   **On Local Input:**
        1.  Generate your `MyClientOp` and a `SequenceNumber`.
        2.  Call `predicted_entity.apply_local_input_and_predict(&op, seq_num, &mut input_buffer, &apply_op_to_state_fn)`.
        3.  Send the `op` and `seq_num` to the server (e.g., wrapped in `plaza_core::game_common::reconciliation::op_payloads::SequencedClientInput`).
    *   **On Receiving Authoritative State from Server** (e.g., `plaza_core::game_common::reconciliation::op_payloads::AuthoritativeStateUpdate`):
        1.  Call `predicted_entity.reconcile_with_server_state(server_state, server_ack_seq, &mut input_buffer, &apply_op_to_state_fn)`.
    *   **Render** using `predicted_entity.current_predicted_state`.

4.  **Remote Entity Interpolation Setup:**
    *   For each remote entity, create a `SnapshotBuffer<ServerTimestampType, MyRemoteEntityState>`.
    *   Ensure `MyRemoteEntityState` implements `Interpolatable<ServerTimestampType>`.
    *   **On Receiving Remote Entity Snapshot from Server** (e.g., `plaza_core::game_common::reconciliation::op_payloads::RemoteEntitySnapshot`):
        1.  Call `snapshot_buffer.add_snapshot(server_timestamp, remote_state)`.
    *   **In Your Render Loop:**
        1.  Calculate your `target_render_time_on_server_timeline`.
        2.  Call `snapshot_buffer.get_interpolated_state(target_time)` to get the state to render.

## Examples

Refer to the `examples/` directory within the `plaza-client-utils` crate for demonstrations:
*   `basic_csp.rs`: Shows `ClientInputBuffer` and `PredictedEntity` in a local simulation.
*   `interpolation_demo.rs`: Shows `SnapshotBuffer` and `Interpolatable` for smoothing.
*   `extrapolation_demo.rs`: Shows basic extrapolation.
*   (Future) `full_csp_net_example_client`: A client for a full client-server example, demonstrating integration of these utilities with (simulated) networking.

## Status

`plaza-client-utils` is **experimental**. APIs are subject to change. Contributions and feedback are welcome!