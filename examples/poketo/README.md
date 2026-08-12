# poketo

A town you walk around, and battles you drop into. **Two netcode regimes in one game**, and the point is that they are genuinely different rather than one being a cheap version of the other.

The overworld is real-time and discrete: a trainer is standing on a tile or walking to the next one, and there is no third state. Battles are turn-based and instanced, which inverts everything the rest of this tree assumes. Nothing is predicted, interpolated, quantised or budgeted; latency is irrelevant, because a turn takes as long as the slower player takes to choose. All the difficulty moves into delivery, ordering and reconnection, which is the half of multiplayer nothing else here exercises.

Nothing is borrowed from any existing creature game. The creatures are invented, three of them, because a battle needs a reason to choose rather than a collection to complete.

## Running it

**The host is the server.** A native run hosts by default and its own player is just another client on a real socket, so what it sees and what it is told cost exactly what they would for anyone else.

```sh
./run-native.sh                                                # --role host: play, and serve joiners
./run-native.sh --role client --connect ws://<host>:8300/ws     # join someone else's town
./run-native.sh --role headless                                 # the deployable server, no window
./wasm-build.sh                                                 # build the browser client only
./wasm-serve.sh                                                 # build it and host it; open the printed URL
```

Arrows or WASD walk a tile at a time. Stepping into **tall grass** is what starts a battle, where `1` to `4` pick a move and any key walks you back out once it is decided. Standing on a **spring** mends what you are carrying; losing sends you back to the start, whole. **Esc** brings up everything the corner readout has no room for, and **F1** brings up the knobs the town runs on. `F2` writes a screenshot next to you; `POKETO_SHOT=<path>` takes one without a person present, which is how everything here was checked, because a GL window is out of reach of `screencapture` without the recording permission.

```sh
cargo test -p poketo --test town -- --nocapture        # the numbers below
cargo test -p poketo --test reconnect -- --nocapture   # what a reconnection costs
```

## Discreteness buys exactness, not bytes

The plan for this example predicted that a tile position would be dramatically cheaper than a continuous one, and that the saving would let the view radius grow enormously. Both halves were measured and both were wrong in the same direction.

```
one trainer, on the wire:

  continuous, full width   106 bits
  continuous, quantised     51 bits
  a tile                    36 bits
```

**2.9x against a naive wire of two floats and an angle, and 1.4x against the quantised position every other example here actually sends.** Comparing tiles against raw `f32` flatters the tile; nothing in this tree sends raw floats. Against a fair opponent the saving is modest.

What discreteness buys instead is **exactness**. A tile is an index rather than a measurement, so it has no bounds to outgrow, no quantiser, no precision to argue about, and two machines comparing positions can use `==`. cube_yard shipped a bug that cannot exist here, by widening its world past the range its quantiser covered and freezing everything that wandered out. Not needing the apparatus is the result; the byte saving is a side effect.

The same correction applies to the view radius:

```
300 trainers in a town 80 tiles across, one client's share at 60Hz:

  radius    in view       as tiles    as a position
       8       12.1         3.2 KiB/s         4.5 KiB/s
      24       61.5        16.2 KiB/s        23.0 KiB/s
      80      296.5        78.2 KiB/s       110.7 KiB/s
```

Ten times the radius is **24.5x the people**, not the 100x its area suggests, because a town runs out of people before a radius runs out of tiles. But it is nowhere near free either: the per-client cost still climbs roughly with area until it saturates. Discreteness makes each person cheaper; it does not make the radius cheap.

## A step is a rule, not a sample

A trainer occupies one tile or the next, with a four-bit phase saying how far along. **Arriving is what moves the tile**, and a facing cannot change part way through a step.

That is what lets a client draw a step entirely from its start: a known beginning, a known end and a known duration, with nothing to predict and nothing to smooth. No snapshot buffer, no interpolation clock, no error smoother. The whole rung `client_utils` exists for is replaced by arithmetic, because the *rule* is shared rather than the positions.

It also has to begin and advance on the same tick. Beginning on one and advancing from the next makes a step one tick longer than its name and leaves its first frame at phase zero, which reads as a stutter before every move.

The same four bits that place the trainer also pick its walk frame, so **the animation costs nothing the wire was not already paying**. What that needs is a beat counted through the *tile* rather than through the phase: a phase restarts every tile and is zero for one tick on arrival, so a frame chosen from it alone drops a standing pose into the middle of every step, which is a hitch seven times a second. Counting half tiles instead never restarts, and because a tile is two beats, arriving always lands on an even one: a trainer that is not walking stands still, and one that is alternates its feet.

## A map that is a rule rather than a thing that is sent

Every other world in this tree has to describe itself to a joining client: a level, a heightfield, a set of obstacles, something. Here the ground is a **pure function of the tile index**, so both ends compute the same answer from the same twenty bits.

