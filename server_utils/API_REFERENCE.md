# API Reference: `plaza_server_utils`

## 1. Introduction & Core Concepts

`plaza_server_utils` is the **server half** of the pure netcode primitives, mirroring `plaza_client_utils`. It is pure logic with no async runtime, so a server simulation compiles to wasm alongside its clients, and it shares the client crate's `Interpolatable` and `ToF32` traits so one state type serves both sides.

It provides the building block lag compensation rests on (a rewind of past entity state), the `relevance` building blocks for interest management (deciding what each client needs to see), `aggregate` for the entities a client must compute with rather than merely draw, `delta` for streaming that set to a subscriber reliably, and the two small blocks a real server writes anyway: `seats` and `meter`.

**What is re-exported rather than defined here.** `SetDigest`, `SlotKey`, `SlotAllocator` and `DeltaMirror` come from `plaza_client_utils`. Both sides of a delta stream have to agree about them exactly, and a second implementation that agrees today is a disagreement waiting to happen, whose failure would present as a divergence about the *world* rather than about the arithmetic. The direction is not arbitrary: the client crate is the lower one, and a browser client needs all four and must not inherit a server to get them.

## 2. Error Handling

This crate defines no error type. `HistoricalStateBuffer::get_state_at_or_before` returns `Option`, `None` meaning the entity has no recorded history.

## 3. Core API

### Struct `HistoricalStateBuffer<EntityId, EntityStateSnapshot, ServerTime>`

A rolling per-entity history of states, queryable by time, so the server can reconstruct the world as a client saw it.

Bounds: `EntityId: Eq + Hash + Clone + Debug`; `EntityStateSnapshot: Clone + Debug` (and `Interpolatable<ServerTime>` when queried between two times); `ServerTime: Copy + Debug + Default + PartialOrd + Ord + Sub<Output = Self> + ToF32`. Because `ToF32` covers `u64` and `Duration`, millisecond or tick time works directly, this is the difference from the earlier `plaza` version, whose `TryInto<f32>` bound `u64` could not satisfy.

*   **`new(max_snapshots_per_entity: usize) -> Self`**: keeps at most this many states per entity. **Panics if 0.**
*   **`record_state(&mut self, entity_id, server_time, state)`**: record one entity's state for a tick. A state not newer than the last recorded is ignored, so history stays strictly increasing.
*   **`get_state_at_or_before(&self, entity_id, target_server_time) -> Option<EntityStateSnapshot>`**: the rewind. Interpolates between the two recorded states bracketing the target; clamps to the oldest or newest when the target is outside the retained range; `None` if the entity is unknown. Requires `EntityStateSnapshot: Interpolatable<ServerTime>`.
*   **`remove_entity_history(&mut self, entity_id)`**, **`clear_all_history(&mut self)`**

### Struct `TimedState<ServerTime, State>`

One recorded entry: `time`, `state`. Exposed for callers that inspect raw history.

### Shared traits

`Interpolatable` and `ToF32` are re-exported from `plaza_client_utils`. Implement `Interpolatable<ServerTime>` on your snapshot type to allow interpolated rewinds; it is the same trait a client implements for its `SnapshotBuffer`, so a single impl covers both.

## 4. Sizing the buffer

`max_snapshots_per_entity` should cover the largest rewind you will ask for: roughly `(max expected client latency + interpolation delay) / server tick interval`, plus a margin. Too small and old-enough shots clamp to the oldest retained state instead of rewinding accurately.

## 5. Relevance (module `relevance`)

Interest management: sending each client only the entities near it, and the change since last tick. Building blocks, not a policy, the cell size, the relevance rule, and the wire encoding stay yours.

### Module `relevance::morton`

Z-order curve encoding, integer coordinates interleaved into one locality-preserving key.

*   **`encode_2d(x: u32, y: u32) -> u64`** / **`decode_2d(u64) -> (u32, u32)`**: full 32-bit coordinates.
*   **`encode_3d(x, y, z: u32) -> u64`** / **`decode_3d(u64) -> (u32, u32, u32)`**: each coordinate uses its low **21 bits** (three axes share 63 bits).

### Struct `GridQuantizer`

Maps continuous world coordinates onto a uniform integer grid.

