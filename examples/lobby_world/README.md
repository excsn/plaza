# lobby_world

Three arenas behind one lobby, where **your link decides where you fit and your wallet comes with you**.

This is the `plaza_lobby` example. That crate has been the least-demonstrated block in the workspace: `horde_playground` borrows the free routing function and nothing else, so `RoomFactory`, `InMemoryLobbyManager` and `InProcessRoomHandle` have only ever been exercised by their own unit tests. Here they run. A room is spawned on demand with its own session and its own controller, the lobby measures a link and admits or refuses on it, and a player carries a balance from one room to the next.

The game inside each arena is deliberately thin: a pot refills on a timer and whoever claims it keeps the coins. It only has to be interesting enough that a wallet is worth carrying.

## Running it

```sh
./run.sh                                   # http://127.0.0.1:8090, from anywhere
```

One script, not the three the playgrounds carry: there is no wasm step here, because the browser client is plain HTML embedded with `include_str!` and served by the same actix app as the sockets. What the script buys over `cargo run` is working from any directory, since the examples are their own workspace.

The plain form, from inside `examples/`:

```sh
cargo run -p plaza_example_lobby_world     # http://127.0.0.1:8090
cargo test -p plaza_example_lobby_world    # every claim below, as a test
```

Open the page in **four tabs**. Each connection is assigned a different simulated link, in rotation, so the tabs disagree about which arenas they can play. That is the whole demonstration.

## The three arenas

| Arena | Seats | Schedule budget | Who can play it |
|---|---|---|---|
| `sprint` | 2 | 30 ms one way | the first tab only |
| `cruise` | 3 | 90 ms one way | the first three |
| `drift` | 4 | none | anybody |

A budget is a property of the arena's own simulation, not a policy the lobby invented: an arena that schedules inputs ahead can only carry a connection whose delay fits inside the schedule. Past that, every input a player sends lands outside the accepting window and is dropped, so they would be seated and then unable to play.

## What you are looking at

