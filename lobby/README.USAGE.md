# Usage Guide: plaza_lobby

How to run rooms above a controller: building one of your game, creating and listing and joining, routing a connection to a room its latency can carry, queueing players who would rather be paired than choose, holding a seat between admission and arrival and reaping what has finished.

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
    *   [Picking a Room for the Connection](#picking-a-room-for-the-connection)
*   [Pairing Players Who Do Not Choose](#pairing-players-who-do-not-choose)
*   [Holding a Seat Between Admission and Arrival](#holding-a-seat-between-admission-and-arrival)
*   [Reserving a Seat Through the Room](#reserving-a-seat-through-the-room)
    *   [Teaching the Handle the Room's Ops](#teaching-the-handle-the-rooms-ops)
    *   [Holding and Releasing a Seat From the Lobby](#holding-and-releasing-a-seat-from-the-lobby)
    *   [Redeeming the Hold in the Room](#redeeming-the-hold-in-the-room)
    *   [Handling a Room That Takes No Reservations](#handling-a-room-that-takes-no-reservations)
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
*   **Join**: a successful join means the lobby authorized a player and returned an endpoint. The lobby never proxies gameplay traffic.
*   **`RoomMetadata`**: what a client is shown about a room. Reports `has_password`, never the hash.
*   **`max_one_way_ms`**: the worst one-way delay a room's simulation can carry. Stated by the room, because nothing above it knows the number.
*   **`MatchQueue`**: for players who would rather be paired than choose. Forms full matches and reports how many seats to fill with bots when patience runs out.
*   **`SeatReservations`**: holds a seat between admission and arrival.
*   **Reservation seam**: `RoomHandle::reserve_seat` and `withdraw_seat`, which the lobby calls without knowing the room's op type. The room keeps the hold itself.
*   **`TicketStore`**: a one-use token a room resolves a connecting player from, instead of trusting a URL.

## Quick Start

### A Room Factory

Plaza cannot know how a room of your game is built, so this is the one trait you implement. Inside `spawn_room` you build a `StateController` as usual, spawn its `run()` and wrap the pieces.

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
  ) -> Result<Arc<dyn RoomHandle<PlayerId, MySettings>>, LobbyError> {
    let session = ActixWsPlazaSession::<MyOp, PlayerId>::new();
    let (command_tx, controller) = StateControllerBuilder::new(
      Arc::new(MyLogic), session.clone(), Arc::new(MySnapshotter), MyState::default(),
    ).build();
    let task = tokio::spawn(controller.run());

    Ok(Arc::new(InProcessRoomHandle::new(
      room_id,
      RoomMetadata { /* from settings */ },
      command_tx,
      task,
      format!("ws://host/game/{room_id}"),
      settings.password_hash.clone(),
    )))
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

In order: find the room, verify the password if it has one, check capacity, then ask the room to accept the player. The room decides last, because it may have filled since the lobby checked.

A successful join returns an **address**. The gameplay join happens when the client connects to the room's own transport.

### Reaching a Specific Room

```rust,ignore
use plaza_lobby::RoomHandle;

if let Some(room) = lobby.room(&room_id) {
  let players = room.metadata().current_players;
  let endpoint = room.session_endpoint_info();
  if room.is_finished() { /* ... */ }
}

for room in lobby.rooms() { /* ... */ }
```

`room` and `rooms` return `Arc<dyn RoomHandle<..>>`, so only the trait's methods are available. To send a room a `ControllerCommand` or call `InProcessRoomHandle::update_player_count_in_metadata`, keep the `CommandSender` or the concrete handle from your factory in a map of your own keyed by `RoomId`.

### Reaping

Nothing reaps automatically.

```rust,ignore
// From a scheduled job or your own tick.
lobby.reap_finished_rooms().await;
lobby.handle_player_leaving_lobby(&player_id).await;
```

It removes rooms whose controller task has ended, requesting shutdown on each and clearing player assignments.

## Passwords

```rust,ignore
let lobby = InMemoryLobbyManager::new(factory)
  .with_password_verifier(Arc::new(|attempt, hash| argon2_verify(attempt, hash)));
```

The default is plain string equality, which is only suitable for low-stakes room codes. The client sends plaintext in `password_attempt`; the verifier compares it against the stored hash. `RoomMetadata` exposes only `has_password`.

## Routing by Latency

A game that schedules inputs ahead can only carry a connection whose delay fits inside the schedule. Past that, every input lands outside the accepting window and is dropped, so a player is seated and then cannot play. To the player that looks like a broken game rather than an unsuitable connection.

### Stating What a Room Can Carry

```rust,ignore
RoomMetadata { max_one_way_ms: Some(60), .. }   // this schedule tolerates 60ms
RoomMetadata { max_one_way_ms: None, .. }       // applies input on arrival: nothing to miss
```

The limit is a property of that room's simulation, so the room states it.

### Supplying the Measurement

**The lobby owns no socket** and the number must be one the *server* measured rather than one the client reported, since a client can understate its own latency and this decides entry.

```rust,ignore
let (rtt, samples) = session.agent_rtt(&id).unwrap_or_default();
lobby.handle_join_room_request(&id, agent, &JoinRoomRequestPayload {
  room_id,
  password_attempt: None,
  measured_one_way_ms: (samples >= 8).then(|| rtt.as_millis() as u32 / 2),
}).await
```

### Picking a Room for the Connection

A room can only accept or refuse a connection. The lobby can also pick which room it goes to.

```rust,ignore
let options = lobby.rooms_playable_at(one_way);     // tightest schedule first
let best = plaza_lobby::routing::best_for(one_way, options.clone());
```

Sorting tightest first keeps a fast link out of a room built for slow ones. A room with no limit sorts last, since it takes anybody and is the fallback.

When nothing fits, refusal carries both numbers:

```rust,ignore
Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => {
  tell_client(measured_ms, allowed_ms);
}
```

## Pairing Players Who Do Not Choose

`MatchQueue` is bookkeeping your own `StateLogic` drives. It holds no timers and spawns no tasks.

```rust,ignore
let mut queue: MatchQueue<PlayerId, u64> = MatchQueue::new(4, patience);   // seats per match, wait before bots fill

queue.enqueue(player, now);
for Formed { players, bots, .. } in queue.drain_ready(now) {
  let room = lobby.handle_create_room_request(&host, settings.clone()).await?;
  seat(room.room_id, players);
  fill_with_bots(room.room_id, bots);
}
```

When patience runs out it reports how many seats to fill with bots rather than refusing to start.

## Holding a Seat Between Admission and Arrival

```rust,ignore
let mut reservations: SeatReservations<PlayerId> = SeatReservations::with_expiry(Duration::from_secs(30));

if !reservations.reserve(player) {
  // this player already held a seat; the first promise stands
}
// On AgentJoined:
let admitted = reservations.consume(&player);
// From LogicInput::TimeStep, with the same delta_time:
for lapsed in reservations.tick(delta_time) { free_seat(lapsed); }
// From the lobby, when the player was placed elsewhere:
reservations.withdraw(&player);
```

`SeatReservations::new()` holds a reservation until it is consumed or withdrawn. The type never reads a clock: `tick` advances its own.

**A closing socket deliberately does not cancel a reservation.** A room hop closes the old connection *after* the new seat is reserved, so treating a disconnect as a cancellation silently demotes a player the lobby already promised.

## Reserving a Seat Through the Room

The hold lives in the room's own state, because the room is what seats a player when they connect. The lobby reaches it through `RoomHandle`, which names no game type. `examples/parlour_game` wires this up end to end. The full surface is under [`RoomHandle`](API_REFERENCE.md#trait-roomhandlegameagentid-agentid-customroomsettings) and [`InProcessRoomHandle`](API_REFERENCE.md#struct-inprocessroomhandlegameop-gameid-gamestatetype-customroomsettings).

### Teaching the Handle the Room's Ops

The factory tells the handle how this room spells a reservation and a withdrawal in its own ops.

```rust,ignore
let room: Arc<dyn RoomHandle<PlayerId, MySettings>> = Arc::new(
  InProcessRoomHandle::new(
    room_id,
    metadata,
    command_tx,
    task,
    endpoint,
    settings.password_hash.clone(),
  )
  .with_reservations(|player| MyOp::Reserve { player }, |player| MyOp::Withdraw { player }),
);
```

`reserve_seat` and `withdraw_seat` then submit those ops to the room's controller as system ops.

### Holding and Releasing a Seat From the Lobby

```rust,ignore
use plaza_lobby::RoomHandle;

if let Some(room) = lobby.room(&room_id) {
  room.reserve_seat(&player).await?;
}

// The player left the lobby or was placed somewhere else:
if let Some(room) = lobby.room(&previous_room_id) {
  room.withdraw_seat(&player).await?;
}
```

Withdraw the old seat before reserving a new one when a player is moved, so the first room does not keep counting a seat nobody will fill. If the room's controller has already ended, both calls return `LobbyError::InternalOrchestrationError`.

### Redeeming the Hold in the Room

The room keeps a `SeatReservations`, fills it from the system ops and spends it on `AgentJoined`.

```rust,ignore
LogicInput::AgentOps { source, ops } => {
  let system = source.is_system();
  for op in ops {
    match op {
      MyOp::Reserve { player } if system => {
        state.reserved.reserve(player);
      }
      MyOp::Withdraw { player } if system => {
        state.reserved.withdraw(&player);
      }
      MyOp::Reserve { .. } | MyOp::Withdraw { .. } => {
        // A client sent it: refuse.
      }
      // ...
    }
  }
}

LogicInput::AgentJoined { agent } => {
  let Some(player) = agent.id_cloned() else { return Ok(LogicOutput::none()) };
  let seat = if state.reserved.consume(&player) { Seat::Player } else { Seat::Spectator };
  // ...
}
```

Accept `Reserve` and `Withdraw` only from a system source. A client that could send them would seat itself. An [`OpGuard`](../core/README.USAGE.md#authorizing-ops) can do the same screening ahead of the rules. `consume` spends the hold once, so a second connection on the same id arrives as a spectator. Leave the hold alone on `AgentLeft`; only `Withdraw` cancels it.

### Handling a Room That Takes No Reservations

Without `with_reservations`, both calls answer `LobbyError::NotImplemented`. The same default applies to a `RoomHandle` you write yourself until you implement the two methods.

```rust,ignore
match room.reserve_seat(&player).await {
  Ok(()) => {}
  Err(LobbyError::NotImplemented(_)) => {}   // this room seats whoever connects
  Err(error) => warn!(?error, "seat not held"),
}
```

## Handing Out a Join Ticket

A ticket lets a room resolve the connecting player from a one-use token instead of trusting a URL.

```rust,ignore
let tickets: Arc<dyn TicketStore<PlayerId>> = Arc::new(MapTicketRegistry::with_expiry(Duration::from_secs(20)));

let token = tickets.issue(player.clone(), room_id);
outcome.player_game_token = Some(token);

// In the room's own route:
match tickets.redeem(&token, &room_id) {
  Some(Ticket { player, .. }) => seat(player),
  None => refuse(),   // never issued, already used, expired or issued for another room
}
```

`redeem` checks the room before it spends the ticket, so a token presented at the wrong door is refused and stays valid. `revoke(&token)` drops a ticket the lobby has since cancelled. Keep the ticket window shorter than the [reservation](#holding-a-seat-between-admission-and-arrival) window; otherwise a player lands with a spent ticket and no seat.

The ticket handles **placement** only and does not authenticate anyone.

### The Two Registries

```rust,ignore
MapTicketRegistry::new()                                       // never expires
MapTicketRegistry::with_expiry(Duration::from_secs(20))        // sweeps on issue, once per window
CachedTicketRegistry::with_expiry(Duration::from_secs(20))     // feature `cache`: fibre_cache's janitor drives expiry
```

Both implement `TicketStore`, so a route written against `Arc<dyn TicketStore<ID>>` works with either.

### Issuing Your Own Value

```rust,ignore
let token = sign(&player, &room_id);
tickets.issue_with(token.clone(), player, room_id);
```

Supply your own signed value when the token has to survive being handled by something you do not control.

## Scope

Single server. Rooms are in-process tasks and nothing here coordinates across machines; that stays an application concern.

All four blocks around the manager are exercised by [`examples/lobby_world`](../examples/lobby_world/).

## Error Handling

`LobbyError` is the one error type and every flow returns it.

```rust,ignore
match lobby.handle_join_room_request(&id, agent, &payload).await {
  Ok(outcome) => outcome,
  Err(LobbyError::RoomNotFound(id)) => return no_such_room(id),
  Err(LobbyError::JoinRoomFailed(why)) => return refused(why),   // full, wrong or missing password
  Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => {
    return too_slow(measured_ms, allowed_ms);
  }
  Err(e) => return internal(e),
}
```

`UnsuitableConnection` is its own variant rather than a string because it is the one refusal a client can act on and both numbers belong in it: a client that knows it was measured at 140ms against a 60ms limit can say so or go looking for a room that fits.

A factory error propagates out of `handle_create_room_request` and leaves no room registered, so a half-built room is never listed.