```
a 256 by 256 corner of the map:

          path    6.9%
         grass   42.8%
    tall grass   20.8%
         water   16.6%
          tree   12.9%
        spring    0.0%   (one per 48 tiles of country)
```

**Not one byte of that crosses the wire.** There is no map payload, no join baseline for it, no version of it to disagree about, and a client holding a map that differs from the server's is not representable. It is the same trade the step already makes, applied to the ground: share the rule, and the state stops needing to be sent.

Two details are worth naming because they are where cheap noise stops looking like a place. A path is a **contour** of the height field rather than a third noise field, and the tiles where one field crosses one value form connected winding ribbons, which is what a road looks like and what uncorrelated noise cannot produce at any threshold. And the variant of a tile has to come from a properly mixed hash: multiplying the coordinates by small constants and xoring them leaves the low bits periodic, and a field of grass comes out as a visible checkerboard.

It also makes a test into a measurement. Because the map is a function, a test that wants somewhere an encounter can happen can **look one up** rather than walk about hoping, which is what `terrain::grass_run` is for.

**Springs are where the pure-function trick starts costing something.** A spring mends what you are carrying, and there is one per forty-eight tiles of country, placed at a hashed offset inside its region. The complication is that a third of the map is lake, wood or road, so a single offset leaves whole regions with nothing in them and "there is always one within a walk" quietly stops being true. Six offsets are tried and the first that lands on ground someone can stand on wins, which means the *placement* rule has to consult the *terrain* rule, and the terrain rule must not consult the placement rule back. Splitting `base_terrain` out from `terrain_at` is what keeps that from recursing, and the cheap candidate test in front of it is what keeps a per-tile query from evaluating terrain six extra times while drawing a thousand tiles a frame.

That is the honest cost of a map with no storage: anything that has to be *placed* rather than merely *computed* needs a search, and the search has to terminate without looking at itself.

## Two rhythms in one tick

The overworld goes out **every tick**, because a trainer nobody describes stops moving on screen. A battle goes out **only when something happens**, because nothing in it decays.

That is not an optimisation of one design into another. It is what the two regimes are: **a state has to be repeated to stay true and a transcript does not.** A client in a battle receives nothing at all on a tick, and is completely up to date however long ago its last frame arrived.

The switch between them is which collection holds a seat, never a flag on a player. A trainer in a battle is not walked, not sent the overworld, and not visible to anyone still in it. A boolean would leave a body standing in the grass while its owner is elsewhere, and every rule would have to remember to check it.

The panel says it out loud while you play: walking reads about 33 KiB/s and a battle reads **0.0 KiB/s recent**, because nothing arrives on a tick at all.

There is now a third rhythm, and it is the same argument a third time. A creature's level and experience are sent **on a change** rather than on a tick, because experience does not decay either. Keeping it a separate op rather than a field of the overworld frame is what leaves the per-tick frame exactly the shape every number below was measured against.

## A town that walks itself

The map used to hold nobody but the people connected to it, which meant a solo run was one rectangle on an empty grid. It now seats its own wanderers, driven by the same hashed wander the benchmarks always used, and they are ordinary walkers: they cost what a player costs on the wire and they appear in exactly the same relevance query.

```
a town of 240 wanderers across 4 zones, one client at 24 tiles:

    on this map        61
    in view            33.4
    on the wire        8.8 KiB/s
```

**75% of the town is on another map and is never considered at any radius.** That is not a distance check that happens to exclude them: somebody on another map is *absent*, so no radius reaches them and no work is done to decide it. The zone rule has been true since the first commit and until now only a test had ever seen it.

Seats are split rather than shared: a player is admitted into the low 256 and the town's own people sit above them, so a town full of itself can never refuse a player a seat, and `SEAT_BITS` stays at ten so the figures below do not move. A wanderer is also never given an encounter, because a battle nobody can answer freezes that walker where it stands, hidden from every view, waiting for a choice that is not coming.

## A choice names a slot, and a level is a field

A battle needed a reason to think, and the way it got one is the same trade as everything else here.

**A choice names a slot, not a move.** `Choose { turn, choice }` keeps the exact shape that makes a resend harmless, and which move a slot means is a rule both ends run, so a creature's four moves never cross the wire and a choice cannot name a move its creature does not have.

**A level is a field and everything it implies is a function.** `Creature` carries `kind`, `level`, `xp` and `health`; power and speed are derived from `(kind, level)`. Health is there because it is accumulated history and level because it comes from a record the client does not hold. So a creature that can grow costs *one byte more* than the fixed one it replaced, rather than three, and there is no way for a stated power to disagree with the level beside it.

