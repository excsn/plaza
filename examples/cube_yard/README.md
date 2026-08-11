# cube_yard

A pile of 901 cubes, a solver nobody re-simulates, and one question: how few bits does it take to tell you where the pile went. It is the scene from Glenn Fiedler's [networked physics](https://gafferongames.com/post/snapshot_interpolation/) articles, at his cube count, so the numbers can be read against his.

```sh
./run-native.sh                                  # desktop window; hosts and plays
./run-native.sh --encoding packed --snap         # stage 2: quantised, bit-packed
./run-native.sh --encoding budgeted              # stage 3: a hard 256 kbit/sec
./run-native.sh --encoding delta                 # stage 4: deltas against what you hold
./run-native.sh --role client --connect ws://host:8100/ws
./wasm-serve.sh                                  # build the browser client, host it on :8100
cargo test -p cube_yard --test baseline -- --nocapture   # what the current stage costs
```

A wide flat field of cubes, evenly spaced on the floor, and one big cube you drive. WASD or arrows move; **enter switches mode**.

**Hovering** (the default) floats above the field with a repulsion field, so you shove cubes aside without ever touching them and plough visible furrows through the lattice. **Enter** drops you into **rolling**: you land, tumble along the ground, and magnetise whatever you run into, so a ball builds up as you plough through. A held cube sticks to the point on your **surface** it arrived at, kept in your own frame so the clump turns with you, and it is weightless while held: springing it toward a fixed *distance* instead lets gravity choose which point on that sphere, and gravity always chooses the bottom, so everything collects in a bag underneath. Space jumps, in roll mode only. Enter again and the ball scatters.

Grey means asleep, red means awake, which is the at-rest flag drawn directly: grey cubes are nearly free on the wire and red ones are what the bandwidth is being spent on.

The controls are a platformer's, and deliberately so. **The world is simulated and the player is driven**, which is the line worth drawing rather than a shortcut: horizontal velocity is *set* rather than pushed, so letting go stops you on the next tick, and the roll is read off the velocity that results instead of causing it. Gravity, jumping and every contact stay physical, and the weight of a gathered ball is a coefficient on the drive. Held cubes push nothing, because a solid ball is one you climb and one that wedges against the lattice; and a cube stuck underneath is not ground, or jump can be held down for ever. The cube also **rolls** at the rate it travels, a quarter turn per face width, because a cube that slides reads as a hockey puck; the roll axis is `up x velocity`, and getting the rate wrong looks like skidding one way and spinning on the spot the other. The camera sits at a fixed offset behind your cube and never orbits, which is what lets the input be plain world axes: a turning camera makes "left" mean a different direction every second.

## The other family

[puck_rink](../puck_rink/README.md) is rollback: five bodies, fixed point, every client re-simulating the same step, and a digest proving two machines computed the same world. This is the opposite one. The server owns the only simulation, clients draw what arrives and never guess, and the entire engineering problem is bandwidth rather than agreement.

That inverts the physics configuration too, and for a reason rather than a preference. puck_rink's rapier backend must run `enhanced-determinism` and is therefore forbidden `parallel` and `simd8`. Here nothing re-simulates, so determinism buys nothing and `parallel` is free to take. Same crate, same version, opposite flags, and the netcode family is what decides.

## Where it is

**All four stages.** Reproduce with `cargo test -p cube_yard --test baseline -- --nocapture`.

```
905 cubes, 901 asleep, one frame at 60 Hz

stage                     bytes   Mbit/sec vs stage 1  worst error
1  full width             49800      23.90      1.0x        exact
2  quantised + packed      8756       4.20      5.7x      0.0033u
3  + priority budget        489       0.23    101.8x      0.0033u
4  + delta encoding         532       0.26     93.6x      0.0033u

   cubes refreshed per tick, inside the same budget:
     stage 3      43
     stage 4     417

   mean quantisation error 0.00192 units, on cubes one unit across
   worst single packet 499 bytes against a 507 byte budget
```

