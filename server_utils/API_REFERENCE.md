# API Reference: `plaza_server_utils`

## 1. Introduction & Core Concepts

`plaza_server_utils` is the **server half** of the pure netcode primitives, mirroring `plaza_client_utils`. It is pure logic with no async runtime, so a server simulation compiles to wasm alongside its clients, and it shares the client crate's `Interpolatable` and `ToF32` traits so one state type serves both sides.

It provides the building block lag compensation rests on (a rewind of past entity state), and the `relevance` building blocks for interest management (deciding what each client needs to see).

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

### Struct `SetDigest`

An order-independent checksum of a set of `u64` keys, for giving a delta-relevance stream a liveness check. A client that streams only *entered* and *left* events has no way to notice it has drifted out of step; sending a digest of what the server believes the client's set to be lets the client compare against its own and ask for a resync.

*   **`new()`**, **`from_keys(keys)`**, **`insert(key)`**, **`remove(key)`**, **`clear()`**, **`len() -> u32`**, **`is_empty() -> bool`**.
*   **`digest(&self) -> u64`**: the value to compare. Summation of a mixed hash per key, so it is order independent and maintainable incrementally: adding then removing the same key returns to the previous digest exactly.

`VisibilitySet::digest()` computes the same value over a bitset's membership.

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
