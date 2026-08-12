# fog_skirmish

Fog of war, where relevance is secrecy rather than bandwidth.

```sh
cargo run -p plaza_example_fog_skirmish
```

Then open http://127.0.0.1:8082. Click the map to send your three scouts; two bots hold the other corners. You are shown only what your scouts can see, and the panel counts what the server told you.

## Culling you cannot be generous about

`horde_playground` culls to save bytes, and being approximate costs it a few wasted frames. This culls because a client must never *possess* what its player cannot see, so being approximate in the generous direction is not a rounding error, it is the cheat. That difference is the whole example.

The mechanism is the same seam `card_table` uses, asked a different question: that one decides what you may hold by asking whose hand it is, this one by asking where your eyes are. A relic outside your vision is **absent from your payload**, not flagged in it, so there is no field a modified client could read to learn what it was not sent.

The query behind it is a query. 240 relics sit in a uniform grid keyed by cell; a pass gathers the cells your scouts' vision touches and then measures. A linear scan over 240 would work fine and teach nothing, which is exactly why the panel shows what the grid *offered* next to what survived: a typical view is **17 relics sent from 103 offered, out of 240 in the world**.

## Where the second channel must not go

`gow_3d`, `horde_playground` and `spacemo` all gained a subscription channel beside their spatial one: a party, a squad, a target lock. Each is a set you chose, told to you wherever its members are, which no radius will ever return.

This example deliberately has none, and the reason is the example itself. **A subscription is permission to be told about something you cannot see.** Here, not being able to see is the entire mechanic, so that permission is exactly what must not exist: any set a player could subscribe to is somebody else's scouts or somebody else's relic, and reaching either through the fog is the cheat the whole thing is built to prevent.

The one set that is legitimately yours at any distance is your own scouts, and it is already unconditional: `my_units` bypasses vision, `enemy_units` goes through `can_see`. That is the second channel, keyed on ownership, and it needs no subscription block to express because it can never grow past what you already own.

So the rule the four examples make together is not "use both channels". It is that a second channel is a **grant**, and a game whose subject is what a player may hold has to be able to say no to one.

## The panel counts ops, not frames

This is the amendment `pellet_maze` forced on this example before it was written. It shipped a per-recipient frame that filtered correctly and leaked anyway, because the events beside it named cells nobody had scouted. A readout proving "hidden positions never crossed the wire" is therefore the *minimum* here, not the demonstration, and it has to account for every op rather than every frame.

So [`positions_named`](src/vision.rs) maps any op to the places it reveals, with **no wildcard arm**: a new op variant does not compile until someone decides what it discloses. Two audits run against it, neither of which is allowed to drop anything:

- The server counts, on the way out, every position it told someone they could not see. Nothing repairs the leak, because a panel reading zero for two different reasons is worth nothing.
- [`tests/no_leaks.rs`](tests/no_leaks.rs) reads what actually arrived in a client's inbox, rather than what the server intended.

## Holding an event back and telling it late

A capture out of your sight is not broadcast and not summarised. It is **held whole**, and delivered when you next see the place it happened, marked `late`. Your client then agrees with everyone else's about a relic it never watched change hands, which is the claim: two boards stay consistent without a live position ever being revealed.

Telling you "something happened somewhere" instead would leak the timing. Never telling you would leave two clients permanently disagreeing about a relic they both end up standing on. The feed shows the difference: `P1 took relic 88 on tick 2140 — you are only being told now`.

## The number you can watch move

The panel's leak counter reads zero, which proves nothing on its own: a counter that cannot move is decoration. So the button turns the deferral off, which is the implementation this example is arguing against, and the game plays identically.

Measured over one run: **13 captures told late and 21 still held back**, both audits at **0 leaks**. Press the button, and leaks go **0 → 28** as the backlog empties at once.

## The bots play blind

`bots.rs` runs in the server process and could read `FogState`, which would send it straight to an uncaptured relic across the map. It reads `player_view` through `query_with` instead, the same payload a browser gets, so a bot is genuinely exploring: it heads for the nearest relic it can see and sweeps when it can see none. A bot playing on information the fog is meant to deny would make the example demonstrate the opposite of what it claims.
