# poketo

A town you walk around and battles you drop into. It runs **two netcode regimes in one game** and neither is a cut-down version of the other.

The overworld is real-time and discrete: a trainer is either standing on a tile or walking to the next one. Battles are turn-based and instanced, the opposite of what the rest of this tree assumes. Nothing in a battle is predicted, interpolated, quantised or budgeted. Latency does not matter, because a turn takes as long as the slower player takes to choose. The hard parts are delivery, ordering and reconnection, which no other example here exercises.

Nothing is borrowed from any existing creature game. There are three invented creatures, enough to give a battle a choice to make; there is no collection to complete.

## Running it

A native run hosts by default and **the host is the server**. Its own player is another client on a real socket, so what it sees and what it is told cost exactly what they would for anyone else.

```sh
./run-native.sh                                                # --role host: play and serve joiners
./run-native.sh --role client --connect ws://<host>:8300/ws     # join someone else's town
./run-native.sh --role headless                                 # the deployable server, no window
./wasm-build.sh                                                 # build the browser client only
./wasm-serve.sh                                                 # build it and host it; open the printed URL
```

Arrows or WASD walk a tile at a time. Stepping into **tall grass** starts a battle. In a battle `1` to `4` pick a move and any key walks you back out once it is decided. Standing on a **spring** heals the creature you are carrying; losing sends you back to the start fully healed. **Esc** shows what the corner readout has no room for and **F1** opens the settings the town runs on. `F2` writes a screenshot next to you; `POKETO_SHOT=<path>` takes one with nobody at the keyboard. Everything here was checked that way, because `screencapture` cannot reach a GL window without the recording permission.

```sh
cargo test -p poketo --test town -- --nocapture        # the numbers below
cargo test -p poketo --test reconnect -- --nocapture   # what a reconnection costs
```

## Tile positions on the wire

The plan for this example expected a tile position to be much cheaper than a continuous one and the saving to let the view radius grow a lot. Both were measured and both savings were smaller than expected.

```
one trainer, on the wire:

  continuous, full width   106 bits
  continuous, quantised     51 bits
  a tile                    36 bits
```

A tile is **2.9x smaller than a naive wire of two floats and an angle and 1.4x smaller than the quantised position every other example here actually sends.** Nothing in this tree sends raw `f32`, so 1.4x is the fair comparison and the saving is modest.

The bigger gain is **exactness**. A tile is an index rather than a measurement. It has no bounds to outgrow, no quantiser and no precision to argue about, so two machines comparing positions can use `==`. cube_yard shipped a bug that cannot happen here: it widened its world past the range its quantiser covered and froze everything that wandered out. Not needing a quantiser matters more than the byte saving.

The view radius estimate needed the same correction:

```
300 trainers in a town 80 tiles across, one client's share at 60Hz:

  radius    in view       as tiles    as a position
       8       12.1         3.2 KiB/s         4.5 KiB/s
      24       61.5        16.2 KiB/s        23.0 KiB/s
      80      296.5        78.2 KiB/s       110.7 KiB/s
```

Ten times the radius covers **24.5x the people** rather than the 100x its area suggests, because a town runs out of people before a radius runs out of tiles. The per-client cost still climbs roughly with area until it saturates.

## Steps

A trainer occupies one tile or the next, with a four-bit phase saying how far along. The tile changes on arrival and the facing cannot change part way through a step.

So a client can draw a whole step from its start, since the beginning, the end and the duration are all known. It needs no snapshot buffer, interpolation clock or error smoother. Arithmetic replaces the whole rung `client_utils` exists for, because the client runs the same step rule as the server.

A step has to begin and advance on the same tick. Beginning on one tick and advancing from the next makes a step one tick longer than it should be and leaves its first frame at phase zero, which shows as a stutter before every move.

The same four bits that place the trainer also pick its walk frame, so the animation costs nothing extra on the wire. The frame has to come from a beat counted through the *tile* rather than from the phase alone. A phase restarts every tile and is zero for one tick on arrival, so a frame chosen from it drops a standing pose into the middle of every step, a hitch seven times a second. A count of half tiles never restarts and since a tile is two beats, arriving always lands on an even one: a trainer that is not walking stands still and one that is alternates its feet.

## The map

