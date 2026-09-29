# lobby_world

Three arenas behind one lobby. The lobby admits you to an arena based on your measured link and your wallet carries over from one arena to the next.

This is the `plaza_lobby` example. That crate was the least-demonstrated block in the workspace: `horde_playground` borrows the free routing function and the room metadata and nothing else, so `RoomFactory`, `InMemoryLobbyManager` and `InProcessRoomHandle` had only been exercised by their own unit tests until this example ran them. `parlour_game` has since built on `RoomFactory` and `InProcessRoomHandle` as well. A room is spawned on demand with its own session and its own controller, the lobby measures a link and admits or refuses on it and a player carries a balance from one room to the next.

The game inside each arena is deliberately thin: a pot refills on a timer and whoever claims it keeps the coins. It only needs to make a wallet worth carrying.

## Running it

```sh
./run.sh                                   # http://127.0.0.1:8090, from anywhere
```

There is one script instead of the three the playgrounds have. There is no wasm step here, because the browser client is a plain HTML page in `static/` served by the same actix app as the sockets. The script's advantage over `cargo run` is that it works from any directory, since the examples are their own workspace.

The plain form, from inside `examples/`:

```sh
cargo run -p plaza_example_lobby_world     # http://127.0.0.1:8090
cargo test -p plaza_example_lobby_world    # tests for the rules described below
```

Open the page in **four tabs**. Each connection is assigned a different simulated link, in rotation, so the tabs disagree about which arenas they can play.

## The three arenas

| Arena | Seats | Schedule budget | Who can play it |
|---|---|---|---|
| `sprint` | 2 | 30 ms one way | the first tab only |
| `cruise` | 3 | 90 ms one way | the first three |
| `drift` | 4 | none | anybody |

A budget comes from the arena's own simulation rather than from lobby policy: an arena that schedules inputs ahead can only carry a connection whose delay fits inside the schedule. Past that, every input a player sends lands outside the accepting window and is dropped, so they would be seated and then unable to play.

## What you are looking at

