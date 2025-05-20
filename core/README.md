# Plaza Core (`plaza-core`)

**License:** Mozilla Public License 2.0 (MPL-2.0)
**Status:** Experimental

`plaza-core` provides the foundational traits, structs, and architectural patterns for building server-side, real-time, shared-state applications. It enables developers to create robust, operation-driven backends by managing state, logic, and client interactions in a decoupled and testable manner.

This crate is the heart of the Plaza ecosystem.

## Core Abstractions

`plaza-core` is built around several key abstractions that you, as an application developer, will implement or utilize:

1.  **`StateType` (Your Application's State):**
    *   This is a struct or enum defined by your application that holds the shared, authoritative state. Examples: `PongGameState`, `CollaborativeDocumentState`.
    *   It must be `Clone + Debug + Default + Send + Sync + 'static`.

2.  **`Op` (Operations):**
    *   An enum defined by your application representing all possible actions that can modify your `StateType`.
    *   Examples: `MovePlayerOp { direction: Vec2 }`, `SubmitChatMessageOp { text: String }`.
    *   Must be `Clone + Debug + Send + 'static + Serialize + for<'de> Deserialize<'de>`.

3.  **`AgentId` Trait and `Agent<ID>` Enum:**
    *   `ID: AgentId` is your application's chosen type for uniquely identifying connected clients or system actors (e.g., `uuid::Uuid`, `u64`).
    *   `Agent<ID>` wraps this `ID` and distinguishes between human users, bots, or the system itself.
    *   `AgentId` requires `Clone + Debug + Eq + Hash + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static`.

4.  **`StateLogic<Op, ID, StateType>` Trait:**
    *   The **heart of your application's rules and behavior**. You implement this trait.
    *   Its primary method, `process_input(state: &mut StateType, input: LogicInput<Op, ID>) -> Result<Vec<TargetedOp<Op, ID>>, StateLogicError>`, is called by the `StateController`.
    *   `LogicInput` can be client operations, time steps, or agent join/leave events.
    *   It mutates the `StateType` and returns a list of `TargetedOp`s to be sent to clients.

5.  **`Session<Op, ID, SnapshotPayload>` Trait:**
    *   An abstraction over the network transport layer (e.g., WebSockets, TCP).
    *   You'll use a pre-built session implementation (like one from a future `plaza-session` crate) or implement this trait for your custom transport.
    *   Handles receiving serialized `Op`s from clients and sending serialized `Op`s or `SnapshotPayload`s to clients.
    *   Provides broadcast channels for `StateController` to subscribe to incoming messages, agent joins, and agent leaves.

6.  **`SnapshotProvider<ID, StateType, SnapshotPayload>` Trait:**
    *   You implement this to define how a snapshot of your `StateType` is created.
    *   The `SnapshotPayload` is a serializable representation of your state (or relevant parts) sent to new or reconnecting clients.
    *   `SnapshotPayload` must be `Clone + Debug + Send + 'static + Serialize + for<'de> Deserialize<'de>`.

7.  **`StateController<Op, ID, StateType, S: Session, SP: SnapshotProvider, QueryResponse>`:**
    *   The central orchestrator provided by `plaza-core`.
    *   Manages a single instance of your `StateType`.
    *   Owns your `StateLogic`, `Session` adapter, and `SnapshotProvider`.
    *   Runs an internal loop that:
        *   Listens for incoming messages/events from the `Session`.
        *   Receives external commands (e.g., to process a time step, submit system ops, handle disconnections).
        *   Passes these as `LogicInput` to your `StateLogic`.
        *   Takes the `TargetedOp`s returned by `StateLogic` and uses the `Session` to send them to the appropriate clients.
        *   Handles snapshotting for new agents.
    *   You create it using `StateControllerBuilder`.

8.  **`TargetedOp<Op, ID>` and `MessageTarget<ID>`:**
    *   Structs used by `StateLogic` to specify which `Op`s should be sent to which client(s) (`Agent(ID)`, `All`, `AllExcept(ID)`, etc.).

## Common Utility Modules

`plaza-core` also provides foundational "common" components (patterns and utilities) in its `common` module, such as:

*   **`common::scheduler`:** `TickEventScheduler`, `TimeEventScheduler`, `TickCallbackScheduler`, `TimeCallbackScheduler` for managing timed logic.
*   **`common::fsm`:** A generic `StateMachine` for managing entities or systems with distinct states.
*   **`common::participants`:** A `ParticipantTracker` for basic management of connected agents.
*   **(And future `game_common` and `app_common` for more specialized patterns like reconciliation support, flow control, presence, locking, etc.)**

## Getting Started with `plaza-core`

1.  **Add `plaza-core` to your `Cargo.toml`:**
    ```toml
    [dependencies]
    plaza-core = "0.1.0" # Or your specific version/path
    # ... other dependencies like tokio, serde, uuid ...
    ```

2.  **Define Your Core Types:**
    *   Your `StateType` struct/enum.
    *   Your `Op` enum.
    *   Your `PlayerId` type (e.g., `type PlayerId = uuid::Uuid;`).
    *   Your `SnapshotPayload` struct/enum.
    *   (Optional) Your `QueryRequest` / `QueryResponse` types if using the controller's query mechanism.

3.  **Implement the Core Traits:**
    *   `impl StateLogic<MyOp, MyPlayerId, MyStateType> for MyGameLogic { ... }`
    *   `impl SnapshotProvider<MyPlayerId, MyStateType, MySnapshotPayload> for MySnapshotter { ... }`

4.  **Choose or Implement a `Session`:**
    *   For initial development or testing, you can create a dummy in-process session using Tokio MPSC/broadcast channels (see examples).
    *   For production, you'd use a session adapter for your chosen network transport (e.g., a future `plaza-session-actix-ws`).

5.  **Build and Run the `StateController`:**
    ```rust
    use plaza_core::controller::StateControllerBuilder;
    use std::sync::Arc;
    use std::time::Duration;

    // async fn main() -> Result<(), Box<dyn std::error::Error>> {
    //     let initial_state = MyStateType::default();
    //     let logic = Arc::new(MyGameLogic::default());
    //     let session_adapter = Arc::new(MySessionImpl::new(/* ... */));
    //     let snapshot_provider = Arc::new(MySnapshotter::default());
    //
    //     let (command_tx, controller) = StateControllerBuilder::new()
    //         .op_handler(logic)
    //         .initial_state(initial_state)
    //         .session(session_adapter)
    //         .snapshot_provider(snapshot_provider)
    //         .command_buffer(128) // Size of the internal command channel
    //         .tick_interval(Duration::from_millis(50)) // Optional: for automatic TimeStep inputs
    //         .build()
    //         .expect("Failed to build StateController");
    //
    //     // Run the controller in its own task
    //     tokio::spawn(async move {
    //         if let Err(e) = controller.run().await {
    //             eprintln!("StateController exited with error: {}", e);
    //         }
    //     });
    //
    //     // Now your application can interact with the controller via `command_tx`
    //     // (e.g., from your network layer receiving client connections and ops)
    //     // and by setting up the Session to forward client messages.
    //     // ...
    //     Ok(())
    // }
    ```

## Examples

Refer to the `examples/` directory in the Plaza repository for working demonstrations, including:
*   `shared-counter`: A very basic example.
*   `ability_cooldowns`, `timed_debuff`, `typing_indicator`: Showcasing schedulers.
*   `pong`: A more complete game example (work-in-progress for session adapter).
*   `csp_net_example`: Demonstrates server-side setup for client-side prediction.

## Status

`plaza-core` is **experimental**. APIs are subject to change. Contributions and feedback are highly welcome!