**A miss is a property of the turn, not a roll.** The hash takes the battle's seed, the turn, the acting side, and **both sides' choices**. That last part is load-bearing: a roll a client could compute from what it already holds would let it pick whichever move is going to hit, so the inaccurate move would carry no risk at all. Neither side can know the other's choice until both have committed, which is exactly when the question is asked. Nothing in it may read the server's clock, or the same choice replayed at a different wall time would resolve differently and a resend being harmless would quietly weaken from a rule to a coincidence, with every reconnection test still green.

The wild side's answer is hashed the same way rather than hardcoded, so the transcript is complete instead of partly the server's private mind, and both status effects and turn order are read before anything is applied: a `Slow` that lands this turn must not reorder the turn it landed on, or the ordering depends on which machine evaluated it first.

## A result and the return from it are two different messages

The first version of this ended a battle the moment it was decided: the final `Battle` and the `Returned` that sends you back went out in the same batch. A client applies a batch in order, so it set the finished battle and cleared it inside one loop, and **the result was never on screen for a single frame**. Played, that reads as pressing a key and being dumped back into the town with no idea what happened, which is exactly what it was.

The fix is not a delay. A decided battle is left in `battles`, which is where the seat already was, and the client sends `Dismiss` when the player has read it. That keeps every property the example is built on: a seat is still in exactly one collection, the finished battle is still a transcript that is exactly as valid a minute later, and a battle whose owner drops mid-result still parks and resumes. `Dismiss` is refused for a battle still being fought, or the key that reads a result would walk a losing player out of the fight.

The general form, and it is worth stating because it is not about this game: **an op that says "here is what happened" and an op that says "and you are no longer here to see it" cannot be sent together.** The second destroys the audience for the first. Whether the gap between them is a delay, an acknowledgement or an input is a design choice; that there must be a gap is not.

The same session turned up the other half of that complaint, which was a balance problem rather than a protocol one. A level-one creature with a type disadvantage went down in **two hits**, so the moveset, the type chart and the accuracy roll might as well not have existed. Base health is now several times what a move takes off, and an ordinary first encounter runs about seven turns.

## Losing sends you home, and a teleport is something the client works out

A creature walked back out on the single point it had left could only lose again, and the nearest spring is a region's walk through the grass that just beat it, so the one thing a beaten player could do is the one thing that cannot work. **Losing returns the seat to the start, whole.** Winning leaves the damage on, because that is what a spring is for.

The part worth keeping is how the client knows it was moved. **Nothing tells it.** A step moves exactly one tile and that rule is already shared, so a trainer's tile changing by more than one step is not a walk, and the client tests for it in the one place it already reads its own position. No op was added, no flag was set, and the arrival effect is driven off a rule that was on the wire since the first commit.

That is the same trade as the ground and the walk cycle, for a third thing: the state was already sufficient, and what was missing was only the willingness to *derive* from it rather than to be told.

## Adding a level to a creature is a wire change two files from the wire

`Creature` lives in `battle.rs` and `Trainer` in `grid.rs`, and neither is where the ops are declared. Under a build script that hashed a list of files, giving a creature a level would have changed what `BattleState` encodes without moving the version, so two builds that disagreed about the wire would have completed the handshake and then mis-decoded.

That is not what happens, because `build.rs` here resolves rather than lists: `Wire::detect()` starts at the types tagged `plaza-wire: root` and walks their fields, so a payload two files away is counted and nobody has to remember to add it. This example is a fair test of that, since it moves `Creature`, `Choice` and `Battle` all at once and the version follows on its own.

The corollary is worth knowing before putting a new file near the wire: what the ops reach is hashed, so a type close to the protocol moves the version whether or not it is sent. That is why the terrain function lives in its own `terrain.rs` rather than beside the tile it takes, since tuning the ground should not disconnect anybody.

## Reconnection, which is where a turn-based game keeps its difficulty

Two decisions do all the work, and neither is a mechanism.

**A choice names the turn it is for.** A resend after a dropped connection names a turn that has already resolved, so it is stale and ignored rather than applied twice. That single field is ordering, deduplication and late-arrival handling together: no sequence number for the server to remember, no dedup table, no window to age out. The bug it prevents is invisible from both ends, which is why it is worth a test that compares the *whole battle* before and after a resend rather than just the health.

**A dropped connection parks what you had, not just where you were.** Experience does not decay any more than a battle does, so a parked seat keeps its creature too, and a reconnecting client is told what it holds. The mirror of that is the bug it creates if you only do half of it: a seat index is handed out again, so a joiner must be given a fresh creature *unconditionally* on admission or it inherits whatever the last occupant of that seat had grown, which looks like a gift rather than like a defect.

**A dropped connection parks a battle rather than ending it.** Nothing in a turn-based battle decays, so it is exactly as valid a minute later; ending it discards the only state here worth resuming. A reconnecting client is a **new connection with a new id**, so a token issued on seating is the only thing that can link it to what it was doing. The token spends once, a failed resume is silence (an expired token and a first join are the same situation from where the client is standing), and a park window is what stops it being a leak.