| On screen | Meaning |
|---|---|
| **measured rtt** | what the transport timed with its own WebSocket ping. Zero for the first second: it pings eight times at 125 ms before settling, so the page re-lists to let it catch up |
| **assigned extra** | delay this example assigned to the connection, so a demo on localhost still has slow links in it |
| **one-way, judged** | `rtt / 2 + extra`. The number admission is actually decided on |
| **best fit / fits (#n)** | this arena's position in `rooms_playable_at`, tightest schedule first |
| **too slow for you** | the arena is listed but greyed. Refusal and an empty catalogue are different answers, so both are shown |
| **wallet** | the shared registry's number, not the arena's. It stays the same when you leave and join another arena |
| **claims here** | per-arena and resets on arrival, unlike the wallet |

## What the example covers

### 1. Spawning rooms on demand

Every arena, including the three at startup, is spawned through `RoomFactory::spawn_room`. There is no second path that builds one directly, so the startup catalogue goes through the factory too. The factory creates the arena's session, builds and spawns its controller, registers the socket so the HTTP layer can find it and hands back an `InProcessRoomHandle` holding the join handle that lets `reap_finished_rooms` know when the arena is done.

Dynamic rooms are also torn down cleanly. A dynamic room that carries no traffic for `ROOM_IDLE_AFTER` is drained by the sweep through `disconnect_all`: every occupant hears `Closed` before the socket goes, never a silent EOF. Only then is the controller told to shut down, so `reap_finished_rooms` collects the handle on a later pass. The teardown uses the same flush-then-farewell close as a kick. The three fixed arenas are never reaped.

`RoomFactory::GameStateType` is bound `Default`, but `ArenaState`'s derived `Default` is not a usable arena. The factory always builds it explicitly from the room's settings. The bound should be a constructor that takes those settings, which `Default` cannot express.

### 2. Admission on server-measured latency

`JoinRoomRequestPayload::measured_one_way_ms` is documented as needing a figure the *server* measured, because a client that reports its own latency can understate it and this gates entry. That is why the lobby here is a plaza controller on a WebSocket rather than an HTTP handler: the only place the measurement exists is on a socket the transport has been pinging. `ActixWsPlazaSession::agent_rtt` is where it comes from.

A refusal is `LobbyError::UnsuitableConnection`, which carries **both** numbers rather than a string. The client can therefore say what was measured against what is allowed and offer somewhere that fits. This is why latency admission belongs in a lobby rather than a room: a room can only accept or refuse, while a lobby can also point the player at an arena that fits.

### 3. A wallet that follows the player

A wallet cannot live on `Agent`, which holds identity only. It cannot live in an arena's state either, because moving to another arena destroys that. So it lives in a `WalletRegistry` the lobby and every arena share, keyed by the id the lobby issued. The registry keeps a balance when its player leaves a room and clears it only when the player leaves the world.

This follows from the `Agent` slimming and is why the registry is about forty lines in the example instead of a feature of the crate. Whether a balance outlives a room, a session or a process is for the application to decide. The implementation here is just a `Mutex` around a map.

### 4. Spectating without a seat

Spectating deliberately does **not** go through `handle_join_room_request`. A spectator consumes no capacity and runs no schedule, so neither the seat count nor the latency budget applies to watching and a spectator can watch a full arena. The lobby's accounting never sees a spectator.

The arena decides the seat itself, from a reservation the lobby places ahead through `RoomHandle::reserve_seat`; the in-process handle turns that into the arena's own `RoomOp::Reserve`, so the seam still names no game type. Without that an arena could not tell an admitted player from a passer-by and would seat whoever arrived until it filled, ignoring the lobby's capacity accounting.

A reservation is cancelled by the lobby or lapses after `RESERVATION_WINDOW` (45 seconds) if nobody dials in, never by a closing socket. The second bug described below came from getting that wrong.

### 5. Fewer, complete frames for a slow link

Every tab is assigned a different simulated delay, so the same arena serves links of 0, 25, 70 and 140 ms one way. The arena ticks at 20 Hz and a 140 ms link is not sent twenty snapshots a second: the lobby declares the admitted link to the arena beside the reservation (`RoomOp::Link`), `snapshot_budget` turns it into an `OutboundBudget` on that seat's connection when it joins and `ArenaSnapshotter` asks `agent_owed` before building a viewer's snapshot, answering `Ok(None)` when the budget has no credit. A 25 ms link keeps the tick rate, 70 ms gets ten a second and 140 ms gets four. The transport withholds nothing: ops and events still go, they are charged to the same credit and the next snapshot the viewer does get is the whole view, so the skip costs latency and never correctness.

This arena publishes on change rather than every tick: a snapshot goes out when the pot refreshes or somebody claims, which four browser tabs measured at about one arena frame every five seconds. That is under every budget in the table, so the skip is wired here and never fires. It binds in an arena that snapshots at its tick rate, which is horde at player count.

## What it shows about plaza

**The seat check sits inside the rules.** `RoomOp::Reserve` is server-originated and is the only thing standing between a client and a free seat, so the arena checks `source.is_system()` inside the rule that acts on it. When this example was written there was nowhere else to put it. Core now has that hook, `plaza::OpGuard`, which the controller runs per op before `process_input`, but this example does not use it yet.

**`JoinRoomOutcomePayload::player_game_token` was unused before this example.** Without it the arena URL would have to carry the player id and a client that can name its own id can name someone else's and take their wallet. The lobby mints a one-use ticket and the arena route resolves it, so identity comes from the lobby rather than from the client. The ticket itself is a counter and can be guessed in one try. It shows *where the check goes* and is not a real credential, because plaza has no authentication design yet for it to follow.

## Verified end to end

`cargo test` covers the seat, wallet and ticket rules directly. The socket-level flow was also driven against a running server: identity and placement, the ticket refusing a replay and refusing an absent ticket, a claim crediting the shared registry, the wallet arriving intact in a second arena, four connections receiving four different catalogues, a refusal carrying both numbers, a spectator bypassing the latency gate without taking a seat or being able to claim and the arena's seat count flowing back into the lobby's own `RoomMetadata`.

That run also found two bugs the unit tests could not have caught, because both are about ordering between two connections and the tests only ever had one.

**The pot had no ceiling.** An idle arena turned server uptime into coins and the first arrival after 70 seconds took 445 of them. Every wallet in the readout then depended on how long the process had been up rather than on play and the wallet is the number this example is meant to show moving between arenas. The pot is capped now. Two tests cover the ceiling and check that a full pot announces nothing.

**A closing socket cancelled a reservation, so spectating an arena and then joining it left you a spectator.** The lobby said `Placed { spectator: false }` and the arena seated you as a spectator. The two halves disagreed and neither reported an error. The order of events was:

1. the lobby reserves your seat;
2. your page closes the spectator socket to open the new one and the arena sees `AgentLeft`;
3. `AgentLeft` cleared the reservation, so the new connection arrived unreserved.

The fix is to let the lobby cancel a reservation instead of the transport. This is the same rule [`ReconnectTracker`](../../core/src/common/reconnect.rs) follows: plaza reports a dropped connection immediately and leaves the application to decide what it means. Here it means "the same player, one second later" rather than "gone". So `AgentLeft` frees the seat but keeps the reservation. A new `RoomOp::Withdraw` cancels it once the lobby knows the player was placed elsewhere or left. The lobby tracks where each outstanding reservation is, because ids are issued per lobby connection and one that is never consumed can never be consumed later.

Joining a *different* arena worked throughout, which is why the one-connection tests and a casual play-through both missed the bug.
