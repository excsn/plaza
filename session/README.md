# Plaza Session (`plaza-session`)

**License:** Mozilla Public License 2.0 (MPL-2.0)
**Status:** Experimental (Implementations for specific transports may vary in maturity)

`plaza-session` provides concrete implementations of the `plaza_core::session::Session` trait for various network transports. Its goal is to reduce boilerplate for developers using `plaza-core` by offering ready-to-use session management solutions.

This crate uses Cargo features to enable specific transport implementations, allowing you to only compile and depend on what you need.

## Core Concept

The `plaza_core::session::Session<Op, ID, SnapshotPayload>` trait defines the interface between a `StateController` and the underlying networking layer. It's responsible for:
*   Managing client connections.
*   Handling agent join and leave notifications.
*   Sending `SessionMessage`s (containing `Op`s or `SnapshotPayload`s) to clients.
*   Providing a stream of incoming `SessionMessage`s from clients to the `StateController`.

`plaza-session` provides structs that implement this trait for you.

## Features & Implementations

Currently planned/available implementations (enable via Cargo features):

*   **`actix_ws` (Feature: `"actix_ws"`)**
    *   Provides `ActixWsPlazaSession<Op, ID, SnapshotPayload>`.
    *   Uses `actix-web` and the `actix-ws` crate for WebSocket communication.
    *   Manages individual WebSocket connections using an internal Actix actor.
    *   Handles serialization/deserialization of `Op`s and `SnapshotPayload`s (typically to/from JSON).
    *   Suitable for web-based clients.

*   **`tcp` (Feature: `"tcp"`) - *Conceptual/Planned***
    *   Would provide `TcpPlazaSession<Op, ID, SnapshotPayload>`.
    *   Uses `tokio::net::TcpListener` and `tokio::net::TcpStream`.
    *   Requires a framing protocol (e.g., length-prefixing) and serialization format (e.g., JSON, Bincode) for messages over TCP.
    *   Suitable for native clients or server-to-server communication.

*   **`stdio` (Feature: `"stdio"`) - *Conceptual/Planned***
    *   Provides `StdioPlazaSession<Op, ID, SnapshotPayload>`.
    *   Reads serialized `SessionMessage`s (e.g., JSON lines) from standard input.
    *   Writes serialized `SessionMessage`s to standard output.
    *   Useful for debugging, testing `StateLogic` in isolation, or creating simple CLI bots/clients.

*   **`in_process` (Feature: `"in_process"`) - *Conceptual/Planned***
    *   Provides `InProcessPlazaSession<Op, ID, SnapshotPayload>`.
    *   Uses Tokio MPSC and broadcast channels for communication.
    *   Allows running a "client" and "server" (StateController) in the same process, communicating via in-memory channels.
    *   Excellent for integration testing, examples, and single-process simulations.

## General Design of Session Implementations

Most session implementations in this crate follow a similar pattern:

1.  **Public Struct (e.g., `ActixWsPlazaSession`):**
    *   Implements `plaza_core::session::Session<Op, ID, SnapshotPayload>`.
    *   Is generic over your application's `Op`, `ID`, and `SnapshotPayload` types.
    *   Handles the serialization of your concrete types to a byte format (e.g., JSON `Vec<u8>`) for network transmission and deserialization of received bytes back into your types.
    *   Typically manages an internal actor or task system for handling individual network connections.

2.  **Connection Management:**
    *   The session implementation is responsible for accepting new connections (e.g., WebSocket upgrades, TCP accepts) and spawning a task/actor to handle each one.
    *   It tracks active connections and their associated `Agent<ID>`.

3.  **Message Handling:**
    *   **Outgoing:** When `StateController` calls `send_message()`, the session implementation serializes the message and routes it to the correct client connection(s).
    *   **Incoming:** Tasks handling individual client connections receive raw data, deserialize it into your `Op` type (or a generic `Vec<u8>` that `StateController` later deserializes, depending on the design choice - this crate aims for the former for better ergonomics), wrap it in a `SessionMessage`, and forward it to the `StateController` via a broadcast channel.

## Getting Started

1.  **Add `plaza-session` to your `Cargo.toml` with desired features:**
    ```toml
    [dependencies]
    plaza-core = "0.1.0" # Or your version
    plaza-session = { version = "0.1.0", features = ["actix_ws"] } # Example for actix_ws
    # ... other dependencies (actix-web if using actix_ws, tokio, serde, etc.)
    ```

2.  **Define Your Core Types** (as you would for `plaza-core`):
    *   `MyOp`, `MyPlayerId`, `MySnapshotPayload`. Ensure they implement `Serialize` and `DeserializeOwned` and other necessary traits (`Clone`, `Debug`, `Send`, `'static`).

3.  **Instantiate and Use the Session Implementation:**
    ```rust
    // Example using ActixWsPlazaSession
    // use plaza_session::ActixWsPlazaSession; // If re-exported from lib.rs
    use plaza_session::actix_ws_session::ActixWsPlazaSession; // Direct import
    use plaza_core::controller::StateControllerBuilder;
    use std::sync::Arc;

    // async fn setup_server() -> Result<(), Box<dyn std::error::Error>> {
    //     // Define your Op, ID, SnapshotPayload types
    //     // type MyAppOp = ...;
    //     // type MyAppId = ...;
    //     // type MyAppSnapshot = ...;

    //     // 1. Start the chosen session manager
    //     // The `start` method and its signature will be specific to each session type.
    //     // It typically returns an Arc<ActualSessionType> and any handles needed
    //     // by the web server/network listener to pass new connections to it.
    //     let (session_adapter, session_manager_handle_for_routes) = 
    //         ActixWsPlazaSession::<MyAppOp, MyAppId, MyAppSnapshot>::start();

    //     // 2. Create your StateLogic, initial_state, SnapshotProvider
    //     let initial_state = MyAppStateType::default();
    //     let logic = Arc::new(MyAppLogic::default());
    //     let snapshot_provider = Arc::new(MyAppSnapshotProvider::default());

    //     // 3. Build and run the StateController
    //     let (_command_tx, controller) = StateControllerBuilder::new()
    //         .op_handler(logic)
    //         .initial_state(initial_state)
    //         .session(session_adapter) // Pass the Arc<ActualSessionType>
    //         .snapshot_provider(snapshot_provider)
    //         .build()?;
    //     
    //     tokio::spawn(controller.run());

    //     // 4. Set up your network listener (e.g., Actix Web server)
    //     // The route handler for WebSocket upgrades will use `session_manager_handle_for_routes`
    //     // to register new WebSocket connections with the `ActixWsPlazaSession`'s internal manager.
    //     // HttpServer::new(move || App::new().app_data(session_manager_handle_for_routes.clone()).route("/ws", web::get().to(my_ws_route_handler)))
    //     //     .bind("127.0.0.1:8080")?
    //     //     .run()
    //     //     .await?;
    //     Ok(())
    // }
    ```
    *(The exact setup for `start()` and integrating with the web server route will be detailed in each session module's documentation, e.g., `actix_ws_session.rs`)*

## Status

`plaza-session` is **experimental**. Implementations for different transports will be added incrementally. APIs and features are subject to change. Feedback and contributions are welcome.