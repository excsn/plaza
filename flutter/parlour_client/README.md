# parlour_client

A Flame client for [`examples/parlour_game`](../../examples/parlour_game/): **two sockets with separate lifetimes**, and a card table that animates between state changes.

The other Flame example, [`plaza_flame/example`](../plaza_flame/example/), stops at the lobby: it drives `lobby_world`, holds one JSON socket, and its point is the version-skew policy. This one is the half nothing had done. It takes the endpoint the lobby hands out, opens a **second** connection to it on a **different codec**, and plays a turn-based game across both.

```sh
cargo run -p plaza_example_parlour_game    # in the plaza repo, port 8092
flutter run -d macos                       # here
flutter test                               # against LoopbackSocket, no server
flutter test --tags e2e                    # against the live server
```

`--dart-define=host=1.2.3.4:8092` points it somewhere else. `../e2e.sh` runs the live suite with the server for you.

## The two things this client is for

### 1. The lobby socket stays open

Closing the lobby connection once `Placed` arrives is the obvious thing to do, and it is exactly wrong. The server reads a closed lobby socket as the player giving up, withdraws the reservation it issued a moment earlier, and the table then seats them as a **spectator**: a player who can see the game and cannot play it.

So the two connections have separate lifetimes and the first one gates the second. `ParlourGame` holds the lobby for as long as it is seated, and `the lobby socket stays open after placement` is the test that says so.

This is [`lobby_world`](../../examples/lobby_world/)'s "a disconnect is not an intention" from the client's side. There the server must not read a dropped socket as leaving; here the client must not drop the socket it needs the server to keep reading. Both follow from the same rule: **the transport never has the information**, so somebody has to say it out loud, and here that somebody is the client staying connected.

### 2. Ops are paced, not applied

A snapshot arrives on a deal and on a resolved trick and **nothing in between**; the rest of the round is narrated as ops. That is the server's side of a bargain most server-authoritative games make ("full state only on major changes"), and this is the client's side of it.

`PlazaClient.ops` delivers as fast as frames arrive, which is right for a real-time game where the newest frame is the truth and an old one is worthless. A card game is the opposite: every op is a thing that *happened*, in order, and a player who does not see the deal before the first card lands has missed the game. [`OpSequencer`](lib/sequencer.dart) sits between the stream and the scene: ops queue, `pump` releases them one at a time, and an op worth watching asks for a hold.

It knows nothing about ops. The caller's function does the work and returns how long to wait, so the pacing lives next to the animation rather than in a table of durations somewhere else.

### 3. The types are generated, and compact MessagePack is the payoff

[`lib/wire_types.dart`](lib/wire_types.dart) is written by the server's build script (`Wire::dart_types` in `examples/parlour_game/build.rs`), not by hand. Under the compact codec a struct is an array and field order is the whole contract; generated types read that order from the Rust definitions, which is what makes the table safe to run on `MsgPackCodec` and is why this client pays no field-name premium. Every generated type encodes `toWire(named: ...)`, named maps for the JSON lobby and compact arrays for the table, and `fromWire` accepts either shape. `test/wire_conformance_test.dart` re-encodes the server's golden fixtures byte for byte, which is the proof the order matches; regenerate the fixtures with `PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_example_parlour_game --test wire_fixtures` after a wire change.

## What this cost, which is the part worth reading

**The mixin owns one client, so the second one is hand-rolled.** [`PlazaGame`](../plaza_flame/lib/src/game.dart) creates its client in `onLoad` and holds it for the game's life, which is right for the connection an app always has. A second connection whose URL is not known until the first one names it does not fit, so `ParlourGame` carries its own `PlazaClient`, its own two `StreamSubscription`s, its own teardown, and its own `resume` on the lifecycle hook. About forty lines, all of it a second copy of what the mixin already does once.

**Not extracted, deliberately.** The obvious shape is a `PlazaLink` that owns one client plus its subscriptions and its stats, with `PlazaGame` holding a primary link and any number of named others. That is a real API change to a shipped package on the evidence of one consumer, and the thing one consumer cannot tell you is whether "a lobby and a room" generalises to "N links" or whether two is the whole story. Filed rather than built.

**`OpSequencer` is a candidate and not a graduate, for a narrower reason.** It is a pure primitive: self-contained, no dependencies, in-domain, opt-in, and this repository has already corrected itself once about deferring those. What is genuinely undecided is its *shape*: `apply` returning a hold duration is one design, an explicit `hold()` the applier calls is another, and a "tell me when the animation finishes" callback is a third. One consumer picks a shape; it does not tell you it is the right one. A second turn-based client would.

## Reading order

| File | What is in it |
|---|---|
| [`lib/sequencer.dart`](lib/sequencer.dart) | The queue, and nothing about plaza or Flame |
| [`lib/parlour_game.dart`](lib/parlour_game.dart) | Both connections, the view, and what each op is worth watching |
| [`lib/main.dart`](lib/main.dart) | The widget layer, which only reads what the game decided |
| [`test/parlour_game_test.dart`](test/parlour_game_test.dart) | Two loopback sockets, and the claims above |
| [`test/live_test.dart`](test/live_test.dart) | The same against a real server, which is the only place the MessagePack is real |