Every other world in this tree sends a joining client a description of itself: a level, a heightfield or a set of obstacles. Here the ground is a **pure function of the tile index**, so both ends compute the same answer from the same twenty bits.

```
a 256 by 256 corner of the map:

          path    6.9%
         grass   42.8%
    tall grass   20.8%
         water   16.6%
          tree   12.9%
        spring    0.0%   (one per 48 tiles of country)
```

None of that is sent. There is no map payload and no join baseline for it and a client cannot hold a map that differs from the server's. Steps work the same way: both ends share the rule, so the state does not need to be sent.

Two details keep the generated ground from looking like noise. A path is a **contour** of the height field rather than a third noise field. The tiles where one field crosses one value form connected winding ribbons that look like roads, which uncorrelated noise cannot produce at any threshold. The variant of a tile has to come from a properly mixed hash: multiplying the coordinates by small constants and xoring them leaves the low bits periodic and a field of grass comes out as a visible checkerboard.

Because the map is a function, a test that needs somewhere an encounter can happen can **look one up** with `terrain::grass_run` instead of walking around until it finds one.

**Springs** are harder. A spring heals the creature you are carrying and there is one per forty-eight tiles of country, placed at a hashed offset inside its region. A third of the map is lake, wood or road, so a single offset leaves whole regions without one and "there is always one within a walk" stops being true. Six offsets are tried and the first that lands on walkable ground is used. So the *placement* rule has to consult the *terrain* rule and the terrain rule must not consult the placement rule. Splitting `base_terrain` out from `terrain_at` keeps that from recursing and a cheap candidate test in front of it stops a per-tile query from evaluating terrain six extra times while drawing a thousand tiles a frame.

## Overworld and battle send rates

The overworld goes out **every tick**, because a trainer that is not described stops moving on screen. A battle goes out **only when something happens**, because nothing in it decays.

The overworld is state and has to be repeated to stay current. A battle is a transcript, so a client in a battle receives nothing on a quiet tick and is still completely up to date however long ago its last frame arrived.

Which regime a seat is in is decided by which collection holds it rather than by a flag on the player. A trainer in a battle is not walked, not sent the overworld and not visible to anyone still in it. With a boolean the body would stay standing in the grass while its owner was elsewhere unless every rule remembered to check it.

The panel shows this while you play: walking reads about 33 KiB/s and a battle reads **0.0 KiB/s recent**.

A creature's level and experience are sent **only when they change**, because experience does not decay either. They are a separate op rather than a field of the overworld frame, so the per-tick frame keeps exactly the shape every number below was measured against.

## Wanderers

The map used to hold only the people connected to it, so a solo run was one rectangle on an empty grid. It now seats its own wanderers, driven by the same hashed wander the benchmarks always used. They are ordinary walkers: they cost what a player costs on the wire and appear in the same relevance query.

```
a town of 240 wanderers across 4 zones, one client at 24 tiles:

    on this map        61
    in view            33.4
    on the wire        8.8 KiB/s
```

**75% of the town is on another map and is never considered at any radius.** No distance check is involved, since somebody on another map is not in the query at all. The zone rule has been there since the first commit, but before the wanderers only a test exercised it.

Players are seated in the low 256 seats and the town's own people sit above them, so a town full of wanderers can never refuse a player a seat. `SEAT_BITS` stays at ten so the figures below do not move. A wanderer is never given an encounter, because nobody would answer the battle and the wanderer would stay frozen where it stands, hidden from every view.

## Battle choices, levels and misses

A choice names a move slot rather than a move. `Choose { turn, choice }` keeps the exact shape that makes a resend harmless. Which move a slot means is a rule both ends run, so a creature's four moves never cross the wire and a choice cannot name a move its creature does not have.

`Creature` carries `kind`, `level`, `xp` and `health`. Power and speed are derived from `(kind, level)`. Health is a field because it is accumulated history and level is a field because it comes from a record the client does not hold. So a creature that can level up costs *one byte more* than the fixed one it replaced rather than three and a stated power can never disagree with the level beside it.

