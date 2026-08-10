# cube_yard

A pile of 901 cubes, a solver nobody re-simulates, and one question: how few bits does it take to tell you where the pile went. It is the scene from Glenn Fiedler's [networked physics](https://gafferongames.com/post/snapshot_interpolation/) articles, at his cube count, so the numbers can be read against his.

```sh
./run-native.sh                                  # desktop window; hosts and plays
./run-native.sh --encoding packed --snap         # stage 2: quantised, bit-packed
./run-native.sh --encoding budgeted              # stage 3: a hard 256 kbit/sec
./run-native.sh --role client --connect ws://host:8100/ws
./wasm-serve.sh                                  # build the browser client, host it on :8100
cargo test -p cube_yard --test baseline -- --nocapture   # what the current stage costs
```

WASD or arrows drive your cube, space jumps. Drive it into the pile.

## The other family

[puck_rink](../puck_rink/README.md) is rollback: five bodies, fixed point, every client re-simulating the same step, and a digest proving two machines computed the same world. This is the opposite one. The server owns the only simulation, clients draw what arrives and never guess, and the entire engineering problem is bandwidth rather than agreement.

That inverts the physics configuration too, and for a reason rather than a preference. puck_rink's rapier backend must run `enhanced-determinism` and is therefore forbidden `parallel` and `simd8`. Here nothing re-simulates, so determinism buys nothing and `parallel` is free to take. Same crate, same version, opposite flags, and the netcode family is what decides.

## Where it is

**Stage 3 of 4.** Reproduce with `cargo test -p cube_yard --test baseline -- --nocapture`.

```
905 cubes, 905 asleep, one frame at 60 Hz

stage                     bytes   Mbit/sec vs stage 1  worst error
1  full width             49800      23.90      1.0x        exact
2  quantised + packed      8740       4.20      5.7x      0.0008u
3  + priority budget        512       0.25     97.3x      0.0008u

   mean quantisation error 0.00048 units, on cubes one unit across
   worst single packet 517 bytes against a 533 byte budget
   target 0.256 Mbit/sec: 0.96x
```

**The target is met.** Which is the thing worth saying plainly: the last 16x was not compression at all.

Stage 1 is the naive thing on purpose. Fiedler's uncompressed figure for the same scene is 17.38 Mbit/sec; ours is higher because MessagePack has envelope overhead and each cube also carries velocity and a rest flag.

Stage 2 is `plaza_wire::bits` doing what a derive cannot: positions on a bounded grid at 16 bits an axis, orientation as smallest-three at 29 bits instead of 128, velocity only when the cube is awake, and one bit for at-rest. The layout is in [`src/pack.rs`](src/pack.rs), and so is the reader, which is the honest cost of the 5.7x: two functions that must agree with only a comment holding them together. That is why the envelope stays MessagePack and only the hot array gets this treatment.

The error column is the point of doing it this way. A worst case of 0.0008 units on cubes a full unit across is four ten-thousandths of a cube, which is not a visible thing, and now it is a number rather than a hope.

Stage 3 is `PriorityAccumulator` and `RestDetector` in [`src/budget.rs`](src/budget.rs). Every cube gains priority each tick (an awake one far more than a sleeping one, a near one more than a far one), the highest fill 4266 bits, and **what did not fit keeps what it accumulated**, so waiting is itself what earns the next slot. Nothing starves, and there is a test that says so: run a still yard for 80 ticks and every one of the 905 has had a turn.

Two things fell out of building it. A budget is **per link**, so the frame stopped being a broadcast: each client is scored from where it is standing and gets its own packet, and a joiner is handed the whole yard once rather than learning it over several seconds. And the per-cube cost is derived from the layout (`pack::cube_bits`) rather than written down beside it, because the first hand-guessed figure overran the budget by 20% and a constant like that drifts silently the moment the layout changes.

A budget also makes corrections bigger: a distant cube can wait several ticks and then move a long way at once. That is what `plaza_client_utils::AdaptiveDecay` is for, and it is the third technique from the articles that plaza was missing.

The stage left:

- **4. Interpolation and delta.** `HermiteView` at a low send rate, then value-level delta encoding, measured the honest way: more cubes inside the same budget rather than a smaller number in isolation.

## Quantise both sides, and what it costs

Fiedler names quantising the simulation on both sides as the critical trick in [state synchronization](https://gafferongames.com/post/state_synchronization/): the server simulating at a precision it never transmits means the client is always looking at a rounded copy of a truth that has already moved on. `--snap` turns it on.

Doing it naively **destroyed the thing it was supposed to help**, and the number is worth keeping. Snapping every body every tick took the settled pile from 905 asleep to **0**. A resting cube jitters by less than one quantisation step, so it is re-snapped forever, and writing a body's position marks it modified, which is enough that it never reaches the sleep threshold. Keying on `is_sleeping` does not rescue it either, because that is precisely the state it can no longer get into.

Keying on **motion** breaks the circle, and the rule it leaves is the one that was always right: a body that is not moving is not drifting, so there is no divergence for snapping to prevent. With that, the pile settles to 905 asleep exactly as it does without snapping, and the two runs end up 0.011 units apart on average.

Worth stating plainly because the articles do not: the technique has a cost, it lands on the at-rest optimisation, and at-rest is worth more.

Every stage keeps a position-error readout beside the bandwidth. Compression without an error number is half a measurement.

## Drawing 901 tumbling cubes

macroquad's `draw_cube` takes a position and a size and **no rotation**, so it cannot draw a tumbling rigid body at all. Every cube therefore goes into one mesh whose vertices are rebuilt each frame. That turns out to be the fast path as well as the only correct one: 901 cubes cost about 158us to rebuild, under one percent of a 16.7ms frame, and the cost is linear in cubes rather than in draw calls. At 5000 cubes it is still only 4.5%.

The ceiling is `u16` mesh indices, which cap one mesh at 65535 vertices, so 2730 cubes. Past that it wants splitting into several.

## Structure

The usual listen-server shape: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`). MessagePack with a build-derived protocol version, derived from `protocol.rs` alone, because the simulation's state is rapier's and only the projection crosses.

`rapier3d` is behind the `server` feature, so the browser client never compiles a solver it would not run.
