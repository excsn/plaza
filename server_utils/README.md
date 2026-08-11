# `plaza_server_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The server half of real-time netcode, the counterpart to [`plaza_client_utils`](../client_utils/). Where the client crate holds prediction, interpolation, and smoothing, this holds what an authoritative server needs, starting with the rewind that lag compensation is built on.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_server_utils = "0.6"
```

Its only dependency is `plaza_client_utils`, for the shared `Interpolatable` and `ToF32` traits, plus `tracing`. No async runtime, so like the client crate it compiles to wasm: a server *simulation* can run in a browser, which the interactive [`netcode_playground`](../examples/netcode_playground/) example relies on.

## What it addresses

| Problem | Piece |
|---|---|
| A client aims at where a target *was* (it renders remotes in the past), so hits must be judged then, not now | `HistoricalStateBuffer` |
| A world has more entities than fit on the wire, and players in different places, so each client needs only what is near it | `relevance` (`SpatialGrid`, `VisibilitySet`, Morton keys) |
| Some of those entities are simulation *inputs*, so dropping the distant ones changes the answer, but sending them all does not scale | `aggregate` (`AggregateTree`) |
| Streaming that set as *entered* and *left* assumes every packet arrives, and one that does not is lost for good | `delta` (`DeltaBaseline`) |
| A bounded number of seats, where a fresh occupant must not inherit the last one's accumulated state | `seats` (`SeatTable`, `Seating`) |
| Seating policy: a lock for games that seat only between rounds, a ranked waitlist, displacement (a bot holds a seat only until a person wants one), seats held across an absence, bot-driven empties | `seats` (`Roster`, composed of `SeatSlots` and `RankedQueue`, both public) |
| A claim about bandwidth should be a number on screen, not an assertion in a README | `meter` (`RateMeter`) |
| A one-shot op with nothing behind it (a `Welcome`, a `Refused`) is lost for good on a lossy link, and nothing in the protocol will ever mention it again | `oneshot` (`Pending`) |
| An accuracy figure taken against the *present* charges a client for a render delay it chose, so the number grows with the buffer depth rather than with anything going wrong | `render_error` (`render_error_at`) |
| ...and that number should be a **rate**, not the session's average, which climbs for ever toward a level it never reaches | `RateMeter::per_sec` (windowed) against `lifetime_per_sec` |

`SetDigest`, `SlotKey`, `SlotAllocator` and `DeltaMirror` are re-exported from [`plaza_client_utils`](../client_utils/) rather than defined here. Both sides of a delta stream have to agree about them exactly, and a browser client needs them and must not inherit a server to get them.

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

## Filling the packet (priority, and what is asleep)

Relevance answers *who can see what*, which is a yes or no. It does not answer what comes next: a hundred entities are relevant, the budget holds twenty, so which twenty this tick? First-twenty-by-id starves the tail, and so does nearest-twenty. That is the difference between a bandwidth **budget** and a bandwidth **outcome**, where the packet is whatever size the world happened to be.

- **`PriorityAccumulator`**: Fiedler's accumulator from [state synchronization](https://gafferongames.com/post/state_synchronization/). Every entity gains priority each tick, the highest fill the budget, and **whatever did not fit keeps what it accumulated**, so waiting is itself what earns a slot. Nothing starves, the budget is respected exactly, and how often a thing updates becomes a rate you choose per entity rather than a consequence of the sort order. The walk continues past an entity too large to fit, so one big one near the front cannot leave the rest of the packet empty; its score keeps climbing until it wins outright. Ties break by index, so a server and a replay of it agree.
- **`SpatialGrid` indexes two axes**, which is the right shape for a plane and for most 3D games, since a landscape is locally 2.5D. In open volume it over-returns rather than missing: a query on `(x, z)` answers with a disc where a sphere was asked for, which spacemo measured at 7.1x the bandwidth per client with the game looking entirely correct. Filter what it returns on height; that is exact at the same query cost.

- **`RestDetector`**: in a settled scene most things are not moving, and saying so costs one bit against the thirty-three a velocity costs. That is Fiedler's at-rest flag, the cheapest trick in [snapshot compression](https://gafferongames.com/post/snapshot_compression/) because it needs nothing new on the wire. Knowing *which* is the part worth a type: a single quiet tick means nothing, since a body at the top of its arc has zero velocity and is about to fall, so rest is a **run** of quiet ticks while waking is immediate. Being slow to notice motion is visible; being slow to notice stillness only costs bandwidth. What counts as moving stays yours. A solver already knows, but at its own granularity: rapier sleeps an **island**, every body in a chain of contacts, so one cube jostling in a heap holds the whole heap awake and each of them pays a velocity to hold still. A per-body speed test feeding this type took cube_yard from 205 bodies claiming to be awake to 56, against 57 that had moved.

Both are indexed densely, so a `SlotKey` is already the index, and both compose: score an at-rest entity lower and it naturally updates less often without a special case anywhere.

[cube_yard](../examples/cube_yard/) prices them. 901 cubes at 4.20 Mbit/sec, which is the floor bit packing alone reaches, went to **0.25** under a 256 kbit budget, and adding delta encoding then bought 206 cubes refreshed per tick instead of 46 inside that same budget. Two things it found are worth knowing before you copy the shape: the per-entity cost you hand `fill` should be derived from your encoding rather than estimated (the guessed figure overran by 20%), and a budget is per link, so the frame stops being a broadcast.

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

## Streaming that set reliably (the half most people get wrong)

`VisibilitySet::diff` gives you *entered* and *left*, and the obvious next step is to send those and let each client keep a mirror. That is cheap and it has a failure built into it: the server diffs against **what it last sent**, which silently assumes every packet arrives. One dropped packet leaves the client permanently missing whatever it carried, and nothing in the stream ever mentions it again. Measured in [`horde_playground`](../examples/horde_playground/) at 25% loss: 185 corpses a client can never be told about, and render error at 73.7 px.

`delta::DeltaBaseline` diffs against **what the client acknowledged** instead, with `AckWindow` on the return path. Corpses fall to single digits and render error to 0.5 px, and digest mismatches to zero, for roughly three times the bandwidth at that loss rate. It owns the per-subscriber baselines, the acknowledgement frontier, the staleness rebuild and the digest drift check, and it never learns what a key means. Four details in it are load-bearing, and three were wrong in the first working version:

- **The baseline is the newest *contiguous* acknowledgement, not the newest bit set.** Receiving packet N+1 after losing N does not put a client in the state N+1 implies. Taking the newest set bit made recovery statistically indistinguishable from no recovery.
- **The keys carry generations.** A retraction re-derived after a slot was recycled names the slot's *current* occupant, so the client's lookup misses and the entity it actually holds is never mentioned again. Loss recovery is exactly what makes generations load-bearing, because re-deriving widens the window between naming an entity and reading the name.
- **The two halves of the diff need baselines built by opposite operations.** What to *send* must assume the least the client holds: the acknowledged state **intersected** with everything sent since. What to *retract* must assume the most: the acknowledged state **unioned** with everything announced since, because an entity that entered and left inside the gap appears in neither the baseline nor the current set. Getting one right and leaving the other raw trades one silent failure for its mirror image.
- **Cold start is a decision.** Recovery diffs against the acknowledged state, and there is no acknowledged state before the first acknowledgement, so the naive fallback is silently the very behaviour the mode exists to replace.

`RecoveryPolicy::Naive` keeps the broken behaviour available, because the failure is worth being able to demonstrate. The client's half is `plaza_client_utils::DeltaMirror`.

## Relationship to `plaza`

The `plaza` server framework keeps its own reconciliation helpers (`ServerInputBuffer`, `ClientInputTracker`) under `game_common::reconciliation`. Those are coupled to the server runtime; the pieces here are pure and portable, and will grow as more of the server half is decoupled.
