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
    let session = ActixWsPlazaSession::<MyOp, PlayerId, MySnapshot>::new();
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
