# spacemo

Ships and rocks in open space, used to work out **who can see whom in a volume**, which a flat world never has to answer.

`plaza_server_utils`'s `SpatialGrid` is two-dimensional. `insert(id, x, y)`, `query_radius(x, y, radius, out)`. Every 3D example built here so far could ignore that: a yard has a floor, an arena has a plane and a voxel world or a character on a landscape is locally 2.5D. Shipping MMOs use a flat grid plus a height check. Open space is where that stops working and it is also the cheapest place to test it, since space needs no terrain, no gravity, no character controller and no solver.

It sits at the far end of the axis the [examples index](../README.md) is organised on. [puck_rink](../puck_rink/) rolls back; [cube_yard](../cube_yard/) predicts nothing at all. This one has to predict, because nothing in the design absorbs latency for it.

```sh
./run-native.sh                      # host and play
./run-native.sh --role client --connect ws://<host>:8200/ws
```

Mouse aims, W and S (or up and down) are the throttle, space or left click fires, right click or left shift launches a missile. Click to capture the pointer, escape to release it. In the browser the click is required, since pointer lock needs a user gesture.

Everything that changes what crosses the wire is a **host dial on the panel** rather than a flag: the relevance strategy, bit packing, held locks, relative positions, whether a straight shot's path is sent, the view radius and the bot population. They change *who you are told about* and *what that costs* and the difference only shows while the volume is moving. The bots are there for that reason: with one ship in flight every strategy returns the same answer, so moving the dial changed nothing visible.

## Flat grid versus volume

Reproduce with `cargo test -p spacemo --test interest -- --nocapture`.

Dropping an axis does not *hide* anyone. A grid on `(x, z)` returns everything inside the **disc**, which is a superset of the sphere, so nothing is ever missed. The cost is false positives. Interest management that errs this way does not break the game; it sends extra ships and spends the bandwidth it was built to save.

```
2000 ships in a 800-unit cube, 80-unit view, per client at 60Hz

strategy            in view       packed    examined   cells
flat (x,z)             59.3     51.7 KiB/s     229.2    21.6
flat + y band           8.4      7.3 KiB/s     229.2    21.6
volume                  8.4      7.3 KiB/s      77.0    53.8
```

**A flat grid costs 7.1x the bandwidth of the same query with a one-line height filter on it.** The filtered query touches the same cells and examines the same candidates; all that changes is a cheaper test per candidate.

That leaves query cost as the only place a third grid axis could win. It trades **3x fewer distance tests for 2.5x more cell lookups**, which counts alone cannot settle. [examples/grid_timing.rs](examples/grid_timing.rs) times both on one scene (`cargo run --release --example grid_timing -p spacemo`, in high power mode). The three strategies now live in `plaza_server_utils::field` as `Strategy::Flat`, `FlatBand` and `Volume` and `relevance.rs` here re-exports them. gow_3d measured the opposite case there: once fliers stack over one spot, the height filter examines 2.7x what the volumetric grid does. For a spread-out volume like this one the one-line filter is the recommendation.

The control run varies the slab thickness:

```
 thickness         flat  with filter    over-send
         4         59.3         59.3        1.00x
        40         59.3         50.2        1.18x
       200         59.3         15.4        3.85x
       400         59.3          8.4        7.06x
```

At slab thickness a flat grid over-sends nothing. The over-send grows smoothly as the world gets thicker, so it comes from the geometry rather than from one particular scene.

## Bolts and missiles

A **bolt** flies straight, so its path follows from where it started and how fast. A client told once can carry it forward itself. That is not prediction in the reconciliation sense, since there is nothing to get wrong and nothing to correct against. A **missile** turns after its target, so its path depends on where that target goes next, which nobody knows at launch. It has to be sent every frame.

The two differ by one field, which decides the cost:

```
shots carried per frame, per client:

  every path sent          20.4
  spawns and homing only    1.2
```

That is a **17.3x** difference and all of it is shots whose paths were already implied. The tests check the distinction directly: they fly both forward forty ticks with the target manoeuvring, then compare each against a straight-line extrapolation from its own spawn. The bolt is still on that line to within 0.01 units and the missile has left it.

The setting is a dial, so you can watch the bolt count collapse while the missiles keep streaming.