*   **`new(origin: (f32, f32), cell_size: f32)`**: `origin` is the world's minimum corner (points at or below it clamp to cell 0); `cell_size` near a viewer's relevance radius keeps a view a few cells wide. **Panics if `cell_size` is not positive.**
*   **`cell(x, y) -> (u32, u32)`**, **`key(x, y) -> u64`** (Morton key of the cell), **`cells_for_radius(radius) -> u32`**, **`cell_size() -> f32`**.

### Struct `SpatialGrid<Id: Copy>`

Buckets entity ids into cells for range queries. Rebuild each tick.

*   **`new(GridQuantizer)`**, **`clear(&mut self)`** (empties buckets, keeps capacity), **`insert(&mut self, id, x, y)`**.
*   **`query_radius(&self, x, y, radius, out: &mut Vec<Id>)`**: appends every id in the cells overlapping the region, a cell-granular *superset*, so apply an exact distance test after. `out` is not cleared, so reuse one `Vec` to avoid per-query allocation.
*   **`quantizer(&self) -> &GridQuantizer`**.

### Struct `VisibilitySet`

A dense bitset of which entities (by `u32` index) are visible to one client, with a fast diff.

*   **`new()`** / **`with_capacity(max_index: u32)`**, **`clear`**, **`insert(index)`**, **`contains(index) -> bool`**, **`count() -> usize`**, **`iter()`** (ascending).
*   **`diff(&self, previous, entered: &mut Vec<u32>, left: &mut Vec<u32>)`**: appends newly-visible indices to `entered` (`self & !previous`) and no-longer-visible to `left` (`previous & !self`), word at a time, the spawn/despawn stream. Vectors are not cleared, so reuse them.

For sparse handles (`Uuid`), map to dense indices first, or diff two sorted lists; this is the dense-index fast path. See the [`relevance_demo`](examples/relevance_demo.rs) example.

### Struct `SetDigest` (re-exported)

An order-independent checksum of a set of `u64` keys, for giving a delta-relevance stream a liveness check. A client that streams only *entered* and *left* events has no way to notice it has drifted out of step; sending a digest of what the server believes the client's set to be lets the client compare against its own and ask for a resync.

*   **`new()`**, **`from_keys(keys)`**, **`insert(key)`**, **`remove(key)`**, **`clear()`**, **`len() -> u32`**, **`is_empty() -> bool`**.
*   **`digest(&self) -> u64`**: the value to compare. Summation of a mixed hash per key, so it is order independent and maintainable incrementally: adding then removing the same key returns to the previous digest exactly.

`VisibilitySet::digest()` computes the same value over a bitset's membership. Defined in `plaza_client_utils::digest` and re-exported here, so both ends of a stream fold the same arithmetic; its full documentation is in that crate's reference.

**An integrity check that only detects is half a tool.** A digest says *that* a mirror is wrong and never *how*, and one horde bug it caught still took days, because the counter was unactionable. Pair it with an opt-in mode that ships the ground truth it is a checksum of (`DeltaMirror::divergence_from` is the consumer side); the cost is a switch and some bandwidth while it is on. In that case the diagnosis was immediate: every missing key was generation zero and spread evenly across the whole slot range, which is the signature of a client that joined an arena already in progress, not of a drift.

Attach the comparison to the **report**, not to the state change. Running it only when an acknowledgement advances the frontier skips the one case it is for: a client re-acknowledging the same sequence still tells you what it holds, and a mirror that loses something *without* losing a packet reports it exactly then.

## 6. Aggregation (module `aggregate`)

The third option between sending every entity and sending none, for entities a client must *compute* with rather than merely draw. Relevance culling drops a distant contribution entirely; aggregation keeps it and drops only its resolution, replacing a distant group with one stand-in at its weighted centroid. This is the Barnes-Hut construction.

### Struct `WeightedPoint`

`{ x: f32, y: f32, weight: f32 }`, with **`new(x, y, weight)`**. The weight is whatever additive quantity the consumer superposes: mass, headcount, threat, loudness.

### Struct `AggregateTree`

*   **`build(points: &[WeightedPoint], max_depth: u8)`**: fits the root cell to the points' extent. Convenient for a one-off.
*   **`build_in(points, center: (f32, f32), size: f32, max_depth: u8)`**: fixed root cell. **Prefer this when rebuilding every tick over moving points**: `build` re-centres the whole subdivision whenever the extent changes, so clusters re-form for reasons unrelated to their members and a consumer integrating the summaries sees the field twitch. Points outside the cell are still included; the tree just stops being balanced.
*   **`summarize(&self, x: f32, y: f32, theta: f32, out: &mut Vec<Summary>)`**: walks from a viewpoint, accepting a node when its cell width over its distance falls below `theta`. `out` is cleared and reused, so a per-frame walk allocates nothing after the first call. `theta <= 0.0` accepts nothing and returns every point exactly, which is the intended off switch.
*   **`members(&self, summary) -> &[u32]`**: the original input indices a summary stands for.
*   **`len()`**, **`is_empty()`**.