The error column moved when the floor did, and that is the trade an endless stage makes on the wire. Quantisation spends a fixed number of bits over a **bounded** range, so widening the world four times over costs four times the step at the same 16 bits: 0.0008 units became 0.0033. Still a three-hundredth of a cube and still invisible, but it is the reason the floor is finite at all rather than a plane going on for ever. Buy the precision back with two more bits an axis if a world ever needs it.

**The target is met at stage 3**, and the thing worth saying plainly is that the last 16x was not compression at all. Quantising has a floor: 905 cubes times the smallest honest encoding is still 4.2 Mbit/sec, and no number of bits saved per cube reaches 256 kbit. What closed it was choosing.

Which is also why stage 4's row is the wrong way to read it. The bandwidth was already at the ceiling, so delta encoding cannot lower it; what it buys is **ten times more of the yard inside the same budget**, 417 cubes a tick against 43. Every cube is refreshed about every other tick instead of every twenty.

Both rows are measured at the wire, envelope included, which is what the target is a number about. Budgeting only the payload overshoots by the op tag, the tick, the stamp and the byte-string header, and at 60Hz that is 12 kbit/sec of quiet overspend.

Stage 1 is the naive thing on purpose. Fiedler's uncompressed figure for the same scene is 17.38 Mbit/sec; ours is higher because MessagePack has envelope overhead and each cube also carries velocity and a rest flag.

Stage 2 is `plaza_wire::bits` doing what a derive cannot: positions on a bounded grid at 16 bits an axis, orientation as smallest-three at 29 bits instead of 128, velocity only when the cube is awake, and one bit for at-rest. The layout is in [`src/pack.rs`](src/pack.rs), and so is the reader, which is the honest cost of the 5.7x: two functions that must agree with only a comment holding them together. That is why the envelope stays MessagePack and only the hot array gets this treatment.

The error column is the point of doing it this way. A worst case of 0.0008 units on cubes a full unit across is four ten-thousandths of a cube, which is not a visible thing, and now it is a number rather than a hope.

Stage 3 is `PriorityAccumulator` and `RestDetector` in [`src/budget.rs`](src/budget.rs). Every cube gains priority each tick (an awake one far more than a sleeping one, a near one more than a far one), the highest fill 4266 bits, and **what did not fit keeps what it accumulated**, so waiting is itself what earns the next slot. Nothing starves, and there is a test that says so: run a still yard for 80 ticks and every one of the 905 has had a turn.

Two things fell out of building it. A budget is **per link**, so the frame stopped being a broadcast: each client is scored from where it is standing and gets its own packet, and a joiner is handed the whole yard once rather than learning it over several seconds. And the per-cube cost is derived from the layout (`pack::cube_bits`) rather than written down beside it, because the first hand-guessed figure overran the budget by 20% and a constant like that drifts silently the moment the layout changes.

A budget also makes corrections bigger: a distant cube can wait several ticks and then move a long way at once. That is what `plaza_client_utils::AdaptiveDecay` is for, and it is the third technique from the articles that plaza was missing.

Stage 4 encodes each cube against what the client is known to hold. A cube that has not moved costs **eight bits**: an index delta and three flags, against the eighty-two an absolute sleeping cube costs. In a settled yard that is nearly all of them, which is what turns a refresh from expensive into free.

Two things about it are worth knowing. It needs no acknowledgements, unlike Fiedler's, because plaza's WebSocket transport is TCP: what was last *sent* is what the other end holds, in order. On a datagram transport this would have to delta against an acked baseline instead, which is what `plaza_server_utils::DeltaBaseline` is for.

**Stage 5 removes that dependency**, and prices it. `--encoding delta` measures from what was last *sent*; an acknowledged baseline measures from what the client has confirmed, so a lost frame costs bandwidth rather than correctness. Over 400 ticks on a deterministically lossy link:

```
loss             delta vs last sent      delta vs acknowledged
           worst err     cubes/tick   worst err     cubes/tick
0%             0.003            218       0.003            125
2%             1.129            218       0.003            125
10%            6.717            218       0.003             96
```