| On screen | Meaning |
|---|---|
| **measured rtt** | what the transport timed with its own WebSocket ping. Zero for the first second: it pings eight times at 125 ms before settling, so the page re-lists to let it catch up |
| **assigned extra** | delay this example assigned to the connection, so a demo on localhost still has slow links in it |
| **one-way, judged** | `rtt / 2 + extra`. The number admission is actually decided on |
| **best fit / fits (#n)** | this arena's position in `rooms_playable_at`, tightest schedule first |
| **too slow for you** | the arena is listed but greyed. Refusal and an empty catalogue are different answers, so both are shown |
| **wallet** | the shared registry's number, not the arena's. Leave, join elsewhere, watch it follow |
| **claims here** | per-arena, and resets on arrival. The contrast with the wallet is the point |

## The four things this example is for

### 1. A room that did not exist a moment ago

Every arena, including the three at startup, is spawned through `RoomFactory::spawn_room`. There is no second path that builds one directly, so the startup catalogue is a real exercise of the factory rather than a shortcut around it. The factory creates the arena's session, builds and spawns its controller, registers the socket so the HTTP layer can find it, and hands back an `InProcessRoomHandle` holding the join handle that lets `reap_finished_rooms` know when the arena is done.

And a room that stops existing does it politely. A dynamic room that carries no traffic for `ROOM_IDLE_AFTER` is drained by the sweep through `disconnect_all`: every occupant hears `Closed` ahead of the socket going, never a silent EOF, and only then is the controller told to shut down, so `reap_finished_rooms` collects the handle on a later pass. The teardown is the same flush-then-farewell close a kick uses, which is the point: a room ending and a guest being removed are the same mechanism at different scopes. The three fixed arenas are the standing offering and are never reaped.

One wrinkle worth naming: `RoomFactory::GameStateType` is bound `Default`, and `ArenaState`'s derived `Default` is not a usable arena. The factory always builds it explicitly from the room's settings. A `Default` that nothing should call is a bound asking for a constructor it cannot name.

### 2. Admission on a number the client cannot lie about

`JoinRoomRequestPayload::measured_one_way_ms` is documented as needing a figure the *server* measured, because a client that reports its own latency can understate it and this gates entry. That is why the lobby here is a plaza controller on a WebSocket rather than an HTTP handler: the only place the measurement exists is on a socket the transport has been pinging. `ActixWsPlazaSession::agent_rtt` is where it comes from.

A refusal is `LobbyError::UnsuitableConnection`, which carries **both** numbers rather than a string. The client can therefore say what was measured against what is allowed, and offer somewhere that fits. That is the reason latency admission belongs to a lobby rather than a room: a room can only say yes or no, and a lobby can say *where*.

### 3. Travel with baggage

A wallet cannot live on `Agent`, which is identity and nothing else, and it cannot live in an arena's state, which is what the trip destroys. So it lives in a `WalletRegistry` the lobby and every arena share, keyed by the id the lobby issued. Leaving a room is exactly the case it exists to survive; only leaving the world clears it.

This is the design line the `Agent` slimming drew, and it is why the registry is thirty lines in the example rather than a feature of the crate. Whether a balance survives a room, a session, or a process is an application's question, and a `Mutex` around a map is the whole implementation.

### 4. A spectator is not a seat

Spectating deliberately does **not** go through `handle_join_room_request`. A spectator consumes no capacity and runs no schedule, so neither the seat count nor the latency budget applies to watching, and a full arena is exactly when spectating is interesting. The lobby's accounting must never see one.

The arena decides the seat itself, from a reservation the lobby sends ahead over the room's command channel. Without that an arena could not tell an admitted player from a passer-by and would seat whoever arrived until it filled, which would make the lobby's capacity accounting decorative.

A reservation is cancelled only by the lobby, never by a closing socket. See below: that distinction is the whole of one bug.

## Two things it says about plaza rather than about itself

**There is no authorization hook ahead of `StateLogic`.** `RoomOp::Reserve` is server-originated and is the only thing standing between a client and a free seat, so the arena checks `source.is_system()` inside the rule that acts on it. Security and simulation end up mixed together because there is nowhere else to put the check. This is an open item, and this example is a small argument for it.

**`JoinRoomOutcomePayload::player_game_token` was an unused field.** It is used here. Without it the arena URL would have to carry the player id, and a client that can name its own id can name someone else's and walk off with their wallet. The lobby mints a one-use ticket and the arena route resolves it, so identity arrives from the lobby rather than from the client. The ticket itself is a counter, guessable in one try: it demonstrates *where the check goes*, not how to build a credential, and the reason it is not a real one is that plaza has no authentication story to be consistent with yet.

## Verified end to end

`cargo test` covers the seat, wallet and ticket rules directly. The socket-level flow was also driven against a running server: identity and placement, the ticket refusing a replay and refusing an absent ticket, a claim crediting the shared registry, the wallet arriving intact in a second arena, four connections receiving four different catalogues, a refusal carrying both numbers, a spectator bypassing the latency gate without taking a seat or being able to claim, and the arena's seat count flowing back into the lobby's own `RoomMetadata`.

That run also found two bugs worth recording, neither of which the unit tests could have caught, because both are about *ordering between two connections* and the tests only ever had one.

**The pot had no ceiling.** An idle arena turned server uptime into coins, and the first arrival after 70 seconds scooped 445 of them. Every wallet in the readout became a function of how long the process had been up rather than of play, which is precisely the number the example exists to show travelling. Capped now, with two tests holding the ceiling and the silence at it.

**A closing socket cancelled a reservation, so spectating an arena and then joining it left you a spectator.** The lobby said `Placed { spectator: false }` and the arena seated you as a spectator, which is the bad kind of bug: the two halves disagreed and neither complained. The order is the whole story:

1. the lobby reserves your seat;
2. your page closes the spectator socket to open the new one, and the arena sees `AgentLeft`;
3. `AgentLeft` cleared the reservation, so the new connection arrived unreserved.

The fix is to make cancellation the lobby's word rather than the transport's, which is [`ReconnectTracker`'s lesson](../../core/src/common/reconnect.rs) arriving from a different direction: **plaza reports a dropped connection immediately and deliberately does not decide what it means**, and here it means "the same player, one second later", not "gone". So `AgentLeft` frees the seat and keeps the reservation, and a new `RoomOp::Withdraw` cancels it when the lobby actually knows: the player was placed elsewhere, or left. The lobby tracks where each outstanding reservation is, because ids are issued per lobby connection and one that is never consumed can never be consumed later.

Worth generalising: **an arena cannot infer intent from a disconnect, and a system that lets it try will be wrong in exactly the cases that involve two connections.** Joining a *different* arena worked throughout, which is why one-connection tests and a casual play-through both missed it.
