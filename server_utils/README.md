# `plaza_server_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The server half of real-time netcode, the counterpart to [`plaza_client_utils`](../client_utils/). Where the client crate holds prediction, interpolation, and smoothing, this holds what an authoritative server needs, starting with the rewind that lag compensation is built on.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_server_utils = "0.1"
```

Its only dependency is `plaza_client_utils`, for the shared `Interpolatable` and `ToF32` traits, plus `tracing`. No async runtime, so like the client crate it compiles to wasm: a server *simulation* can run in a browser, which the interactive [`netcode_playground`](../examples/netcode_playground/) example relies on.

## What it addresses

| Problem | Piece |
|---|---|
| A client aims at where a target *was* (it renders remotes in the past), so hits must be judged then, not now | `HistoricalStateBuffer` |
| A world has more entities than fit on the wire, and players in different places, so each client needs only what is near it | `relevance` (`SpatialGrid`, `VisibilitySet`, Morton keys) |
| Some of those entities are simulation *inputs*, so dropping the distant ones changes the answer, but sending them all does not scale | `aggregate` (`AggregateTree`) |

## Lag compensation in one paragraph

Because clients render other entities slightly in the past (entity interpolation), a player aims at a target's past position. To judge a time-sensitive action such as a shot fairly, the server rewinds its authoritative world to the instant the client saw, and checks the hit there.

```rust,ignore
// Each server tick, record where every entity is:
history.record_state(entity_id, server_time, entity_state);

// When a shot arrives stamped with the time the client was seeing:
if let Some(past) = history.get_state_at_or_before(&target_id, shot.aim_time) {
    if hit_test(shot.aim, &past) {
        // A hit, judged against the world as the shooter saw it.
    }
}
```

The state type is yours, and the same type can feed both this buffer and a client's `SnapshotBuffer`, because both use the shared `Interpolatable` trait.

## Relevance (interest management)

A multiplayer world is bigger than one screen and its players stand apart, so sending every entity to every client is `players x entities` per tick and does not scale, a horde game (thousands of short-lived enemies) makes that plain. The `relevance` module is the mechanism for sending each client only what it needs, and streaming the churn:

- **Morton (Z-order) keys** (`relevance::morton`): interleave 2D or 3D integer cell coordinates into one locality-preserving integer. The math primitive under the grid, usable alone for locality sorts and broadphase.
- **`GridQuantizer` + `SpatialGrid`**: bucket entities into grid cells so a viewer gathers the ids near it (`query_radius`) without scanning the world. Rebuilt each tick; the buckets keep their capacity, so a steady entity count does not churn the heap.
- **`VisibilitySet`**: a dense bitset of who is visible to one client, with a fast word-at-a-time diff (`entered = new & !old`, `left = old & !new`) that is exactly the spawn/despawn stream a client applies each tick.

What stays the app's: the cell size and origin, the relevance rule (a radius, a frustum, a team), and how the streams are encoded on the wire. This only makes them cheap to compute. The [`relevance_demo`](examples/relevance_demo.rs) example (`cargo run --example relevance_demo -p plaza_server_utils`) shows a field of entities and moving players, and reports the bandwidth the filter saves.

## Aggregation (when relevance is the wrong question)

Relevance gives a binary answer: in the set or out of it. That is right for entities a client merely *draws*, and wrong for entities it has to *compute* with, because dropping an input silently changes the result. Measured in [`blackhole_playground`](../examples/blackhole_playground/) with 64 gravitational attractors: culling the distant ones by view distance cut the field's share of the traffic from 280 to 33 KiB/s and multiplied the client's simulation error by 2.4x, because a hole you were not told about still bends every pellet you hold.

`aggregate::AggregateTree` is the third option between sending everything and sending nothing: **keep the distant contribution, drop only its resolution**. It is the Barnes-Hut construction, a quadtree over weighted points walked once per viewer, replacing a distant group with one stand-in at its weighted centroid whenever the group's cell width over its distance falls below an opening angle `theta`. Build cost is O(n log n) once per tick regardless of how many clients ask; each viewer gets O(log n) summaries.

```rust,ignore
// Once per tick, over the whole set. Prefer `build_in` with the world's own
// bounds: `build` fits the cell to the current extent, so one entity drifting
// outward re-centres the whole subdivision and clusters re-form for reasons
// unrelated to their members.
let tree = AggregateTree::build_in(&points, world_center, world_size, 10);

// Once per viewer, reusing the output buffer.
tree.summarize(eye.x, eye.y, 0.5, &mut out);
for summary in &out {
  if summary.count == 1 {
    send_exactly(tree.members(summary)[0]);   // close enough to resolve
  } else {
    send_summary(summary.x, summary.y, summary.weight);  // a stand-in for many
  }
}
```

`theta = 0.0` accepts nothing and returns every point exactly, so "aggregation off" is the same code path rather than a second implementation. Two things the measurement was clear about and intuition was not:

- **It has a safe range, not a monotone dial.** Past about `theta = 1.0` the criterion starts accepting cells the viewer is sitting close to, dropping a whole quadrant's weight onto a single nearby point. At `theta = 1.2` the black hole example is *worse than culling*: a spurious concentration beats a missing force for damage.
- **Know which resource you are buying back.** In that example the field was a third of the bandwidth and all of the per-machine compute, so coarsening it halved the work (7.6 to 3.7 M force evaluations/sec at `theta = 0.5`) while total bandwidth moved 17%. Check your own byte breakdown before assuming this is a bandwidth technique.

Nothing in the module knows what a weight is. It is mass for a gravity field in `blackhole_playground`, and a headcount in `horde_playground`, where one tree over 3000 enemies gives each player twelve stand-ins covering 98% of the world outside their view radius for about 3% more bandwidth (relevance culling gives none of it, at any bandwidth). It is equally a cluster's threat for target selection, or an accumulated noise level.

Those two consumers also settle how to pick `theta`, and the answer is that there is no single right value. The field version has a hard ceiling near `1.0`, past which it is worse than culling; the crowd version is comfortable at `1.5`. The difference is what consumes the summaries: a *simulation* compounds the approximation into error, a *drawing* does not. **How coarse an approximation may be is a property of the consumer.** The only requirement is that the quantity be additive and that a distant group be adequately described by its weighted centroid.

## Relationship to `plaza`

The `plaza` server framework keeps its own reconciliation helpers (`ServerInputBuffer`, `ClientInputTracker`) under `game_common::reconciliation`. Those are coupled to the server runtime; the pieces here are pure and portable, and will grow as more of the server half is decoupled.
