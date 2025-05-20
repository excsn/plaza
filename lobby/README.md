# Plaza Lobby (`plaza-lobby`)

**License:** Mozilla Public License 2.0 (MPL-2.0)
**Status:** Experimental

`plaza-lobby` is a Rust crate that provides components and patterns to accelerate the development of lobby and room management systems for applications built with `plaza-core`. It is primarily targeted at **single-server deployments** where game rooms (or collaborative sessions) are managed as in-process tasks.

While it offers concrete implementations for common scenarios, it's designed with flexibility in mind, allowing applications to customize game-specific aspects.

## Core Features & Components

1.  **Standardized Lobby Interaction Payloads (`op_payloads` module):**
    *   Defines common data structures (DTOs) for client-lobby and lobby-client communication.
    *   Examples:
        *   `RoomSettings<CustomGameSettings>`: For configuring new rooms.
        *   `RoomMetadata<CustomGameSettings>`: Publicly visible information about a room.
        *   `CreateRoomRequestPayload`, `JoinRoomRequestPayload`, `ListRoomsRequestPayload`.
        *   `JoinRoomOutcomePayload` (tells client connection details or failure reason).
        *   `RoomCreatedNoticePayload`, `RoomClosedNoticePayload`, `RoomListResponsePayload`.
    *   These payloads are generic over `CustomGameSettings` (application-defined) and use common `RoomId` and `GameMode` types.
    *   Applications integrate these payloads into their own `LobbyOp` enum.

2.  **In-Process Room Management:**
    *   **`RoomFactory` Trait:**
        *   The **primary integration point for your application's game logic**. You implement this trait for each distinct type of game or session your lobby can launch.
        *   It defines how to:
            *   Construct the initial `StateType` for a new game room.
            *   Provide the `StateLogic` for the game.
            *   Set up the `Session` and `SnapshotProvider` for the game room.
            *   Spawn the game room's `plaza-core::StateController` as a Tokio task.
        *   Returns an `InProcessRoomHandle`.
    *   **`InProcessRoomHandle` Struct:**
        *   A concrete handle to a game room's `StateController` running in the same OS process.
        *   Provides methods for the `InMemoryLobbyManager` to interact with the room (get metadata, request player authorization (conceptually), notify of departures, request shutdown, check if finished).
        *   Contains the `mpsc::Sender` to the game room's `StateController` and its `JoinHandle`.
    *   **`InMemoryLobbyManager` Struct:**
        *   A concrete lobby manager implementation provided by `plaza-lobby`.
        *   Manages a collection of active `InProcessRoomHandle`s.
        *   Uses an application-provided `RoomFactory` (via `Arc<F: RoomFactory>`) to spawn new rooms.
        *   Handles the logic for creating rooms, processing join requests (checking capacity, basic validation before handing off to the room itself), listing rooms, and reaping finished rooms.
        *   Designed to be owned or used by your application's `LobbyStateLogic`.

## How It Works (Single-Server Model)

1.  **Lobby as a Plaza Application:** Your main lobby service is itself a `plaza-core` application:
    *   It has its own `LobbyStateType` (e.g., to store `HashMap<RoomId, Arc<InProcessRoomHandle>>` via the `InMemoryLobbyManager`, and data about players currently in the lobby).
    *   It has its own `LobbyOp` enum (using payloads from `plaza_lobby::op_payloads`).
    *   It has its own `LobbyStateLogic` that interacts with an instance of `InMemoryLobbyManager`.
    *   It runs within a `plaza-core::StateController`.
    *   Players connect to this lobby controller via a `plaza-core::Session` implementation (e.g., from `plaza-session-actix-ws` on an endpoint like `/ws/lobby`).

2.  **Game Rooms as In-Process Plaza Applications:**
    *   When a room is created, the `InMemoryLobbyManager` (using your `RoomFactory`) spawns a new, independent `plaza-core::StateController` task *within the same OS process*.
    *   Each game room has its own game-specific `StateType`, `GameOp`, and `StateLogic`.
    *   Each game room requires its own `Session` handling, typically on a unique network endpoint (e.g., `ws://server/game/{room_id}`). The `InProcessRoomHandle` stores this endpoint information.