`max_depth` bounds the recursion and is required, because coincident points cannot be separated by subdivision. A leaf that hits the limit holds several points and is summarized as a group.

### Struct `Summary`

`{ x, y, weight, count, size }`. `count == 1` means nothing was approximated, so the consumer can send the real entity rather than a stand-in; near entities always come back this way, because a close node fails the opening-angle test.

### Choosing `theta`

Not a monotone dial. Below roughly `0.7` it is sound and trades accuracy for work smoothly; past about `1.0` the criterion begins accepting cells the viewer sits close to, collapsing a whole quadrant's weight onto one nearby point, and the result gets worse than culling. Measure at your own scale; `blackhole_playground`'s report table (section 1d) is a worked comparison against both baselines.

**How coarse an approximation may be is a property of the consumer, not of the approximation.** The same tree summarises a gravity field in `blackhole_playground`, where the summaries feed a *simulation* and compound into physics error, and a crowd in `horde_playground`, where they feed a *drawing* and nothing integrates them. The first has a hard ceiling near `1.0`; the second is comfortable at `1.5`. The instinct to look for one right value of theta is the thing to resist.

**Check which resource it buys back before believing it addresses yours.** In the black hole example the field was a third of the bandwidth and all of the per-machine compute, so coarsening it halved the work (7.6 to 3.7 M force evaluations/sec at `theta = 0.5`) while total bandwidth moved 17%. Aggregation reads like a bandwidth technique and is often a compute one.

## 7. Delta streaming (module `delta`)

`relevance` tells you what changed; this owns the bookkeeping that gets it to a subscriber and keeps it there. One `DeltaBaseline` per subscriber. The client's half is `plaza_client_utils::DeltaMirror`, and the two are keyed by the same `SlotKey` and checked by the same `SetDigest`.

### Enum `RecoveryPolicy`

*   **`Naive`**: diff against the last state *sent*. Simple, and wrong the moment a packet is lost: whatever it carried is never mentioned again, so the subscriber is permanently short of it while every readout looks healthy. Selectable because the failure is worth being able to demonstrate; a block that only knew how to be correct would make the failure it prevents invisible.
*   **`AckRecovery`** (the default): diff against the newest state the subscriber has *acknowledged*, so a lost packet's contents are re-derived by the next difference. Also enables the digest drift check and the stale-baseline rebuild.

### Struct `DeltaPlan`

What to send this round. `full_baseline: bool` (the subscriber must clear its mirror first, because what follows is the whole visible set), `baseline_seq: Option<u64>` (the sequence the differences were computed against, worth putting on the wire), `entered: Vec<u64>`, `left: Vec<u64>`.

### Struct `DeltaBaseline`

*   **`new(history)`**: how many recent sent states to retain. An acknowledgement older than the window forces a full rebuild rather than a wrong diff, so size it past the worst round trip you intend to survive.
*   **`with_policy(policy)`** / **`set_policy(policy)`**, **`reset()`** (a fresh subscriber took this slot: the next plan is a full baseline).
*   **`plan(&mut self, current: &BTreeSet<u64>, seq) -> DeltaPlan`**: what to send, given the visible set now.
*   **`observe_ack(&mut self, newest, mask, digest)`**: the return path. Passing the digest of an empty set disables the drift check.
*   **`request_full_baseline(&mut self)`**, **`full_rebuilds()`**, **`unacked()`**, **`acked_seq()`**.

**Four details in here are load-bearing, and three were wrong in the first working version.** They are the content of the block:

