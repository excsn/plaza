# API Reference: `plaza_lobby`

## 1. Introduction & Core Concepts

`plaza_lobby` manages **rooms on a single server**: creating them, listing them, authorizing joins, and cleaning up finished ones. Each room is a `plaza` `StateController` running as its own task in the same process.

The division of labour:

*   **You implement [`RoomFactory`](#trait-roomfactory)**: how a room of your game is built. Plaza cannot know what your game needs, so spawning the controller is yours.
*   **The crate provides [`InMemoryLobbyManager`](#struct-inmemorylobbymanagerf-roomfactory)**: the registry and the join/list/reap flows around your factory.
*   **Rooms are reached through [`RoomHandle`](#trait-roomhandlegameagentid-agentid-customroomsettings)**: the lobby talks to a room only through `plaza`'s `ControllerCommand` channel, so it needs no knowledge of your game's types beyond the associated types you declare.

**Authorization, not connection.** A successful join means the lobby has *authorized* a player and returns the endpoint to connect to. The gameplay join happens when that client connects to the room's own transport and the room's `Session` fires its presence event. The lobby never proxies gameplay traffic.

**Single server.** Rooms are in-process tasks. Nothing here coordinates across machines; that is an application concern.

## 2. Error Handling

### Enum `LobbyError`

`Clone + Debug`, implements `std::error::Error` via `thiserror`.

*   `RoomNotFound(RoomId)`
*   `RoomSpawnFailed(String)`: returned by your factory.
*   `PlayerActionInvalid(String)`
*   `InvalidRoomSettings(String)`
*   `JoinRoomFailed(String)`: wrong password, room full, room refused.
*   `InternalOrchestrationError(String)`
*   `NotImplemented(String)`

## 3. Core Types

### Type Aliases

*   **`RoomId = Uuid`**: assigned by the manager before your factory is called.
*   **`GameMode = String`**: e.g. `"deathmatch"`.
*   **`PasswordVerifier = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>`**: receives `(attempt, stored_hash)`.

### Trait `RoomFactory`

```rust,ignore
#[async_trait]
pub trait RoomFactory: Send + Sync + 'static {
  type CustomGameSettings: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned + Default;
  type GameOp: Clone + Debug + Send + Sync + 'static + Serialize + DeserializeOwned;
  type GameID: AgentId;
  type GameStateType: Clone + Debug + Send + Sync + 'static + Default;

  async fn spawn_room(
    &self,
    room_id: RoomId,
    room_settings: &RoomSettings<Self::CustomGameSettings>,
  ) -> Result<InProcessRoomHandle<Self::GameOp, Self::GameID, Self::GameStateType, Self::CustomGameSettings>, LobbyError>;
}
```

Implement this once per game type. Inside `spawn_room` you build a `StateController` as usual, `tokio::spawn` its `run()`, and wrap the pieces in an `InProcessRoomHandle`.

### Trait `RoomHandle<GameAgentID: AgentId, CustomRoomSettings>`

What the lobby needs from a room. Implemented by `InProcessRoomHandle`; implement it yourself for a different room model.

*   `id(&self) -> RoomId`
*   `metadata(&self) -> RoomMetadata<CustomRoomSettings>`
*   `async accept_authorized_player(&self, player: Agent<GameAgentID>) -> Result<(), LobbyError>` The room's last chance to refuse: it may have filled since the lobby checked.
*   `async notify_player_departed(&self, player_id: &GameAgentID)`
*   `async request_shutdown(&self)`
*   `is_finished(&self) -> bool`
*   `session_endpoint_info(&self) -> String`: where clients connect, e.g. `"ws://host/game/<id>"`.

### Struct `InProcessRoomHandle<GameOp, GameID, GameStateType, CustomRoomSettings>`

A room running as a task in this process.

*   **`new(room_id, initial_metadata, command_tx, task_join_handle, game_session_endpoint, password_hash) -> Self`** Called from your factory. `command_tx` is the `CommandSender` from `StateControllerBuilder::build`; `task_join_handle` is the `JoinHandle` from spawning `controller.run()`.
*   **`update_player_count_in_metadata(&self, count: u32)`**: called by the room's own session as clients connect and disconnect. The lobby reads this when enforcing capacity.
*   **Public fields**: `room_id`, `command_tx`, `metadata`, `game_session_endpoint`.
*   Implements `RoomHandle`. The stored password hash is never exposed in `RoomMetadata`, which reports only whether one exists.

## 4. The Manager

### Struct `InMemoryLobbyManager<F: RoomFactory>`

*   **`new(room_factory: Arc<F>) -> Self`**
*   **`with_password_verifier(self, verifier: PasswordVerifier) -> Self`** Replaces the default, which is plain string equality: appropriate only for low-stakes room codes. Supply an argon2 or bcrypt verifier for real secrets.

#### Room lifecycle

*   **`async handle_create_room_request(&self, requester_id: &F::GameID, settings: RoomSettings<F::CustomGameSettings>) -> Result<RoomMetadata<..>, LobbyError>`** Generates a `RoomId`, calls your factory, registers the room, and returns its metadata. A factory error propagates and leaves no room behind.

*   **`async handle_join_room_request(&self, player_lobby_id: &F::GameID, player_game_agent: Agent<F::GameID>, payload: &JoinRoomRequestPayload) -> Result<JoinRoomOutcomePayload, LobbyError>`** In order: finds the room, verifies the password if it has one, checks capacity, then asks the room to accept the player. On success records the player's room and returns the connection endpoint.

*   **`list_rooms(&self, filters: Option<&RoomFilters>) -> Vec<RoomMetadata<..>>`**

*   **`async reap_finished_rooms(&self)`**: removes rooms whose controller task has ended, requesting shutdown on each and clearing player assignments.

*   **`async handle_player_leaving_lobby(&self, player_id: &F::GameID)`**: forwards the departure to whichever room the player was assigned to.

#### Reaching a room

*   **`room(&self, room_id: &RoomId) -> Option<Arc<InProcessRoomHandle<..>>>`** The way to send a specific room a `ControllerCommand`, read its metadata, or update its player count.
*   **`rooms(&self) -> Vec<Arc<InProcessRoomHandle<..>>>`**

## 5. Payloads

Serde-serializable message shapes for a client-facing lobby protocol. They are data definitions: routing them is the application's job.

### Requests

*   **`RoomSettings<CustomGameSettings>`**: `name: Option<String>`, `game_mode`, `max_players: u32`, `is_private: bool`, `password_hash: Option<String>`, `custom_game_settings`.
*   **`CreateRoomRequestPayload<CustomGameSettings>`**: `settings`.
*   **`JoinRoomRequestPayload`**: `room_id`, `password_attempt: Option<String>` (the client sends plaintext; the verifier compares it against the stored hash).
*   **`ListRoomsRequestPayload`**: `filters: Option<RoomFilters>`.
*   **`RoomFilters`**: `game_mode: Option<GameMode>`, `exclude_full: Option<bool>`, `exclude_private_if_no_password_known: Option<bool>`. All `Default`.

### Responses and notices

*   **`RoomMetadata<CustomGameSettings>`**: `room_id`, `name`, `game_mode`, `current_players`, `max_players`, `has_password`, `custom_game_settings_summary`. Note `has_password`, not the hash.
*   **`JoinRoomOutcomePayload`**: `room_id`, `success`, `reason_if_fail`, `room_session_endpoint: Option<String>`, `player_game_token: Option<String>`.
*   **`RoomListResponsePayload<CustomGameSettings>`**: `rooms`.
*   **`RoomCreatedNoticePayload<CustomGameSettings>`**: `metadata`.
*   **`RoomMetadataUpdatedNoticePayload<CustomGameSettings>`**: `updated_metadata`.
*   **`RoomClosedNoticePayload`**: `room_id`, `reason`.

## 5b. Latency admission and routing

A room states what its simulation can carry; the caller supplies what the server measured; the lobby matches.

*   **`RoomMetadata::max_one_way_ms: Option<u32>`**: the worst one-way delay this room can take. Stated by the room, because it is a property of that room's schedule and nothing above it can know the number. `None` is no limit, which is correct for a game that applies input on arrival.
*   **`JoinRoomRequestPayload::measured_one_way_ms: Option<u32>`**: what the server measured for this connection. Supplied by the caller because **the lobby owns no socket**, and it must be a server measurement rather than a client's claim: a client can understate its own latency and this gates entry. `plaza_session::agent_rtt` is the source.
*   **`LobbyError::UnsuitableConnection { measured_ms, allowed_ms }`**: its own variant rather than a string, because it is the one refusal a client can act on and both numbers belong in it.
*   **`rooms_playable_at(one_way_ms) -> Vec<RoomMetadata<_>>`**: the rooms this connection could actually play in, tightest schedule first so a fast link is not sent to the room built for slow ones. A room with no limit sorts last: it takes anybody, so it is the fallback rather than the first choice.
*   **`RoomFilters::playable_at_one_way_ms`**: the same, applied to a room listing.

**Why the decision is here rather than in the room.** A room can only say yes or no. A lobby can say *where*, which is the useful thing to do about a slow connection: route it to a room whose schedule is deep enough instead of turning it away. Refusal is what is left when nothing fits, not the primary behaviour.

**Why the measurement is not here.** Measuring needs the socket, deciding needs the rule, and routing needs the set of rooms. Those are three layers and they belong to the transport, the room, and the lobby respectively. Collapsing any two of them puts a number in a place that cannot check it.

## 6. Putting It Together

```rust,ignore
#[async_trait]
impl RoomFactory for MyGameFactory {
  type CustomGameSettings = MySettings;
  type GameOp = MyOp;
  type GameID = PlayerId;
  type GameStateType = MyState;

  async fn spawn_room(
    &self,
    room_id: RoomId,
    settings: &RoomSettings<MySettings>,
  ) -> Result<InProcessRoomHandle<MyOp, PlayerId, MyState, MySettings>, LobbyError> {
    let session = ActixWsPlazaSession::<MyOp, PlayerId, MySnapshot>::new();
    let (command_tx, controller) = StateControllerBuilder::new(
      Arc::new(MyLogic), session.clone(), Arc::new(MySnapshotter), MyState::default(),
    ).build();
    let task = tokio::spawn(controller.run());

    Ok(InProcessRoomHandle::new(
      room_id,
      RoomMetadata { /* from settings */ },
      command_tx,
      task,
      format!("ws://host/game/{room_id}"),
      settings.password_hash.clone(),
    ))
  }
}

let lobby = InMemoryLobbyManager::new(Arc::new(MyGameFactory))
  .with_password_verifier(Arc::new(|attempt, hash| argon2_verify(attempt, hash)));
```

Call `reap_finished_rooms` periodically (a scheduled job or a tick), since nothing reaps automatically.
