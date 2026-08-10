# cube_yard

A pile of 901 cubes, a solver nobody re-simulates, and one question: how few bits does it take to tell you where the pile went. It is the scene from Glenn Fiedler's [networked physics](https://gafferongames.com/post/snapshot_interpolation/) articles, at his cube count, so the numbers can be read against his.

```sh
./run-native.sh                                  # desktop window; hosts and plays
./run-native.sh --role client --connect ws://host:8100/ws
./wasm-serve.sh                                  # build the browser client, host it on :8100
cargo test -p cube_yard --test baseline -- --nocapture   # what the current stage costs
```

WASD or arrows drive your cube, space jumps. Drive it into the pile.

## The other family

[puck_rink](../puck_rink/README.md) is rollback: five bodies, fixed point, every client re-simulating the same step, and a digest proving two machines computed the same world. This is the opposite one. The server owns the only simulation, clients draw what arrives and never guess, and the entire engineering problem is bandwidth rather than agreement.

That inverts the physics configuration too, and for a reason rather than a preference. puck_rink's rapier backend must run `enhanced-determinism` and is therefore forbidden `parallel` and `simd8`. Here nothing re-simulates, so determinism buys nothing and `parallel` is free to take. Same crate, same version, opposite flags, and the netcode family is what decides.

## Where it is

**Stage 1 of 4.** Every cube, every tick, at full f32 width. That is the naive thing on purpose: it is the number the rest is measured against.

```
cubes            905
asleep           905 of 905
bytes per frame  49794
per cube         55.0 bytes
at 60 Hz         23.90 Mbit/sec per client
target            0.256 Mbit/sec  (93x to go)
```

Fiedler's uncompressed figure for the same scene is 17.38 Mbit/sec; ours is higher because MessagePack has envelope overhead and each cube also carries velocity and a rest flag.

The stages left, and what each one is allowed to spend:

- **2. Packing.** `plaza_wire::bits`: positions and velocities quantised to the precision the yard renders at, orientation as smallest-three, indices as deltas, at-rest as one bit. **Quantise-both-sides** lands here too, because this is where the failure becomes visible rather than theoretical.
- **3. Priority and a budget.** `PriorityAccumulator` and `RestDetector`, with the solver's own sleeping bodies as the at-rest input, filling a hard 256 kbit/sec. Magnitude-adaptive correction smoothing lands here, because only now do entities update at different rates.
- **4. Interpolation and delta.** `HermiteView` at a low send rate, then value-level delta encoding, measured the honest way: more cubes inside the same budget rather than a smaller number in isolation.

Every stage keeps a position-error readout beside the bandwidth. Compression without an error number is half a measurement.

## Drawing 901 tumbling cubes

macroquad's `draw_cube` takes a position and a size and **no rotation**, so it cannot draw a tumbling rigid body at all. Every cube therefore goes into one mesh whose vertices are rebuilt each frame. That turns out to be the fast path as well as the only correct one: 901 cubes cost about 158us to rebuild, under one percent of a 16.7ms frame, and the cost is linear in cubes rather than in draw calls. At 5000 cubes it is still only 4.5%.

The ceiling is `u16` mesh indices, which cap one mesh at 65535 vertices, so 2730 cubes. Past that it wants splitting into several.

## Structure

The usual listen-server shape: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`). MessagePack with a build-derived protocol version, derived from `protocol.rs` alone, because the simulation's state is rapier's and only the projection crosses.

`rapier3d` is behind the `server` feature, so the browser client never compiles a solver it would not run.