- **The baseline is the newest *contiguous* acknowledgement, not the newest bit set.** A bitmask answers "what arrived", which is what a *retransmitting* protocol wants: it names the holes to refill. A protocol that *re-derives* needs a state the subscriber provably reached, and receiving packet N+1 after losing N does not put it in the state N+1 implies. Taking the newest set bit hands the diff a state that never existed, and it made recovery statistically indistinguishable from no recovery at every loss rate. `observe_ack` calls `AckWindow::contiguous_base`, which exists because this walk was re-derived here once, and wrongly.
- **The keys must carry generations.** A retraction re-derived after a slot has been recycled names the slot's *current* occupant, so the subscriber's lookup misses and the entity it actually holds is never mentioned again. Keying the baseline the way the digest is keyed makes recovery and verification answer the same question. This is why `SlotKey` exists rather than a bare index, and why the key space is documented as `(index << 16) | generation`.
- **The two halves of the diff need baselines built by opposite operations.** What to *send* must assume the least the subscriber holds: the acknowledged state **intersected** with every state sent since, because anything a later packet may have retracted might already be gone. What to *retract* must assume the most: the acknowledged state **unioned** with everything announced since, because an entity that entered and left inside that gap appears in neither the baseline nor the current set and a single diff never mentions it. Getting the union right and leaving the other half as the raw acknowledged state trades one silent failure for its mirror image, and did: the corpses became omissions.
- **Cold start is a decision, not an accident.** Recovery diffs against the acknowledged state, and there is no acknowledged state before the first acknowledgement, so a mechanism defined in terms of accumulated state has undefined behaviour before that state exists, and the accidental fallback here was silently the naive behaviour it exists to replace.

**Measured**, `horde_playground`, 3000 enemies over a real socket:

| loss | policy | mismatches | phantoms | missing | KiB/s | err px |
|---|---|---|---|---|---|---|
| 0% | either | 0 | 4 | 107 | 33.0 | 7.2 |
| 10% | last sent | 227 | 364 | 174 | 33.0 | 19.1 |
| | last acked | 70 | 4 | 107 | 40.1 | 6.7 |
| 25% | last sent | 234 | 188 | 313 | 33.0 | 44.7 |
| | last acked | 126 | 7 | 263 | 39.7 | 9.2 |

**A phantom count alone cannot verify any of this**, which is why the `missing` column is there. A starved mirror agrees with everything: zero corpses, digest agreement, and a flattering render error, because error only averages over entities both sides have. Every metric of the form "wrong things present" needs its "right things absent" twin, or a change is free to satisfy one by breaking the other. `missing` never reaches zero, because entities that just became relevant are still in flight; the number to watch is whether it stays at that floor.

## 8. Seats (module `seats`)

### Enum `Seating`

**`Fresh(usize)`**, **`Existing(usize)`**, **`Full`**, plus **`index() -> Option<usize>`** and **`is_fresh()`**.

Returning this rather than a bare index is the entire reason the type exists. `Fresh` versus `Existing` is exactly "reset this seat's accumulated state" versus "do not", and a server usually keeps advancing per-seat state whether or not anybody is in the seat, which is the correct thing to do and is what makes the trap reachable. Collapsing the two is the warm-arena join bug: a joiner taking a seat whose relevance baseline was already most of the world got a first frame that was a delta against a baseline it never received, almost nothing arrived as `entered`, and the stream had no path back. Making the caller match on freshness turns a thing you have to remember into a thing the compiler asks about.

### Struct `SeatTable<Key>`

*   **`new(capacity)`**, **`seat(key) -> Seating`**, **`unseat(&key) -> Option<usize>`**, **`seat_of(&key)`**.
*   **`occupants()`**, **`keys()`**, **`by_seat() -> HashMap<usize, Key>`**, **`occupied_count()`**, **`capacity()`**, **`is_full()`**, **`clear()`**.
*   **`reseat_all(capacity) -> Vec<(Key, usize)>`**: resize, returning everyone's new seat. For a world rebuilt under a changed configuration.

## 9. Rates (module `meter`)

### Struct `RateMeter`

A running total, a sample count, an elapsed clock, and the three questions over them.

*   **`new()`**, **`add(amount)`**, **`add_empty()`** (a sample carrying nothing, which still counts toward the mean), **`elapsed(elapsed_ms)`**, **`reset()`**.
*   **`total()`**, **`samples()`**, **`elapsed_ms()`**, **`per_sec()`**, **`mean()`**, **`share_of(&other) -> f64`**.

Trivial arithmetic, and every hand-rolled copy had to remember the same divide-by-zero guard, whose absence renders as `NaN` on the first frame and looks like the thing being measured is broken. `share_of` is here because **measuring a stream's share of the packet before optimising its encoding** is the check that would have saved three separate rounds of optimising the wrong thing: despawn ids were 1.2% of horde's traffic while position samples were 86.1%.