That combination is the whole story: the token gets you back to the battle, and the turn number makes whatever you resend on arrival harmless.

## A trade is an agreement

Neither a broadcast nor a rollback. Both sides offer, both confirm, and only then does anything change hands.

**Changing an offer clears both confirmations**, and that one line is the difference between a trade window and an exploit: without it you can agree to what you can see, then swap what you are giving before the commit lands.

Two more, both about refusing to do half a thing. An unfinished trade yields **no outcome at all** rather than one side of one, because a caller applying half a swap creates one creature and destroys another. And a committed trade refuses everything, which is what makes a resend harmless here in the same way naming a turn does in a battle.

## The knobs are a request, not a setting

`F1` is a panel of sliders: view radius, encounter odds, how many ticks a step takes. It is the only egui in this example, and it is egui because a slider is a widget rather than a picture; the rest of the screen, `Esc` included, is hand-drawn like every other panel here.

**Nothing on it takes effect locally.** Every one of those numbers is owned by the server, so moving a slider sends `Tune`, the server clamps it and answers `Tuned`, and the panel redraws from the answer. A control that applied itself and then waited to be contradicted is the same defect as a client holding a map the server disagrees with, one widget along. The clamp is on the server for the same reason: a view radius past the map is a query over everything, and a step of zero ticks is a division by zero in the phase, and neither of those is a client's decision to make.

There is one set for the whole town rather than one per player, which is what makes this a playground: whoever moves a slider moves it for everyone, and the point is to watch the KiB/s on the same panel move with the square of the radius.

## The art, and why it is compiled in

The first sprites in this tree; every other example draws itself with rectangles and circles. Five sheets in [assets/](assets/), generated with SpriteCook for ten credits, listed with their prompts and cell orders in [assets/MANIFEST.md](assets/MANIFEST.md).

They are **embedded with `include_bytes!` rather than fetched at runtime**, for three reasons that are all about failure rather than about speed. A missing or renamed asset becomes a compile error on every target, instead of a 404 in one browser on a stack whose documented failure mode is silent stubbing; this tree already wrote `ws_client/check_js_imports.py` because it refused to tolerate exactly that kind of quiet nothing. `Host::cache_bust` stamps asset URLs written in `index.html`, and a texture the wasm fetches for itself never appears there, so it could serve stale art against a fresh binary forever; bytes inside the wasm inherit the wasm's own stamp. And it is one code path on both targets, with no loading state and no untextured first frames. The cost is 256 KiB in a 1.1 MiB wasm, which the dead `egui-macroquad` dependency removed in the same pass more than paid for.

Generated sheets need mechanical correction before they are usable, and it is worth saying which kinds. The cells came back at 250 pixels square, so they were resampled once, offline, to a size whose cells divide exactly, and the renderer addresses them with integer rectangles. The tileset was asked for with gridlines to make its layout legible and they had to be cropped back off, because they are art the moment the game draws them. The creatures were re-cut from their measured bounding boxes rather than by splitting the sheet in three, because one of them overflowed its share and drew a sliver of itself down the edge of its neighbour. And the walk frames were trimmed to a common size and baseline, because a generator draws each cell at its own scale and cycling those reads as a jiggle rather than as a walk.

One seam worth knowing about: tile positions are rounded from the camera origin once, not per tile. Rounding each tile on its own puts neighbours 31 or 33 pixels apart depending on where the camera happens to be, and the one-pixel gaps that opens are a grid of seams across the whole map.

## Where it sits

## What a reconnection actually costs

`cargo test -p poketo --test reconnect -- --nocapture`

The plan for this example named one failure it had to pin: **an operation applied twice because a reconnect re-sent it.** Both sides have to be running to see it, because each is individually right. The client is correct to resend, since it never heard an answer. The server is correct to accept a choice. What neither owns is whether this choice is the same one.

```
  a choice for turn 1, resent on a new connection after the old
  one dropped: health [13, 22] before and [13, 22] after, turn
  2 both times.
```

The turn number on the choice is the entire mechanism. A choice that named only itself would be indistinguishable from a fresh one, and the resend would play the move again. A resend is therefore **silence** rather than a correction, which is why nothing is sent back for one.

Two smaller things the same test pins. A resumed client is told where it is by the ordinary frame rather than by anything special, so a reconnection needs no catch-up protocol. And a token that aged out is seated fresh with no error, because a resume that fails and a first join are the same situation, and inventing an error would make every client handle a case with no different answer.

[spacemo](../spacemo/) is the far end of the same axis: nothing in its design absorbs latency, so the netcode has to. This is the near end twice over, once because movement is discrete and once because a battle is turn-based. [The netcode chapter](../../docs/guide/02-choosing-your-netcode.md) is the argument; these are the two ends of it running.