Lock is resolved on the server, so pressing launch does nothing when the cone is empty or the reload is running. A client cannot see either condition unless it is told, so the frame carries what a missile would chase and how long until the next one. The client draws them as a box around the target and a bar under the health pips. `lock_for` is the same function `launch` uses to pick a target. A client testing its own cone would eventually draw a box around a ship the missile does not follow.

A missile is removed from a client's screen when it stops being sent. A bolt is sent once and carried forward until its life runs out, but a missile is streamed every frame because its path cannot be derived, so nothing on the client counts it down. No message announces its end either: it hits, expires or loses its target and stops appearing in the frame. Until the client treated that silence as an ending, every missile that came into view stayed for ever where it was last seen and a busy volume filled up with frozen ones. The panel counts missiles going quiet, since from outside a working despawn and one that never fires look the same.

There are three more decisions about the missile. **Lock is resolved on the server**, nearest target inside a 35 degree cone. The target is the one thing here a client could name that it should not be allowed to name and the check costs a dot product. **The counter-play is distance rather than evasion**: a missile leaves at its launcher's velocity plus 70 units a second and holds that speed while a ship tops out at 90, so a ship at full throttle out-runs one fired from a standstill and turning while chased gets you hit. **A missile whose target leaves** expires on a short fuse rather than flying on. A shot chasing nothing looks like a threat without being one and since a missile's path is sent every frame, letting it fly on spends bandwidth on a shot with no target.

## Events and state

Health is **state**: a client that missed the frame a hit landed on still learns the result from the next one, because every frame describes health completely. It costs two bits a ship.

A hit and a kill are **events**. They appear once and no later frame mentions them again, so they are the only things here whose delivery actually matters. On this transport reliable delivery is free. On a datagram transport it would not be. Anything still on screen a second after a kill is there because the client remembered it; the wire does not repeat it.

Three places where the game and the netcode wanted different things:

- **A kill reaches the people it names, even outside the view radius.** Relevance says not to send what cannot be seen, but being killed by something you are never told about is worse than the bandwidth that would save.
- **Streaks are counted on the server.** A client inferring one from arrival order would disagree with the next client about the same fight.
- **The announcement text is built on the client.** One event reads differently to each of the three people it concerns and sending three strings would spend bandwidth on wording.

## Churn

Every other example in the tree measures steady state: N bodies updating every tick. Bolts live about a second, so their cost lands on **entry and exit**.

```
eight ships in one fight, ten seconds, per frame per client:

  ships     8.0 at   133.0 bytes
  bolts    83.0 at  1091.2 bytes
```

The bolts cost eight times what the ships do, while turning over dozens of times in the run. Each shot is *cheaper* than a ship, but there are an order of magnitude more of them. A packet budget that covers the standing entities and ignores the short-lived ones will be missed. `priority`, `rest` and `delta` do not help here, since all three optimise how fresh a standing world stays.

That figure has every path streamed. The bolts and missiles section above shows what happens when the derivable ones stop being sent.

Two decisions keep bolts cheap:

- **A bolt has no orientation on the wire.** It points where it is going, so the client derives the streak from the velocity it already has.
- **Its id carries the slot generation as well as the index.** Slots are dense and reused, so a client keying on the index alone would blend a new bolt into the flight path of the one that just vacated the slot. `SlotKey` handles this and this is the first example that needs it.

## Positions relative to the observer

Absolute quantisation spends a fixed number of bits over the whole world, so its precision depends on *how big the world is*. cube_yard measured that directly: widening its floor four times took the error from 0.0008 units to 0.0033 at the same 16 bits. Open space has no natural size to quantise over.

Relevance already guarantees that every position in a frame is inside the view radius, so encoding the **offset** from the observer makes the range independent of the world size.

```
worst position error, ships within an 80-unit view:

  world half       absolute       relative
         400        0.0125u        0.0254u
        4000        0.1224u        0.0254u
       40000        1.2210u        0.0254u
      400000       12.2075u        0.0254u
```

The relative error stays flat, at 126 bits a ship against 128. At the smallest world absolute is still better, but relative wins from about 4000 units up.

