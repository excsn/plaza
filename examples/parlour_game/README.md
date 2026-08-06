# parlour_game

A card game **behind a lobby**: press play, wait, get dropped into a table that did not exist a moment ago.

This is the shape a matchmade turn-based game takes, and it is the one combination the other examples do not cover between them. [`card_table`](../card_table/) has the turns, rounds, phases and hidden hands but one socket and no lobby. [`lobby_world`](../lobby_world/) has quick match, tickets and rooms-on-demand but a real-time arena and JSON throughout. Neither had ever met the other, and nothing in the workspace had run the placement path over a binary wire or spawned a room **per match** rather than placing into a standing one.

## Running it

```sh
./run.sh                                        # http://127.0.0.1:8092, from anywhere
cargo test -p plaza_example_parlour_game        # every claim below, as a test
cargo run -p plaza_example_parlour_game --example parlour_report   # what the field names cost
```

Open the page in **three tabs** and press quick match in each, or press it in one and wait twelve seconds for the queue to run out of patience and fill the other two seats with bots.

## The four things this example is for

### 1. A room created for the match, not found for it

`lobby_world` places you into one of three standing arenas: `rooms_playable_at(...).find(a free seat)`. A card game does not work that way. Three people are matched and a table is *created* for them, so the room and the match have the same lifetime and every table is eventually reaped.

That is a two-line difference in `seat_formed` and a completely different lifecycle around it: `handle_create_room_request` runs inside the match-forming path, `max_players` is the size of the match rather than a property of the room, and nothing is pre-spawned at boot.

### 2. Two codecs, one server, one port

The lobby session speaks `JsonCodec`. Every table session speaks `MsgPackNamedCodec`. Same binary, same port, different wires, because **a codec belongs to a session and a session belongs to a controller**, so nothing about plaza ties a deployment to one encoding.

The browser page is therefore two clients in one file: JSON text frames on the lobby socket, and a hand-written MessagePack reader on the table socket. That reader is not a nicety, it is the demonstration. `MsgPackNamedCodec` exists precisely for a peer that **cannot be built from the server's struct definitions** and so has no way to recover field order, and a hundred lines of JavaScript is exactly that peer. Under the default compact codec every read in it would be an array index instead of a field name.

### 3. What the field names actually cost

The figure usually quoted for named MessagePack is 67% of JSON against compact's 40%, from a ten-op message. Measured on a whole match of this game's real traffic:

| | messages | json | compact | named |
|---|---|---|---|---|
| notices | 38 | 2923 | 870 | 2327 |
| snapshots | 18 | 4941 | 1170 | 3618 |
| **total** | **56** | **7864** | **2040** | **5945** |

Named is **76% of JSON where compact is 26%**, a premium of **+190%** rather than +67%. At that point the choice is close to "a quarter of JSON or three quarters of it", and adopting named to keep a hand-written client simple is nearly giving up MessagePack.

**The reason generalises, and it is the opposite of the other measurement in this repository.** A field name is paid *per field per message*, so the premium tracks how **wide** a message is, not how large. `PlayerView` has fifteen fields and is sent once per recipient on every deal and every resolved trick; a notice has two or three behind a variant name both encodings pay for. So the widest and most frequent message pays proportionally most. [`curtain_fire`](../curtain_fire/) measured a *per-message* cost, the variant tag, and found the opposite: there it is the **small** messages that are expensive. Both are true. A per-message cost punishes small messages; a per-field cost punishes wide ones.

### 4. Hidden information, through a lobby, to a client that cannot see the types

`SnapshotProvider` builds a payload per recipient: your cards by rank, everyone else's by count. The page draws opponents as backs because their ranks were never in your frame, not because it chose to hide them.

Bots read `player_view`, the same payload a browser gets, for the reason `card_table` gives: a bot reading `TableState` would hold every hand at the table, and an example whose whole claim is that a client cannot would be demonstrating it with a client that does.

## What it found

**A client must hold its lobby socket open until it is seated.** Closing the lobby connection on `Placed`, which is an obvious thing for a client to do once it has an endpoint, makes the lobby emit `AgentLeft`, which withdraws the reservation it just handed out, so the player arrives at the table as a spectator. Found by writing a probe client that did exactly that.

This is [`lobby_world`](../lobby_world/)'s lesson from the other side. There, a disconnect must **not** clear a seat, because hopping rooms closes the old socket after the new seat is reserved. Here, leaving the lobby **must** clear it, or a queue-and-quit leaves a seat nobody is coming to fill. Both are right, and the reconciling rule is the one plaza states in `ReconnectTracker`: *the transport never has the information*. Only the lobby knows whether a closed socket means "gone" or "moved on", and a client that wants its seat has to keep saying so.

For a two-socket client this is a real constraint, not a detail: the lobby socket and the table socket have separate lifetimes and the first one gates the second.

**`RoomFactory::GameStateType: Default` needs a lie, for the second time.** `TableState::default()` produces a table with no name, no stake and a `WalletRegistry` shared with nobody, and nothing ever calls it. It cannot even be derived, because none of `Phased`, `RoundRobinTurnManager` or `SequentialRoundManager` is `Default`, all three for the good reason that they are constructed with the op variants they wrap. So the workaround is a hand-written impl whose only caller is a trait bound. `lobby_world` hit this first and worked around it identically; two independent sightings is what the bound being wrong looks like.

## Reading order

| File | What is in it |
|---|---|
| [`src/types.rs`](src/types.rs) | Both op enums, `TableState`, and the wire version derived from this file |
| [`src/lobby.rs`](src/lobby.rs) | The queue, the link measurement, and `seat_formed`, which is where this differs from `lobby_world` |
| [`src/factory.rs`](src/factory.rs) | Spawning a table: its session, its controller, its endpoint |
| [`src/table.rs`](src/table.rs) | The rules, the seating, and the tests |
| [`src/snapshot.rs`](src/snapshot.rs) | The only place a hand becomes something a client receives |
| [`src/wire_cost.rs`](src/wire_cost.rs) | The measurement above, and the tests that pin it |
| [`static/index.html`](static/index.html) | Two sockets, two codecs, and a MessagePack reader in JavaScript |
