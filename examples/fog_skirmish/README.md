# fog_skirmish

Fog of war, where the server culls what a player cannot see to keep it secret rather than to save bandwidth.

```sh
cargo run -p plaza_example_fog_skirmish
```

Then open http://127.0.0.1:8082. Click the map to send your three scouts; two bots hold the other corners. You are shown only what your scouts can see and the panel counts what the server told you.

## Culling for secrecy

`horde_playground` culls to save bytes. If its culling is approximate, it costs a few wasted frames. This example culls because a client must never hold what its player cannot see, so sending even slightly too much is a cheat.

It uses the same per-recipient snapshot hook as `card_table`. `card_table` decides what you may hold by whose hand a card is in and this example decides by what your scouts can see. A relic outside your vision is left out of your payload entirely, so there is no field a modified client could read to learn what it was not sent.

The visibility check is a spatial query. 240 relics sit in a uniform grid keyed by cell; a pass gathers the cells your scouts' vision touches and then measures distances. A linear scan over 240 relics would also work, but the example uses a grid on purpose and the panel shows what the grid *offered* next to what survived: a typical view is **17 relics sent from 103 offered, out of 240 in the world**.

## No subscription channel

`gow_3d`, `horde_playground` and `spacemo` all gained a subscription channel beside their spatial one: a party, a squad, a target lock. Each is a set you chose and you are told about its members wherever they are, which a radius query cannot give you.

This example has none on purpose. A subscription lets a player be told about something they cannot see. In this game hidden information is the mechanic, so that must not be possible. Any set a player could subscribe to would be somebody else's scouts or somebody else's relics. Reaching either through the fog is the cheat this example prevents.

Your own scouts are the only set you may see at any distance. They are always sent: `my_units` bypasses vision and `enemy_units` goes through `can_see`. That is this example's second channel. It is keyed on ownership and needs no subscription block, because it can never include anything you do not own.

Across the four examples, a second channel is opt-in. A game about hidden information has to be able to leave it out.

## What the panel counts

The panel counts ops and not frames because of `pellet_maze`. That example shipped a per-recipient frame that filtered correctly and leaked anyway, because the events sent beside it named cells nobody had scouted. Checking that snapshot frames hide positions is not enough here. The panel accounts for every op.

[`positions_named`](src/vision.rs) maps any op to the places it reveals and has no wildcard arm, so a new op variant does not compile until someone decides what it discloses. Two audits run against it and neither is allowed to drop anything:

- The server counts, on the way out, every position it told someone they could not see. Nothing repairs the leak, because then a zero on the panel could mean either no leak or a repaired one.
- [`tests/no_leaks.rs`](tests/no_leaks.rs) reads what actually arrived in a client's inbox, rather than what the server intended.

## Holding an event back and telling it late

A capture out of your sight is held in full, not broadcast or summarised. It is delivered, marked `late`, when you next see the place it happened. Your client then agrees with everyone else's about a relic it never watched change hands, so two boards stay consistent without a live position ever being revealed.

Telling you "something happened somewhere" instead would leak the timing. Never telling you would leave two clients permanently disagreeing about a relic they both end up standing on. The feed shows the difference: `P1 took relic 88 on tick 2140 — you are only being told now`.

## The deferral toggle

The panel's leak counter reads zero, but a counter that never moves might not be counting anything. The button turns the deferral off, which is the implementation this example argues against. The game plays identically.

Measured over one run: **13 captures told late and 21 still held back**, both audits at **0 leaks**. Press the button and leaks go **0 → 28** as the backlog empties at once.

## Bots read only their own view

`bots.rs` runs in the server process and could read `FogState`, which would send it straight to an uncaptured relic across the map. It reads `player_view` through `query_with` instead, which is the same payload a browser gets. A bot heads for the nearest relic it can see and sweeps the map when it can see none.