**A miss is computed from a hash rather than rolled.** The hash takes the battle's seed, the turn, the acting side and **both sides' choices**. The choices have to be in it: if a client could compute the roll from what it already holds, it could pick whichever move is going to hit and the inaccurate move would carry no risk at all. Neither side knows the other's choice until both have committed, which is when the hash is computed. Nothing in it may read the server's clock. If it did, the same choice replayed at a different wall time would resolve differently and resends would only be harmless by coincidence, with every reconnection test still passing.

The wild side's choice is hashed the same way rather than hardcoded, so the transcript is complete and does not depend on hidden server state. Status effects and turn order are both read before anything is applied: a `Slow` that lands this turn must not reorder the turn it landed on, or the ordering would depend on which machine evaluated it first.

## Showing the battle result

The first version of this ended a battle the moment it was decided: the final `Battle` and the `Returned` that sends you back went out in the same batch. A client applies a batch in order, so it set the finished battle and cleared it inside one loop and **the result was never on screen for a single frame**. In play you pressed a key and were dumped back in the town with no idea what happened.

The fix does not use a delay. A decided battle stays in `battles`, where the seat already was. The client sends `Dismiss` once the player has read it. Everything else still holds: a seat is in exactly one collection, the finished battle is a transcript that is just as valid a minute later and a battle whose owner drops mid-result still parks and resumes. `Dismiss` is refused for a battle still being fought, or the key that dismisses a result would walk a losing player out of the fight.

This applies outside this game too. An op that reports a result and an op that removes the screen it would be shown on cannot be sent in the same batch. There has to be a gap between them, which can be a delay, an acknowledgement or an input.

The same session turned up a balance problem too. A level-one creature with a type disadvantage went down in **two hits**, too fast for the moveset, the type chart or the accuracy roll to matter. Base health is now several times what a move takes off and an ordinary first encounter runs about seven turns.

## Losing and returning to the start

A creature walked back out with its last point of health could only lose again, since the nearest spring is a region's walk through the same grass. **Losing now returns the seat to the start, fully healed.** Winning leaves the damage on, since healing is what springs are for.

No message tells the client it was moved. A step moves exactly one tile and that rule is already shared, so the client treats a tile change of more than one step as a teleport. It checks for this in the one place it already reads its own position. No op or flag was added: the arrival effect is driven off a rule that has been on the wire since the first commit.

## The wire version

`Creature` lives in `battle.rs` and `Trainer` in `grid.rs` and neither is where the ops are declared. Under a build script that hashed a list of files, giving a creature a level would have changed what `BattleState` encodes without moving the version, so two builds that disagreed about the wire would have completed the handshake and then mis-decoded.

That does not happen here because `build.rs` resolves types instead of listing files. `Wire::detect()` starts at the types tagged `plaza-wire: root` and walks their fields, so a payload two files away is counted without anyone adding it by hand. This example changes `Creature`, `Choice` and `Battle` all at once and the version follows on its own.

Check this before putting a new type near the wire: what the ops reach is hashed, so a type close to the protocol moves the version whether or not it is sent. That is why the terrain function lives in its own `terrain.rs` rather than beside the tile it takes, since tuning the ground should not disconnect anybody.

## Reconnection

Two decisions do the work without adding any machinery.

**A choice names the turn it is for.** A resend after a dropped connection names a turn that has already resolved, so the server ignores it rather than applying it twice. That one field handles ordering, deduplication and late arrival, so the server needs no sequence number, dedup table or window to age out. The bug it prevents is invisible from both ends, so the test compares the *whole battle* before and after a resend rather than just the health.

**A dropped connection parks the creature as well as the position.** Experience does not decay any more than a battle does, so a parked seat keeps its creature too and a reconnecting client is told what it holds. Doing only half of this causes a bug. Seat indices are handed out again, so a joiner must be given a fresh creature *unconditionally* on admission. Otherwise it inherits whatever the seat's last occupant had grown, which looks like a gift rather than a defect.

**A dropped connection parks a battle rather than ending it.** Nothing in a turn-based battle decays, so it is just as valid a minute later and ending it would throw away the only state here worth resuming. A reconnecting client is a **new connection with a new id**, so a token issued on seating is the only thing that can link it to what it was doing. The token can be used once, a failed resume gets no reply (an expired token and a first join look the same to the client) and a park window stops parked seats from leaking.

## Trades

