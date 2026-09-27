# parlour_client

A Flame client for [`examples/parlour_game`](../../examples/parlour_game/): **two sockets with separate lifetimes** and a card table that animates between state changes.

The other Flame example, [`plaza_flame/example`](../plaza_flame/example/), stops at the lobby: it drives `lobby_world`, holds one JSON socket and is about the version-skew policy. This one takes the endpoint the lobby hands out, opens a **second** connection to it on a **different codec** and plays a turn-based game across both.

```sh
cargo run -p plaza_example_parlour_game    # in the plaza repo, port 8092
flutter run -d macos                       # here
flutter test                               # against LoopbackSocket, no server
flutter test --tags e2e                    # against the live server
```

`--dart-define=host=1.2.3.4:8092` points it somewhere else. `../e2e.sh` runs the live suite with the server for you.

## What this client shows

### 1. The lobby socket stays open

Do not close the lobby connection once `Placed` arrives. The server reads a closed lobby socket as the player giving up, withdraws the reservation it issued a moment earlier and the table then seats them as a **spectator**: a player who can see the game and cannot play it.

So the two connections have separate lifetimes and the first one gates the second. `ParlourGame` holds the lobby for as long as it is seated and `the lobby socket stays open after placement` is the test that says so.

This is the client side of [`lobby_world`](../../examples/lobby_world/)'s "a disconnect is not an intention". In `lobby_world` the server must not read a dropped socket as the player leaving. Here the client keeps open the socket the server needs to keep reading. The transport cannot tell what the player intended, so one side has to state it and here the client does that by staying connected.

### 2. Ops are paced rather than applied

A snapshot arrives on a deal and on a resolved trick and **nothing in between**; the rest of the round is narrated as ops. Most server-authoritative games split the work this way ("full state only on major changes").

`PlazaClient.ops` delivers as fast as frames arrive, which suits a real-time game where only the newest frame matters. In a card game every op is an event the player needs to see in order: a player who does not see the deal before the first card lands has missed the game. [`OpSequencer`](../plaza_client_utils/API_REFERENCE.md#class-opsequencer) from `plaza_client_utils` sits between the stream and the scene: ops queue, `pump` releases them one at a time and an op worth watching asks for a hold.

It knows nothing about ops. The caller's function does the work and returns a `Hold`, seconds here since every wait at this table has a known length, so the pacing lives next to the animation rather than in a table of durations somewhere else.

### 3. Generated types and compact MessagePack

[`lib/wire_types.dart`](lib/wire_types.dart) is written by the server's build script (`Wire::dart_types` in `examples/parlour_game/build.rs`), not by hand. Under the compact codec a struct is an array and field order is the only thing that identifies a field; generated types read that order from the Rust definitions, so the table can run on `MsgPackCodec` without sending field names. Every generated type encodes `toWire(named: ...)`, named maps for the JSON lobby and compact arrays for the table and `fromWire` accepts either shape. `test/wire_conformance_test.dart` re-encodes the server's golden fixtures byte for byte, which checks that the order matches; regenerate the fixtures with `PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_example_parlour_game --test wire_fixtures` after a wire change.

## What is not extracted yet

**The mixin owns one client, so the second one is hand-rolled.** [`PlazaGame`](../plaza_flame/lib/src/game.dart) creates its client in `onLoad` and holds it for the game's life, which is right for the connection an app always has. A second connection whose URL is not known until the first one names it does not fit, so `ParlourGame` carries its own `PlazaClient`, its own two `StreamSubscription`s, its own teardown and its own `resume` on the lifecycle hook. That comes to about forty lines, all duplicating what the mixin already does.

**Why it is not extracted.** The obvious shape is a `PlazaLink` that owns one client plus its subscriptions and its stats, with `PlazaGame` holding a primary link and any number of named others. That would change a shipped package's API on the evidence of one consumer, which cannot show whether "a lobby and a room" generalises to "N links" or whether two links are all anyone needs. So it is filed rather than built.

## Reading order

| File | What is in it |
|---|---|
| [`lib/parlour_game.dart`](lib/parlour_game.dart) | Both connections, the view and what each op is worth watching |
| [`lib/main.dart`](lib/main.dart) | The widget layer, which only reads what the game decided |
| [`test/parlour_game_test.dart`](test/parlour_game_test.dart) | Two loopback sockets and the claims above |
| [`test/live_test.dart`](test/live_test.dart) | The same against a real server, which is the only place the MessagePack is real |
