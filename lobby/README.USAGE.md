# Usage Guide: plaza_lobby

How to run rooms above a controller: building one of your game, creating and listing and joining, routing a connection to a room its latency can carry, queueing players who would rather be paired than choose, holding a seat between admission and arrival, and reaping what has finished.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [A Room Factory](#a-room-factory)
    *   [The Manager](#the-manager)
*   [Room Lifecycle](#room-lifecycle)
    *   [Creating a Room](#creating-a-room)
    *   [Listing Rooms](#listing-rooms)
    *   [Joining a Room](#joining-a-room)
    *   [Reaching a Specific Room](#reaching-a-specific-room)
    *   [Reaping](#reaping)
*   [Passwords](#passwords)
*   [Routing by Latency](#routing-by-latency)
    *   [Stating What a Room Can Carry](#stating-what-a-room-can-carry)
    *   [Supplying the Measurement](#supplying-the-measurement)
    *   [Placing Rather Than Refusing](#placing-rather-than-refusing)
*   [Pairing Players Who Do Not Choose](#pairing-players-who-do-not-choose)
*   [Holding a Seat Between Admission and Arrival](#holding-a-seat-between-admission-and-arrival)
*   [Handing Out a Join Ticket](#handing-out-a-join-ticket)
    *   [The Two Registries](#the-two-registries)
    *   [Issuing Your Own Value](#issuing-your-own-value)
*   [Scope](#scope)
*   [Error Handling](#error-handling)

## Core Concepts

*   **Room**: one game, running as its own `StateController` task with its own transport endpoint.
*   **`RoomFactory`**: how a room of *your* game is built. The one thing you implement.
*   **`RoomHandle`**: what the lobby needs from a room. `InProcessRoomHandle` implements it for a task in this process.
*   **`InMemoryLobbyManager`**: the registry and the create, join, list and reap flows around your factory.
*   **Authorization, not connection**: a successful join means the lobby authorized a player and returned an endpoint. The lobby never proxies gameplay traffic.
*   **`RoomMetadata`**: what a client is shown about a room. Reports `has_password`, never the hash.
*   **`max_one_way_ms`**: the worst one-way delay a room's simulation can carry. Stated by the room, because nothing above it knows the number.
*   **`MatchQueue`**: for players who would rather be paired than choose. Forms full matches and reports how many seats to fill with bots when patience runs out.
*   **`SeatReservations`**: the gap between being admitted and arriving.
*   **`TicketStore`**: a one-use token a room resolves a connecting player from, instead of trusting a URL.

## Quick Start

### A Room Factory

Plaza cannot know how a room of your game is built, so this is the one trait you implement. Inside `spawn_room` you build a `StateController` as usual, spawn its `run()`, and wrap the pieces.

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
```

### The Manager

```rust,ignore
let lobby = InMemoryLobbyManager::new(Arc::new(MyGameFactory))
  .with_password_verifier(Arc::new(|attempt, hash| argon2_verify(attempt, hash)));
```

## Room Lifecycle

### Creating a Room

```rust,ignore
let metadata = lobby.handle_create_room_request(&requester, RoomSettings {
  name: Some("Table 4".to_owned()),
  game_mode: "deathmatch".to_owned(),
  max_players: 8,
  is_private: false,
  password_hash: None,
  custom_game_settings: MySettings::default(),
}).await?;
```

The manager generates the `RoomId` before calling your factory. A factory error propagates and leaves no room behind.

### Listing Rooms

```rust,ignore
let all = lobby.list_rooms(None);

let joinable = lobby.list_rooms(Some(&RoomFilters {
  game_mode: Some("deathmatch".to_owned()),
  exclude_full: Some(true),
  playable_at_one_way_ms: Some(measured),
  ..Default::default()
}));
```

### Joining a Room

```rust,ignore
let outcome = lobby.handle_join_room_request(&player, agent, &JoinRoomRequestPayload {
  room_id,
  password_attempt: None,
  measured_one_way_ms: Some(one_way),
}).await?;

send_to_client(outcome.room_session_endpoint, outcome.player_game_token);
```

In order: find the room, verify the password if it has one, check capacity, then ask the room to accept the player. The room gets the last word, because it may have filled since the lobby checked.

A successful join is an **address**, not a connection. The gameplay join happens when the client connects to the room's own transport.

### Reaching a Specific Room

```rust,ignore
if let Some(room) = lobby.room(&room_id) {
  room.update_player_count_in_metadata(count);
  room.command_tx.send(ControllerCommand::SubmitSystemOps { /* ... */ }).await?;
}

for room in lobby.rooms() { /* ... */ }
```

### Reaping

Nothing reaps automatically.

```rust,ignore
// From a scheduled job, or your own tick.
lobby.reap_finished_rooms().await;
lobby.handle_player_leaving_lobby(&player_id).await;
```

It removes rooms whose controller task has ended, requesting shutdown on each and clearing player assignments.

## Passwords

```rust,ignore
let lobby = InMemoryLobbyManager::new(factory)
  .with_password_verifier(Arc::new(|attempt, hash| argon2_verify(attempt, hash)));
```

The default is plain string equality, which suits low-stakes room codes and nothing else. The client sends plaintext in `password_attempt`; the verifier compares it against the stored hash. `RoomMetadata` exposes only `has_password`.

## Routing by Latency

A game that schedules inputs ahead can only carry a connection whose delay fits inside the schedule. Past that, every input lands outside the accepting window and is dropped, so a player is seated and then cannot play, which reads as a broken game rather than an unsuitable connection.

### Stating What a Room Can Carry

```rust,ignore
RoomMetadata { max_one_way_ms: Some(60), .. }   // this schedule tolerates 60ms
RoomMetadata { max_one_way_ms: None, .. }       // applies input on arrival: nothing to miss
```

The limit is a property of that room's simulation, so the room states it.

### Supplying the Measurement

**The lobby owns no socket**, and the number must be one the *server* measured rather than one the client reported, since a client can understate its own latency and this decides entry.

```rust,ignore
let (rtt, samples) = session.agent_rtt(&id).unwrap_or_default();
lobby.handle_join_room_request(&id, agent, &JoinRoomRequestPayload {
  room_id,
  password_attempt: None,
  measured_one_way_ms: (samples >= 8).then(|| rtt.as_millis() as u32 / 2),
}).await
```

### Placing Rather Than Refusing

A room can only say yes or no. A lobby can say *where*.

```rust,ignore
let options = lobby.rooms_playable_at(one_way);     // tightest schedule first
let best = plaza_lobby::routing::best_for(one_way, options.clone());
```

A fast link is not sent to the room built for slow ones and made to pay its delay. A room with no limit sorts last, since it takes anybody and is the fallback.

When nothing fits, refusal carries both numbers:

```rust,ignore
Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => {
  tell_client(measured_ms, allowed_ms);
}
```

## Pairing Players Who Do Not Choose

Each of these is bookkeeping your own `StateLogic` drives. No timers, no tasks.

```rust,ignore
let mut queue: MatchQueue<PlayerId, u64> = MatchQueue::new(4);   // seats per match

queue.enqueue(player, now);
for Formed { players, bots_needed } in queue.form(now) {
  let room = lobby.handle_create_room_request(&host, settings.clone()).await?;
  seat(room.room_id, players);
  fill_with_bots(room.room_id, bots_needed);
}
```

When patience runs out it reports how many seats to fill with bots rather than refusing to start.

## Holding a Seat Between Admission and Arrival

```rust,ignore
let mut reservations: SeatReservations<PlayerId> = SeatReservations::new().with_expiry(30_000);

reservations.reserve(room_id, player, now);
// On arrival:
reservations.claim(room_id, &player);
// From your TimeStep, never from a timer of its own:
for expired in reservations.sweep(now) { free_seat(expired); }
```

**A closing socket deliberately does not cancel a reservation.** A room hop closes the old connection *after* the new seat is reserved, so treating a disconnect as a cancellation silently demotes a player the lobby already promised.

## Handing Out a Join Ticket

So a room resolves the connecting player from a one-use ticket instead of trusting a URL.

```rust,ignore
let mut tickets = MapTicketRegistry::new();

let ticket = tickets.issue(player.clone(), room_id, now);
outcome.player_game_token = Some(ticket.value.clone());

// In the room's own route:
match tickets.redeem(&token, now) {
  Some(Ticket { holder, .. }) => seat(holder),
  None => refuse(),
}
```

This is **placement, not authentication**.

### The Two Registries

```rust,ignore
MapTicketRegistry::new()      // sweeps from issue
CachedTicketRegistry::new()   // feature `cache`: fibre_cache's janitor drives expiry
```

### Issuing Your Own Value

```rust,ignore
tickets.issue_with(player, room_id, now, sign(player, room_id));
```

Supply your own signed value when the token has to survive being handled by something you do not control.

## Scope

Single server. Rooms are in-process tasks, and nothing here coordinates across machines; that stays an application concern.

All four blocks around the manager are exercised by [`examples/lobby_world`](../examples/lobby_world/).

## Error Handling

`LobbyError` is the one error type, and every flow returns it.

```rust,ignore
match lobby.handle_join_room_request(&id, agent, &payload).await {
  Ok(outcome) => outcome,
  Err(LobbyError::RoomNotFound(id)) => return no_such_room(id),
  Err(LobbyError::RoomFull { .. }) => return full(),
  Err(LobbyError::InvalidPassword) => return wrong_password(),
  Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => {
    return too_slow(measured_ms, allowed_ms);
  }
  Err(e) => return internal(e),
}
```

`UnsuitableConnection` is its own variant rather than a string because it is the one refusal a client can act on, and both numbers belong in it: a client that knows it was measured at 140ms against a 60ms limit can say so, or go looking for a room that fits.

A factory error propagates out of `handle_create_room_request` and leaves no room registered, so a half-built room is never listed.
