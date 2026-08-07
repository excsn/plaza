# `plaza_lobby`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Rooms on a single server for [`plaza`](../core/): creating them, listing them, authorizing joins, and reaping finished ones. Each room is a `StateController` running as its own task in the same process.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.1"
plaza_lobby = "0.1"
```

## The division of labour

You implement `RoomFactory`: how a room of *your* game is built, since Plaza cannot know that. The crate provides `InMemoryLobbyManager`: the registry and the create/join/list/reap flows around your factory.

```rust,ignore
#[async_trait]
impl RoomFactory for MyGameFactory {
  type CustomGameSettings = MySettings;
  type GameOp = MyOp;
  type GameID = PlayerId;
  type GameStateType = MyState;

  async fn spawn_room(&self, room_id: RoomId, settings: &RoomSettings<MySettings>)
    -> Result<InProcessRoomHandle<MyOp, PlayerId, MyState, MySettings>, LobbyError>
  {
    let session = ActixWsPlazaSession::<MyOp, PlayerId>::new();
    let (command_tx, controller) = StateControllerBuilder::new(
      Arc::new(MyLogic), session, Arc::new(MySnapshotter), MyState::default(),
    ).build();

    Ok(InProcessRoomHandle::new(
      room_id,
      RoomMetadata { /* from settings */ },
      command_tx,
      tokio::spawn(controller.run()),
      format!("ws://host/game/{room_id}"),
      settings.password_hash.clone(),
    ))
  }
}

let lobby = InMemoryLobbyManager::new(Arc::new(MyGameFactory));
```

## Authorization, not connection

A successful join means the lobby *authorized* a player and returns the endpoint to connect to. The gameplay join happens when that client connects to the room's own transport. The lobby never proxies gameplay traffic: it hands out addresses.

## Passwords

`RoomSettings::password_hash` is compared by a verifier you supply. The default is plain string equality, which suits low-stakes room codes and nothing else:

```rust,ignore
let lobby = InMemoryLobbyManager::new(factory)
  .with_password_verifier(Arc::new(|attempt, hash| argon2_verify(attempt, hash)));
```

`RoomMetadata` exposes only `has_password`, never the hash.

## Reaping

Nothing reaps automatically. Call `reap_finished_rooms` from a scheduled job or a tick; it removes rooms whose controller task has ended and clears their player assignments.

To reach a specific room (to send it a command or update its player count as clients connect), use `room(&room_id)` or `rooms()`.

## Scope

Single server. Rooms are in-process tasks, and nothing here coordinates across machines; that remains an application concern.

## Status

Experimental. The API changes.

## Latency admission, and why it lives here

A game that schedules inputs ahead can only carry a connection whose delay fits inside the schedule. Past that, every input a player sends lands outside the accepting window and is dropped, so they are seated and then cannot play, which reads as a broken game rather than an unsuitable connection.

A room states its limit as `RoomMetadata::max_one_way_ms`, because the limit is a property of that room's simulation and nothing above it can know the number. `None` means no limit, which is right for a game that applies input on arrival and therefore has nothing to miss.

**The measurement is supplied by the caller, not taken here.** The lobby owns no socket, and the number has to be one the *server* measured rather than one the client reported: a client can understate its own latency, and this decides entry. [`plaza_session`](../session/) exposes it as `agent_rtt`, timed from its own WebSocket ping.

```rust,ignore
let (rtt, samples) = session.agent_rtt(&id).unwrap_or_default();
lobby.handle_join_room_request(&id, agent, &JoinRoomRequestPayload {
  room_id,
  password_attempt: None,
  measured_one_way_ms: (samples >= 8).then(|| rtt.as_millis() as u32 / 2),
}).await
```

**Refusal is the degenerate case.** The reason this belongs to a lobby rather than to a room is that a room can only say yes or no, while a lobby can say *where*: `rooms_playable_at(one_way_ms)` returns the rooms a connection could actually play in, tightest schedule first, so a fast link is not sent to the room built for slow ones and made to pay its delay. A room with no limit sorts last, since it takes anybody and is therefore the fallback. `RoomFilters::playable_at_one_way_ms` does the same for a room list, so a player is shown what they can play rather than what they will be turned away from.

When nothing fits, `LobbyError::UnsuitableConnection` carries both numbers, so a client can state the case instead of just declining.

## Blocks either side of the manager

`InMemoryLobbyManager` answers "which room". These cover what happens before and after, and each is bookkeeping your own `StateLogic` drives: no timers, no tasks.

| Block | For |
|---|---|
| `MatchQueue` | Being paired rather than choosing. Forms full matches, and when patience runs out reports how many seats to fill with bots rather than refusing to start. |
| `SeatReservations` | The gap between admission and arrival. A closing socket deliberately does **not** cancel a reservation: a room hop closes the old connection *after* the new seat is reserved, so treating a disconnect as a cancellation silently demotes a player the lobby already promised. `with_expiry` bounds the gap, swept from your `TimeStep` and never from a timer of its own. |
| `TicketStore` | Filling `JoinRoomOutcomePayload::player_game_token`, so a room resolves the connecting player from a one-use ticket instead of trusting a URL. Placement, not authentication: supply your own signed value via `issue_with`. Ships as `MapTicketRegistry`, or as `CachedTicketRegistry` behind the `cache` feature when you would rather `fibre_cache`'s janitor drove expiry than sweep from `issue`. |
| `routing` | Placing a connection in the tightest room its measured latency can carry. |

All four are exercised by [`examples/lobby_world`](../examples/lobby_world/).