Two things in that table were not what was expected. **The bytes are identical**, because a budget is a ceiling and both schemes spend all of it; the premium for an older baseline cannot show up as bandwidth, so it shows up as **roughly half the cubes per tick**. And the acked column does not move at all as loss climbs, which is the property being bought: an acknowledged baseline cannot be wrong, only old.

The scene it is measured on matters, and the first version of this test got it wrong. Run over a **settled** yard both schemes reported 0.003 at every loss rate, because a cube that is not moving deltas identically from any baseline: the test was measuring nothing and passing. It now drives a player through the field, and the divergence appears immediately.

Getting it right needed one thing the naive version does not have: **the frame has to name the baseline it was measured from**. The first attempt had the server encoding against its confirmed baseline while the client decoded against everything it had received since, which are different reference points, and the lossless control caught it at 2.0 units of error. Both ends now run the same reconstruction (`Acked::view_at`) over the same per-cube history, which is Fiedler's "5-bit offset identifying which packet contains the delta base" in a different shape.

The other half is a lesson plaza had already written down: the baseline is the newest **contiguous** acknowledgement, not the newest bit set, which is what `AckWindow::contiguous_base` is for.

That dependency is measured rather than asserted. [`tests/agreement.rs`](tests/agreement.rs) drives the real server encode path into the real client decode path and checks every cube a frame names against where the server holds it, then drops a single frame and watches the yard corrupt by **0.273 units** on cubes one unit across, over a hundred times the quantisation step, with no error raised anywhere. The second test is there as much to prove the first one has teeth: a check that has never failed is weak evidence that it could. And a delta frame has its own wire variant rather than a flag, because the two layouts are not distinguishable from their bytes and guessing wrong would decode garbage into a baseline both ends have to agree on.

Stage 4 does not plan against a cost estimate at all, and that is the second thing it taught. A delta cube costs anywhere between eight bits and a full absolute, and any single estimate covering that range has to be generous enough to waste most of the budget: the first version allowed 15 bits for an index delta that is usually 5 and finished 40% under. Adding a cube never shrinks the payload, so the largest prefix of the priority order that fits is found by bisecting on the *written* size, about ten trial encodes a tick. That claimed the headroom: 417 cubes instead of 206.

It also needed a change in `PriorityAccumulator`. `fill` clears the score of everything it picks, which is only sound while the cost you hand it cannot under-count; a caller that packs until full does not know what it sent until afterwards, and clearing an entity that never travelled is exactly the starvation the type exists to prevent. So `order` ranks without clearing and `sent` clears what actually went, which is the pair a measured fit needs.

## Interpolating between sparse updates

A budget means a cube can wait several ticks between updates, which is the problem a low send rate has, and it takes the same fix. `plaza_client_utils::HermiteView` splines through both samples and leaves along the velocity recorded at each.

One cube, chosen by index and falling smoothly, gives the flattering answer: 0.0588 units against a straight line's 0.1219, so 2.1x. Across 300 cubes it reverses completely.

```
500 cubes at 10 sends a second, worst position error:
  hold the newest sample   0.5315u
  interpolate straight     0.0652u   (8.2x better than hold)
  spline through velocity  2.5348u   (38.9x WORSE than straight)

  the spline left the segment its samples bracket on 5% of frames, by up to 2.53u
```

Interpolating beats holding by 8.2x, which is the expected win and the reason to send at a low rate at all. The spline then **loses to the straight line by 39x**, and the overshoot number says why: a chord cannot leave the segment between its two samples and a spline can. Velocity at a sample is a promise about the path to the next one, and in a pile of colliding cubes that promise is broken after the packet has left.

The single-cube figure was not wrong, it was one cube, and picking an index is not sampling. That is the whole lesson.

The overshoot figure carries a second one. It read 50% of frames until the comparison was restricted to frames two samples actually **bracket**: past the newest sample there is nothing to interpolate toward, so "interpolate" silently becomes "hold", and those frames were being counted for both. With them excluded the straight line's win goes from 3.1x to 8.2x and the spline's loss from 13x to 39x. Both directions were being flattened by the same defect, which is what a comparison against a degenerate case does.