3.  **Client Flow:**
    1.  Client connects to the Lobby WebSocket endpoint.
    2.  Client sends lobby `Op`s (e.g., `CreateRoom`, `ListRooms`, `JoinRoom`).
    3.  Lobby `StateLogic` (via `InMemoryLobbyManager`) processes these.
    4.  If joining/creating a room is successful, the lobby sends back a `JoinRoomOutcomePayload` containing the specific WebSocket endpoint for that game room.
    5.  Client disconnects from the lobby WebSocket and connects to the indicated game room WebSocket.

## Getting Started

1.  **Add `plaza-lobby` to your application's `Cargo.toml`:**
    ```toml
    [dependencies]
    plaza-core = "0.1.0" # Or your version
    plaza-lobby = "0.1.0" # Or your version
    # ... tokio, serde, uuid, your networking session crate (e.g., plaza-session-actix-ws) ...
    ```

2.  **Define your Game-Specific Types:**
    *   `MyGameOp`, `MyGameStateType`, `MyGameSnapshotPayload`, `MyGameQueryResponse`.
    *   `MyCustomRoomSettings` (if your rooms have special settings beyond name, mode, max players).

3.  **Implement `RoomFactory` for Your Game Type(s):**
    *   This is the core piece of application-specific code. Your factory will know how to construct and spawn a `StateController` for your game, returning an `InProcessRoomHandle`.
    ```rust
    // use plaza_lobby::{RoomFactory, RoomId, RoomSettings, InProcessRoomHandle};
    // use plaza_core::controller::StateControllerBuilder;
    // use std::sync::Arc;

    // struct MyGameFactory;
    // #[async_trait::async_trait]
    // impl RoomFactory for MyGameFactory {
    //     type CustomGameSettings = MyGameCustomSettings; // Define this
    //     type GameOp = MyGameOp;
    //     type GameID = MyPlayerId; // Your AgentId for the game
    //     type GameStateType = MyGameStateType;
    //     type GameSnapshotPayload = MyGameSnapshotPayload;
    //     type GameQueryResponse = MyGameQueryResponse; // Or ()

    //     async fn spawn_room(&self, room_id: RoomId, room_settings: &RoomSettings<Self::CustomGameSettings>, /* ... */)
    //         -> Result<InProcessRoomHandle<...>, String>
    //     {
    //         // 1. Create initial_game_state from room_settings.custom_game_settings
    //         // 2. Create Arc<MyGameLogic>, Arc<MyGameSessionAdapterForRoom(room_id)>, Arc<MyGameSnapshotProvider>
    //         // 3. let (command_tx, controller) = StateControllerBuilder::new() ... .build().unwrap();
    //         // 4. let join_handle = tokio::spawn(controller.run());
    //         // 5. let metadata = RoomMetadata { /* from room_settings */ room_id, current_players: 0, ... };
    //         // 6. let endpoint = format!("/ws/game/{}", room_id); // Example
    //         // 7. Ok(InProcessRoomHandle::new(room_id, metadata, command_tx, join_handle, endpoint))
    //     }
    // }
    ```

4.  **Set up your Lobby `StateController`:**
    *   Define `MyLobbyOp` (using payloads from `plaza_lobby::op_payloads`), `MyLobbyStateType`, and `MyLobbyStateLogic`.
    *   In your `LobbyStateLogic` (or its associated state), instantiate `InMemoryLobbyManager` with your `RoomFactory`:
        ```rust
        // let my_game_factory = Arc::new(MyGameFactory);
        // let lobby_manager = Arc::new(InMemoryLobbyManager::new(my_game_factory));
        // // Your LobbyStateLogic will hold/use this lobby_manager Arc.
        ```
    *   Your `LobbyStateLogic` will call methods like `lobby_manager.handle_create_room_request(...)` when it processes corresponding `LobbyOp`s.

5.  **Configure Network Routing:** Your web server (e.g., Actix Web) needs to:
    *   Route general lobby connections (e.g., `/ws/lobby`) to the `Session` instance used by your `LobbyStateController`.
    *   Route game room connections (e.g., `/ws/game/{room_id}`) to the `Session` instance created by your `RoomFactory` for that specific room. (A `plaza-session-dynamic-router` utility could help here if using a single WebSocket server).

## Examples

*   An extensive example (`plaza_example_lobby_and_rooms` or similar) will be provided in the Plaza repository to demonstrate a full setup with a simple game type.

## Status

`plaza-lobby` is **experimental** and designed to complement `plaza-core`. Its primary goal is to simplify single-server multi-instance deployments. APIs may evolve.