A trade uses neither a broadcast nor a rollback. Both sides offer and confirm before anything changes hands.

**Changing an offer clears both confirmations.** Without that one line there is an exploit: you can agree to what you can see, then swap what you are giving before the commit lands.

An unfinished trade yields **no outcome at all** rather than half of one, because a caller applying half a swap would create one creature and destroy another. A committed trade refuses everything, which makes a resend harmless here the same way naming a turn does in a battle.

## The F1 panel

`F1` is a panel of sliders: view radius, encounter odds, how many ticks a step takes. It is the only egui in this example, since a slider is a widget. The rest of the screen, `Esc` included, is hand-drawn like every other panel here.

**Nothing on it takes effect locally.** The server owns every one of those numbers, so moving a slider sends `Tune`, the server clamps it and answers `Tuned` and the panel redraws from the answer. A control that applied itself and then waited to be contradicted would be the same defect as a client holding a map the server disagrees with. The clamp is on the server for the same reason. A view radius past the map is a query over everything and a step of zero ticks is a division by zero in the phase; neither is a client's decision to make.

There is one set of values for the whole town rather than one per player. Whoever moves a slider moves it for everyone and you can watch the KiB/s on the same panel move with the square of the radius.

## The art

These are the first sprites in this tree; every other example draws itself with rectangles and circles. Five sheets in [assets/](assets/), generated with SpriteCook for ten credits, listed with their prompts and cell orders in [assets/MANIFEST.md](assets/MANIFEST.md).

They are **embedded with `include_bytes!` rather than fetched at runtime**, for three reasons, none of them speed. First, a missing or renamed asset becomes a compile error on every target instead of a 404 in one browser on a stack whose documented failure mode is silent stubbing. This tree already has `ws_client/check_js_imports.py` to catch pages that fail silently. Second, `Host::cache_bust` stamps asset URLs written in `index.html` and a texture the wasm fetches for itself never appears there, so stale art could be served against a fresh binary indefinitely. Bytes inside the wasm share the wasm's own stamp. Third, it is one code path on both targets, with no loading state and no untextured first frames. The cost is 256 KiB in a 1.1 MiB wasm, less than removing the dead `egui-macroquad` dependency in the same pass saved.

The generated sheets needed mechanical correction before they were usable. The cells came back at 250 pixels square, so they were resampled once, offline, to a size whose cells divide exactly and the renderer addresses them with integer rectangles. The tileset was asked for with gridlines to make its layout legible and they had to be cropped back off, because the game would draw them as part of the tile. The creatures were re-cut from their measured bounding boxes rather than by splitting the sheet in three, because one of them overflowed its share and drew a sliver of itself down the edge of its neighbour. The walk frames were trimmed to a common size and baseline, because a generator draws each cell at its own scale and cycling those looks like a jiggle rather than a walk.

Tile positions are rounded from the camera origin once rather than per tile. Rounding each tile on its own puts neighbours 31 or 33 pixels apart depending on where the camera is and the one-pixel gaps show up as a grid of seams across the whole map.

## Where it sits

## Reconnection cost

`cargo test -p poketo --test reconnect -- --nocapture`

The plan for this example named one failure it had to pin: **an operation applied twice because a reconnect re-sent it.** It only shows with both sides running, because each side is right on its own: the client resends since it never heard an answer and the server accepts a choice. Neither side on its own can tell whether this choice is the same one.

```
  a choice for turn 1, resent on a new connection after the old
  one dropped: health [13, 22] before and [13, 22] after, turn
  2 both times.
```

The turn number on the choice is what prevents it. Without it a resent choice would look like a fresh one and the move would play again. The server ignores a resend rather than correcting anything, so nothing is sent back for one.

The same test pins two smaller things. A resumed client learns where it is from the ordinary frame, so a reconnection needs no catch-up protocol. A token that aged out is seated fresh with no error, because a failed resume and a first join are the same situation and an error would make every client handle a case that needs no different response.

[spacemo](../spacemo/) is at the far end of the same axis: nothing in its design absorbs latency, so the netcode has to. poketo is at the near end for two reasons: movement is discrete and a battle is turn-based. [The netcode chapter](../../docs/guide/02-choosing-your-netcode.md) covers that axis and these two examples sit at its ends.
