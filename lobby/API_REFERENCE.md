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

## 5c. Queues, reservations and tickets

Three blocks either side of what the manager answers. Each holds no timers and spawns nothing, so an application drives it from its own `StateLogic`, in the manner of `plaza::common::reconnect::ReconnectTracker`.

### Struct `MatchQueue<ID: AgentId, T: SchedulerInstant>`

For games where a player is paired rather than choosing a room.

*   **`new(size: usize, patience: T)`**: matches of `size`, waiting `patience` before filling the remaining seats. Panics on a zero `size`.
*   **`enqueue(player, now) -> bool`**: `false` if already queued, so a double press does not take two places.
*   **`remove(&player) -> bool`**: for a cancel or a disconnect.
*   **`drain_ready(now) -> Vec<Formed<ID>>`**: every match that can start. Drive from `TimeStep`. Full matches first and for as long as there are enough people; then, if the longest-waiting player is out of patience, one more with the empty seats counted.
*   **`position(&player) -> Option<usize>`**, **`waiting_since(&player) -> Option<T>`**, **`contains`**, **`match_size`**, **`len`**, **`is_empty`**, **`clear`**.

### Struct `Formed<ID: AgentId>`

*   **`players: Vec<ID>`**: the humans, in the order they queued.
*   **`bots: usize`**: seats nobody is coming to fill. Zero for a full human match.
*   **`timed_out: bool`**: whether patience ran out rather than the match filling. Worth telling apart in a readout: a queue that only ever forms this way is a single-player game with a delay.
*   **`size() -> usize`**: `players.len() + bots`.

**Why patience produces seats to fill rather than a refusal.** A queue that only pairs humans stops working at exactly the moment a game needs it: launch, off-peak, small regions. What fills the seats is yours: a bot, a smaller board, a merged lobby.

### Struct `SeatReservations<ID: AgentId>`

The window between the lobby admitting a player and that player's socket arriving.

*   **`new()`**: reservations held until consumed or withdrawn, however long that takes.
*   **`with_expiry(window)`**: reservations that lapse `window` after they were made.
*   **`reserve(player) -> bool`**: promises a seat; `false` if already held. Re-reserving does not restart the window, so a lobby that re-issues on every quick-match press cannot keep an undialled seat alive indefinitely.
*   **`consume(&player) -> bool`**: takes the reservation, on `AgentJoined`. Spent once, so a second connection on one id is not a second seat.
*   **`withdraw(&player) -> bool`**: cancels one that will never be used.
*   **`tick(delta) -> Vec<ID>`**: advances this type's own clock and drops what lapsed, returning those players so the lobby can clear its records too. Call it from `LogicInput::TimeStep` with the same `delta_time`; nothing here reads a clock.
*   **`expiry()`**, **`holds`**, **`count`**, **`is_empty`**, **`iter`**, **`clear`**.

**A promise with a duration is still the lobby's word.** `with_expiry` does not contradict the rule below: the lobby sets the window when it reserves, so a lapse is the lobby having said "for this long" rather than the transport having said anything.

**Expiry cannot race an arrival**, because `consume` removes the reservation. A player who got in is already out of the sweep's reach, whatever happens on later ticks.

**A closing socket must not cancel a reservation.** This is why the type exists rather than a bare `HashSet`. Moving between rooms reserves the new seat and *then* closes the old socket, so the room sees `AgentLeft` after the promise was made. A room that withdraws on disconnect throws away a seat the lobby already granted, and the player silently lands as a spectator while the lobby reports them seated. Only the lobby can tell "gone" from "the same player, one second later", so `withdraw` is the lobby's word and never the transport's.

Distinct from `plaza_server_utils::SeatTable`, which allocates seat *indices* among agents already connected.

### Trait `TicketStore<ID: AgentId>` and `Ticket<ID>`

Fills `JoinRoomOutcomePayload::player_game_token`. Object-safe, so a route can hold `Arc<dyn TicketStore<ID>>` and be written once whichever store a deployment picked. Every implementation is internally synchronised, because the lobby that issues and the route that redeems are different callers sharing one by `Arc`.

*   **`issue(player, room) -> String`**: mints a token and records it.
*   **`issue_with(token, player, room)`**: records a token you minted, for a real credential.
*   **`redeem(&token, &room) -> Option<Ticket<ID>>`**: spends it for that room. One use, so a leaked token cannot be replayed alongside the connection already holding it.
*   **`revoke(&token) -> bool`**, **`outstanding() -> usize`**: a count that climbs rather than hovering means placements are issued and abandoned. A diagnostic: both shipped stores walk their contents to answer it.
*   **`Ticket<ID> { player: ID, room: RoomId }`**.

**The room is checked before the ticket is spent.** Spending first and comparing afterwards burns a ticket the room had no claim on, so under a guessable token anyone could destroy anyone else's placement by presenting it at the wrong door.

**Placement, not authentication.** Without a ticket, a room's only source for the connecting player's identity is the client, and a client that can name its own id can name somebody else's. `issue` mints a counter, which closes that and is not a secret; anything facing untrusted clients supplies its own signed, expiring value through `issue_with`. Plaza has no authentication story for this to be consistent with, which is why the crate provides the bookkeeping and not the secret.

### Struct `MapTicketRegistry<ID: AgentId>`

A `TicketStore` over a `HashMap` behind a mutex, adding no dependency the crate did not already have.

*   **`new()`**: never expires anything. A ticket issued and never dialled is held until the process ends.
*   **`with_expiry(window)`**: refuses and drops a ticket older than `window`. Sweeping runs from `issue`, the operation that grows the map, and at most once per `window`, so at most one window's worth of dead tickets is held however fast placements arrive. Nothing is spawned and nothing ticks.
*   **`expiry() -> Option<Duration>`**.

### Struct `CachedTicketRegistry<ID: AgentId>` (feature `cache`)

A `TicketStore` over `fibre_cache`, whose janitor sweeps on its own schedule and whose shards replace the single mutex the lobby and every room route otherwise share. Off by default, so nothing downstream pays for it unasked.

*   **`with_expiry(window)`**: a TTL, with no capacity set, so nothing is ever evicted for pressure and a ticket leaves only by being spent, revoked, or timing out.
*   **`run_maintenance()`**: forces the expiry pass deterministically, so a test need not sleep past the window and hope.

### Writing a third

A room in another process cannot share a map. Implement `TicketStore` to verify a signed token and build the `Ticket` from its claims, storing nothing at all; that case is why this is a trait rather than a mode on either type above.

### Expiry does not stand alone

A ticket outliving its `SeatReservations` entry lands a placed player as a spectator holding a spent ticket, and the reverse orphans a seat. Redemption is two steps in two places, the route spending the ticket and the room's logic consuming the reservation, so a ticket window must be **shorter** than the reservation's by at least the time a session takes to come up. Equal windows look correct and are not. `lobby_world` states both numbers adjacently for that reason: `PLACEMENT_WINDOW` at 30s and `RESERVATION_WINDOW` at 45s.

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
    let session = ActixWsPlazaSession::<MyOp, PlayerId>::new();
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
