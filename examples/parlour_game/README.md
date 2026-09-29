# parlour_game

A card game behind a lobby. You press play, wait and get seated at a table the server creates for your match.

This is how a matchmade turn-based game is usually built and no other example covers the combination. [`card_table`](../card_table/) has the turns, rounds, phases and hidden hands but one socket and no lobby. [`lobby_world`](../lobby_world/) has quick match, tickets and rooms-on-demand but a real-time arena and JSON throughout. No example combined the two and nothing in the workspace had run the placement path over a binary wire or spawned a room per match rather than placing into a standing one.

## Running it

```sh
./run.sh                                        # http://127.0.0.1:8092, from anywhere
cargo test -p plaza_example_parlour_game        # the tests behind everything below
cargo run -p plaza_example_parlour_game --example parlour_report   # what the field names cost
```

Open the page in **three tabs** and press quick match in each. Alternatively, press it in one and wait twelve seconds for the queue to time out and fill the other two seats with bots.

## The four things this example is for

### 1. A room created for each match

`lobby_world` places you into one of three standing arenas: `rooms_playable_at(...).find(a free seat)`. A card game does not work that way. Three people are matched and a table is *created* for them, so the room and the match have the same lifetime and every table is eventually reaped.

That is a two-line difference in `seat_formed` and a completely different lifecycle around it: `handle_create_room_request` runs inside the match-forming path, `max_players` is the size of the match rather than a property of the room and nothing is pre-spawned at boot.

The room lasts as long as the same group keeps playing. A settled match deals another after `INTERMISSION_TICKS` (five seconds at the table's 20 Hz) rather than sending three people who want to keep playing back through the queue. Bots keep their seats and play on into it. The stake settles once per match, which is what `settled` guards and what the rematch clears. When the humans leave, the table's socket goes quiet and after `TABLE_IDLE_AFTER` (45 seconds) the reaper drains it and collects it. The room was created for this group and closes when they leave, not between hands.

Every player off the top score pays the stake into a pot and the leaders split it, so a three-way tie moves nothing. A player who leaves a match in progress forfeits the stake into that pot and drops out of the standings. Each occupant's `Settled` carries their own balance and names a winner only when one player leads alone. Each deal is shuffled from a seed built from the table's name, the tick and the deal count, which the table logs at debug level so a deal can be reproduced.

### 2. Two codecs on one port

The lobby session speaks `JsonCodec`. Every table session speaks the compact `MsgPackCodec`. Both run in one binary on one port. The codec is set per session and each session belongs to one controller, so plaza does not tie a deployment to one encoding.

The two table clients show the two ways to cope with compact's encoding, where a struct is an array and the field order has to match exactly. The Flutter client's types are generated from `types.rs` by `Wire::dart_types` in `build.rs`, so its order is machine-checked and needs no upkeep. The browser page is the hand-written peer: its `SHAPES` tables are the field order copied by hand, which is the maintenance burden codegen removes. It is kept deliberately so that cost stays visible. Both are guarded by the derived protocol version, which moves whenever the order changes. `MsgPackNamedCodec` still exists for a peer that cannot be built or generated from the server's definitions. The measurement below shows why it should be a last resort.

### 3. What the field names actually cost

The figure usually quoted for named MessagePack is 67% of JSON against compact's 40%, from a ten-op message. Measured on a whole match of this game's real traffic:

| | messages | json | compact | named |
|---|---|---|---|---|
| notices | 40 | 2993 | 894 | 2377 |
| snapshots | 18 | 4941 | 1170 | 3618 |
| **total** | **58** | **7934** | **2064** | **5995** |

Named is 76% of JSON where compact is 26%, a premium of +190% rather than +67%. Adopting named to keep a hand-written client simple gives up most of what MessagePack saves. This deployment used named until generated Dart types made compact safe. The tables now use compact.

**Why the premium is this large.** A field name is paid per field per message, so the premium grows with the number of fields in a message rather than its size. `PlayerView` has sixteen fields and is sent once per recipient on every deal and every resolved trick; a notice has two or three behind a variant name both encodings pay for. So the widest and most frequent message pays proportionally most. [`curtain_fire`](../curtain_fire/) measured a per-message cost (the variant tag) and found that there the small messages were the expensive ones.

### 4. Hidden information, through a lobby, to a client that cannot see the types

`SnapshotProvider` builds a payload per recipient: your cards by rank, everyone else's by count. The page draws opponents' cards face down because their ranks are not in your frame at all.

Bots read `player_view`, the same payload a browser gets, for the reason `card_table` gives: a bot reading `TableState` would hold every hand at the table and no client should be able to see that.

## What it found

**A client must hold its lobby socket open until it is seated.** Closing the lobby connection on `Placed`, which is an obvious thing for a client to do once it has an endpoint, makes the lobby emit `AgentLeft`, which withdraws the reservation it just handed out, so the player arrives at the table as a spectator. A probe client that did this is how it was found.

[`lobby_world`](../lobby_world/) hit the opposite case. There, a disconnect must not clear a seat, because hopping rooms closes the old socket after the new seat is reserved. Here, leaving the lobby must clear it. Otherwise a queue-and-quit leaves a seat nobody is coming to fill. `ReconnectTracker` documents the rule that covers both: the transport never has the information. Only the lobby knows whether a closed socket means "gone" or "moved on", so a client that wants its seat has to keep the lobby socket open.

So a two-socket client has to manage two lifetimes: the table socket depends on the lobby socket staying open until the player is seated.

**`RoomFactory::GameStateType: Default` needs a placeholder impl, for the second time.** `TableState::default()` produces a table with no name, no stake and a `WalletRegistry` shared with nobody and nothing ever calls it. It cannot even be derived, because none of `Phased`, `RoundRobinTurnManager` or `SequentialRoundManager` is `Default`, since each is constructed with the op variants it wraps. So the workaround is a hand-written impl whose only caller is a trait bound. `lobby_world` hit this first and worked around it identically; two independent examples hitting it suggests the bound itself is wrong.

## Reading order

| File | What is in it |
|---|---|
| [`src/types.rs`](src/types.rs) | Both op enums, `TableState` and the wire version derived from this file |
| [`src/lobby.rs`](src/lobby.rs) | The queue, the link measurement and `seat_formed`, which is where this differs from `lobby_world` |
| [`src/factory.rs`](src/factory.rs) | Spawning a table: its session, its controller, its endpoint |
| [`src/table.rs`](src/table.rs) | The rules, the seating and the tests |
| [`src/snapshot.rs`](src/snapshot.rs) | The only place a hand is turned into a payload a client receives |
| [`src/wire_cost.rs`](src/wire_cost.rs) | The measurement above and the tests that pin it |
| [`static/index.html`](static/index.html) | Two sockets, two codecs and a MessagePack reader in JavaScript |
