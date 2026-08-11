# spacemo

Ships and rocks in open space, and the question every flat world lets you avoid: **who can see whom, in a volume?**

`plaza_server_utils`'s `SpatialGrid` is two-dimensional. `insert(id, x, y)`, `query_radius(x, y, radius)`. Every 3D example built here so far could ignore that, because a yard has a floor and an arena has a plane, and a voxel world or a character on a landscape is locally 2.5D. A flat grid plus a height check is what shipping MMOs actually use. Open space is where the pretence stops working, and it is also by a wide margin the cheapest place to find out: space needs no terrain, no gravity, no character controller and no solver.

It sits at the far end of the axis the [examples index](../README.md) is organised on. [puck_rink](../puck_rink/) rolls back; [cube_yard](../cube_yard/) predicts nothing at all. This one has to predict, because nothing in the design absorbs latency for it.

```sh
./run-native.sh                      # host and play
./run-native.sh --role client --connect ws://<host>:8200/ws
```

Everything that changes what crosses the wire is a **host dial on the panel**, not a flag. What they change is *who you are told about*, and that only reads as a difference while the volume keeps moving.

## The finding: a dropped axis is paid for in bandwidth

Reproduce with `cargo test -p spacemo --test interest -- --nocapture`.

The intuition to correct first: dropping an axis does not *hide* anyone. A grid on `(x, z)` returns everything inside the **disc**, which is a superset of the sphere, so nothing is ever missed. What it costs is false positives, and interest management that is wrong in this direction does not break the game. It quietly funds the bandwidth it was built to save.

```
2000 ships in a 800-unit cube, 80-unit view, per client at 60Hz

strategy            in view       packed    examined   cells
flat (x,z)             59.3     51.7 KiB/s     229.2    21.6
flat + y band           8.4      7.3 KiB/s     229.2    21.6
volume                  8.4      7.3 KiB/s      77.0    53.8
```

**A flat grid costs 7.1x the bandwidth of the same query with a height filter on it, and the filter is one line.** It touches the same cells and examines the same candidates; all that changes is a cheaper test per candidate.

Which leaves the third axis with nothing to win on but query cost, where it trades **3x fewer distance tests for 2.5x more cell lookups**. That is a trade, not a victory, and settling it needs a timed run rather than a count. So `encode_3d` in `relevance.rs` stays unused, and the recommendation this example makes is the one-line filter.

The control matters as much as the result:

```
 thickness         flat  with filter    over-send
         4         59.3         59.3        1.00x
        40         59.3         50.2        1.18x
       200         59.3         15.4        3.85x
       400         59.3          8.4        7.06x
```

**At slab thickness a flat grid costs exactly nothing.** It is right for what it was built for, and degrades smoothly as the world gains thickness. The finding is about geometry, not about one scene.

## Churn is the other half of the bill

Every other example in the tree measures steady state: N bodies updating every tick. Bolts are the opposite. They live about a second and then do not, so the cost lands on **entry and exit**.

```
eight ships in one fight, ten seconds, per frame per client:

  ships     7.8 at   116.2 bytes
  bolts    31.4 at   409.5 bytes
```

**The transient half of the world is 78% of the packet**, while turning over 64 times in the run. A bolt is individually *cheaper* than a ship (13.0 bytes against 14.9) and collectively 3.5x more expensive, because there are four times as many. Budgeting for the entities you can name and not for the ones that come and go is how a packet budget gets missed.

Two decisions carry that:

- **A bolt has no orientation on the wire.** It points where it is going, so the client derives the streak from the velocity it already has. Paying for orientation on the most numerous thing in the scene would be the obvious mistake.
- **Its id carries the slot generation, not just the index.** Slots are dense and reused, so an index alone is not an identity: a client keying on it would blend a new bolt into the flight path of the one that just vacated the slot. That is what `SlotKey` is for, and this is the first example that needs it.

## Positions relative to the observer

Absolute quantisation spends a fixed number of bits over the whole world, so its precision is a property of *how big the world is*. cube_yard measured that directly: widening its floor four times took the error from 0.0008 units to 0.0033 at the same 16 bits. Space has no size to fix.

Relevance already guarantees that every position in a frame is inside the view radius, so encode the **offset** and the range stops depending on the world at all.

```
worst position error, ships within an 80-unit view:

  world half       absolute       relative
         400        0.0125u        0.0254u
        4000        0.1224u        0.0254u
       40000        1.2210u        0.0254u
      400000       12.2075u        0.0254u
```

Flat, at 110 bits a ship against 119. At the smallest world absolute is still better, so it is a trade rather than a free win, but it stops being one from about 4000 units up.

**The first version of this did not work, and is worth keeping in the record.** Quantising the anchor over the world put the world's size straight back into the error, and relative came out very slightly *worse* than absolute at every size. The test passed anyway, because it compared growth **ratios**: relative started higher and grew more slowly, so a ratio comparison went green while the scheme was strictly worse. A ratio hides which curve is higher. The assertion now demands the error be *identical* at 400 and at 400000, and the anchor is sent at full width, where 96 bits once a frame amortises to nothing.

There is a round-trip test 200000 units from the origin, which is a place the absolute scheme cannot represent at all: it clamps by over a thousand units.

## Prediction, and the rule as code

`advance()` is the whole flight model, and it is a free function called by **both** the server's `Space::step` and the client's predictor. Not described twice, not behind a trait: the same code.

Reconciliation exists to absorb *network* disagreement. A second copy of the rule turns every frame into a correction instead, and no amount of smoothing recovers from that. `a_prediction_and_the_server_walk_the_same_line` steps both side by side for 600 ticks under a held input and asserts **exact equality**, so it fails the moment anyone writes a second flight model anywhere.

Two decisions inside it:

- **Orientation is predicted and never corrected.** It is driven entirely by local input, so the client is not guessing, and snapping it would fight the player's hand for no accuracy.
- **Corrections bleed off rather than snap.** Because the rule is shared they are small and constant instead of rare and large, and a teleport on each would be far more visible than carrying a little error for a few frames.

The panel shows **worst correction**, which is the number that says whether the shared rule is really shared.

## What a client has to do that cube_yard's does not

**Nothing announces that a ship left the radius.** There is no despawn message; the server simply stops mentioning it, and the absence *is* the message. So a client notices silence rather than waiting for something to arrive.

That makes the grace period a real decision. Dropping on the first silent frame makes the edge of the world **strobe**, because a ship near the radius flickers in and out of it as both ends drift. A run of silent frames turns that into a fade.

## The bug that only one kind of test finds

The simulation reasons in yaw and pitch because a flight model does. The wire carries a quaternion because smallest-three is 29 bits against 64, and because a client blending orientations wants something to slerp. Those are two expressions of one thing, and nothing forces them to agree.

They did not. A positive rotation about X takes +Z toward **-Y**, while the flight model calls positive pitch *nose up*: one sign, and every ship would have rendered pitched the wrong way. **Every position was correct throughout.** No positional test, no packing test and no relevance test would have noticed; only rotating the forward vector by the wire quaternion and comparing it against the simulation's own `facing()` catches it, and the test implements that rotation the long way so it shares no code with what it checks.

## Reading the numbers yourself

```sh
cargo test -p spacemo --test interest -- --nocapture   # the bandwidth of a dropped axis
cargo test -p spacemo --lib -- --nocapture             # churn, relative encoding, the rest
```