So **cube_yard's client does not use `HermiteView`**, and that is the finding rather than a disappointment. It blends between two real samples with `SnapshotBuffer`, because a chord is bounded by the states it sits between and this scene needs exactly that. The spline's home is a steered character or a projectile in free flight, where velocity at a sample really does predict the path; the example that motivated building it is the example that should not use it. Take the 2x and do not expect the 484x on anything with contacts in it.

## Quantise both sides, and what it costs

Fiedler names quantising the simulation on both sides as the critical trick in [state synchronization](https://gafferongames.com/post/state_synchronization/): the server simulating at a precision it never transmits means the client is always looking at a rounded copy of a truth that has already moved on. `--snap` turns it on.

Doing it naively **destroyed the thing it was supposed to help**, and the number is worth keeping. Snapping every body every tick took the settled pile from 901 asleep to **0**. A resting cube jitters by less than one quantisation step, so it is re-snapped forever, and writing a body's position marks it modified, which is enough that it never reaches the sleep threshold. Keying on `is_sleeping` does not rescue it either, because that is precisely the state it can no longer get into.

Keying on **motion** breaks the circle, and the rule it leaves is the one that was always right: a body that is not moving is not drifting, so there is no divergence for snapping to prevent. With that, the pile settles to 901 asleep exactly as it does without snapping, and the two runs end up 0.009 units apart on average, 0.357 at worst.

Worth stating plainly because the articles do not: the technique has a cost, it lands on the at-rest optimisation, and at-rest is worth more.

**And in this example it buys nothing measurable.** The obvious hypothesis once deltas are on is that snapping pins a body jittering below one quantisation step, so its delta reads "unchanged" instead of flipping back and forth. Measured over 120 ticks of a settling yard, both runs seeing identical motion: 41894 bytes against 41806, a difference of **0.2%**, which is noise.

That is not a refutation of Fiedler, it is a statement about what cube_yard is. His justification for quantising both sides is that the *client* extrapolates, running the simulation forward between updates, so a client holding digits the server never sent diverges as it integrates. Nothing here extrapolates: the client draws what arrived and eases the correction. `--snap` is implemented and honest about costing nothing, and the condition under which it would earn its place is a client that simulates.

Every stage keeps a position-error readout beside the bandwidth. Compression without an error number is half a measurement.

## Drawing 901 tumbling cubes

macroquad's `draw_cube` takes a position and a size and **no rotation**, so it cannot draw a tumbling rigid body at all. Every cube therefore goes into a mesh whose vertices are rebuilt each frame. That is the fast path as well as the only correct one: 901 cubes cost about 158us to rebuild, under one percent of a 16.7ms frame, and the cost is linear in cubes rather than in draw calls. At 5000 cubes it is still only 4.5%.

**In chunks of 128, and that number was paid for.** macroquad's batcher has `draw_call_vertex_capacity` of 10000 and `draw_call_index_capacity` of 5000, and `geometry()` *clamps* anything larger: it warns and then draws the front of the buffer. One mesh of 905 cubes is 21720 vertices and 32580 indices, so roughly a quarter of the yard was drawn and the rest simply was not there. Indices bind first, at 36 per cube, putting the true ceiling at 138 cubes per call rather than the 2730 the `u16` index type suggests.

The rendering spike that was supposed to catch this did catch it, and the output was filtered away: the run printed the warning on every frame and the check grepped for its own success line. A spike that greps for what it hoped to see is not a spike.

## Structure

The usual listen-server shape: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`). MessagePack with a build-derived protocol version, derived from `protocol.rs` alone, because the simulation's state is rapier's and only the projection crosses.

`rapier3d` is behind the `server` feature, so the browser client never compiles a solver it would not run, and the sizes say so: **2.70MB** after `wasm-opt -Oz`, against puck_rink's 6.22MB where the client re-simulates and the solver has to ship with it.