The first version of this did not work. Quantising the anchor over the world put the world's size straight back into the error and relative came out very slightly *worse* than absolute at every size. The test passed anyway, because it compared growth **ratios**: relative started higher and grew more slowly, so the ratio comparison passed while the scheme was strictly worse. The assertion now requires the error to be *identical* at 400 and at 400000 and the anchor is sent at full width, where 96 bits once a frame amortises to almost nothing.

There is a round-trip test 200000 units from the origin, which the absolute scheme cannot represent at all: it clamps by over a thousand units.

## Locks outside the view radius

`LOCK_RANGE` is 320 units. The default view radius is 260. So a reticle could name a ship the relevance query had already culled and the client was told "locked: 7" with no ship 7 to draw it on. Relevance had silently broken lock, a mechanic that names an entity. `horde_playground` hit the same kind of fault when an enemy chased a player the client could not place.

Two things were wrong. The visible one was the less serious.

The lock was recomputed from the cone on every frame, so it changed as fast as the ships moved and turning your head lost it. Nothing can subscribe to a set like that and no player can aim with it. A lock is now **taken once from the cone and held** until the target dies or leaves lock range.

A held lock can then be a subscription, using `plaza_server_utils::subscription`. `Audience::of` unions it with the radius answer, so the locked ship is in the frame wherever it is.

`gow_3d` uses the same shape for a party. spacemo covers the other extreme: **one entry held for seconds rather than a handful held for hours.** The extra cost is counted in the server state's `lock_added` and is at most one ship per client however large the volume gets.

The hold then broke an assumption in the wire format. `REL` bounds an offset by the view radius because a frame used to carry only what the radius reached. The subscription adds ships from outside it: a locked ship can be anywhere in the volume, so its offset clamped at the bound and with the relative dial on it was drawn over a hundred units from where it flew. The mirror test that checks the client lands where the server is caught it. Nothing else could, since encode and decode agreed with each other perfectly. One bit per ship now says which arm carried it, an offset inside the radius or the absolute bounds past it, costing one bit per in-radius ship and three extra for the held one.

The panel switch turns the hold off and brings back the old behaviour along with the old defect: the cone is re-read every tick and the locked ship is only in your frame when the radius happened to reach it anyway.

## Prediction and the shared flight model

`advance()` is the whole flight model. It is a free function called by **both** the server's `Space::step` and the client's predictor, so the rule is not written twice or hidden behind a trait.

Reconciliation is there to absorb *network* disagreement. A second copy of the rule would make every frame a correction and smoothing cannot fix that. `a_prediction_and_the_server_walk_the_same_line` steps both side by side for 600 ticks under a held input and asserts **exact equality**, so it fails if anyone writes a second flight model anywhere.

Two decisions inside it:

- **Orientation is predicted and never corrected.** It is driven entirely by local input, so there is nothing to guess. Snapping it would fight the player's hand and gain no accuracy.
- **Corrections are blended out rather than snapped.** Because the rule is shared they are small and constant rather than rare and large, so snapping each one would be far more visible than carrying a little error for a few frames.

The panel shows **worst correction**, which shows whether the client and server are really running the same rule.

## Ships leaving the radius

Unlike cube_yard's client, a spacemo client has to notice when a ship leaves the radius. There is no despawn message; the server stops mentioning the ship, so the client treats silence as the signal.

That makes the grace period matter. Dropping a ship on the first silent frame makes the edge of the world **strobe**, because a ship near the radius flickers in and out of it as both ends drift. Waiting for a run of silent frames turns that into a fade.

## The pitch sign bug

The simulation works in yaw and pitch. The wire carries a quaternion because smallest-three is 29 bits against 64 and a client blending orientations needs something to slerp. Nothing forces the two to agree.

They disagreed. A positive rotation about X takes +Z toward **-Y**, while the flight model calls positive pitch *nose up*, so every ship would have rendered pitched the wrong way. Every position was correct throughout, so no positional, packing or relevance test would have noticed. The test that catches it rotates the forward vector by the wire quaternion and compares it against the simulation's own `facing()`. It implements that rotation the long way so it shares no code with what it checks.

## Reading the numbers yourself

```sh
cargo test -p spacemo --test interest -- --nocapture   # the bandwidth of a dropped axis
cargo test -p spacemo --lib -- --nocapture             # churn, relative encoding, the rest
```
