# API Reference: `plaza_server_utils`

`plaza_server_utils` is the server half of interest management and delta streaming: who is told about what, in what order, how often, and how the two ends prove they still agree.

## Contents

- [1. Core API](#1-core-api)
  - [Struct `HistoricalStateBuffer<EntityId, EntityStateSnapshot, ServerTime>`](#struct-historicalstatebufferentityid-entitystatesnapshot-servertime)
  - [Function `render_error_at`](#function-rendererrorat)
  - [Struct `RenderError`](#struct-rendererror)
  - [Struct `TimedState<ServerTime, State>`](#struct-timedstateservertime-state)
  - [Shared traits](#shared-traits)
- [2. Sizing the buffer](#2-sizing-the-buffer)
- [3. Relevance (module `relevance`)](#3-relevance-module-relevance)
  - [Module `relevance::morton`](#module-relevancemorton)
  - [Struct `GridQuantizer`](#struct-gridquantizer)
  - [Struct `CellSpace`](#struct-cellspace)
  - [Struct `CellTable<T>`](#struct-celltablet)
  - [Trait `Clearable`](#trait-clearable)
  - [Struct `SpatialGrid<Id: Copy>`](#struct-spatialgridid-copy)
  - [Struct `TierBoundary`](#struct-tierboundary)
  - [Struct `VisibilitySet`](#struct-visibilityset)
  - [Struct `SetDigest` (re-exported)](#struct-setdigest-re-exported)
  - [Module `field`: `Field`, `Strategy`, `Query`](#module-field-field-strategy-query)
- [4. Priority (module `priority`)](#4-priority-module-priority)
  - [Struct `PriorityAccumulator`](#struct-priorityaccumulator)
- [5. At rest (module `rest`)](#5-at-rest-module-rest)
  - [Struct `RestDetector`](#struct-restdetector)
- [6. Subscription (module `subscription`)](#6-subscription-module-subscription)
  - [Enum `Because`](#enum-because)
  - [Struct `Subscriptions<K>`](#struct-subscriptionsk)
  - [Struct `Audience<K>`](#struct-audiencek)
- [7. Aggregation (module `aggregate`)](#7-aggregation-module-aggregate)
  - [Struct `WeightedPoint`](#struct-weightedpoint)
  - [Struct `AggregateTree`](#struct-aggregatetree)
  - [Struct `Summary`](#struct-summary)
  - [Choosing `theta`](#choosing-theta)
- [8. Delta streaming (module `delta`)](#8-delta-streaming-module-delta)
  - [The two failure modes this exists to prevent](#the-two-failure-modes-this-exists-to-prevent)
  - [The digest is also the resume story](#the-digest-is-also-the-resume-story)
  - [Two invariants, both load bearing](#two-invariants-both-load-bearing)
  - [Enum `RecoveryPolicy`](#enum-recoverypolicy)
  - [Struct `DeltaPlan`](#struct-deltaplan)
  - [Struct `DeltaBaseline`](#struct-deltabaseline)
- [9. What each viewer was told (module `told`)](#9-what-each-viewer-was-told-module-told)
  - [Struct `Told<Viewer, K, V>`](#struct-toldviewer-k-v)
- [10. Input scheduling (module `input_schedule`)](#10-input-scheduling-module-inputschedule)
  - [Struct `InputWindow`](#struct-inputwindow)
  - [Enum `Submission`](#enum-submission)
  - [Struct `InputSchedule<Input>`](#struct-inputscheduleinput)
- [11. Seats (module `seats`)](#11-seats-module-seats)
  - [Enum `Seating`](#enum-seating)
  - [Struct `SeatTable<Key>`](#struct-seattablekey)
  - [Struct `SeatSlots<Key>`](#struct-seatslotskey)
  - [Struct `RankedQueue<Key>`](#struct-rankedqueuekey)
  - [Struct `Roster<Key>`](#struct-rosterkey)
  - [Struct `Crew<Key>`](#struct-crewkey)
- [12. Rates (module `meter`)](#12-rates-module-meter)
- [13. One-shot ops (module `oneshot`)](#13-one-shot-ops-module-oneshot)
  - [Struct `Pending<K, Op>`](#struct-pendingk-op)
- [14. Error Handling](#14-error-handling)

## 1. Core API

### Struct `HistoricalStateBuffer<EntityId, EntityStateSnapshot, ServerTime>`

A rolling per-entity history of states, queryable by time, so the server can reconstruct the world as a client saw it.

Bounds: `EntityId: Eq + Hash + Clone + Debug`; `EntityStateSnapshot: Clone + Debug` (and `Interpolatable<ServerTime>` when queried between two times); `ServerTime: Copy + Debug + Default + PartialOrd + Ord + Sub<Output = ServerTime> + ToF32`. Because `ToF32` covers `u64` and `Duration`, millisecond or tick time works directly, with no custom time type.

*   **`new(max_snapshots_per_entity: usize) -> Self`**: keeps at most this many states per entity. **Panics if 0.**
*   **`record_state(&mut self, entity_id, server_time, state)`**: record one entity's state for a tick. A state not newer than the last recorded is ignored (with a `tracing` warning), so history stays strictly increasing.
*   **`get_state_at_or_before(&self, entity_id, target_server_time) -> Option<EntityStateSnapshot>`**: the rewind. Interpolates between the two recorded states bracketing the target; clamps to the oldest or newest when the target is outside the retained range; `None` if the entity is unknown. Requires `EntityStateSnapshot: Interpolatable<ServerTime>`.
*   **`remove_entity_history(&mut self, entity_id)`**, **`clear_all_history(&mut self)`**

### Function `render_error_at`

```rust
pub fn render_error_at<Id, State, Time, D>(
  history: &HistoricalStateBuffer<Id, State, Time>,
  at: Time,
  drawn: impl IntoIterator<Item = (Id, State)>,
  distance: D,
) -> RenderError
where D: Fn(&State, &State) -> f32
```

How wrong a client's screen was, asked **at the instant it was drawing**. Reads truth from the history buffer, which is usually already present because it is the same one a rewind uses.

`at` is a required argument, deliberately: comparing against the present charges a client for a render delay it is taking on purpose, so the honest form is the one that falls out of calling this and the dishonest one has to be typed. `distance` is supplied by the caller because this crate can assume no metric on a state type, the same reason `Correction` hands back two states rather than a scalar. Entities absent from the history are skipped rather than scored zero.

This is a **host or harness** measurement and cannot be anything else: it needs truth, and a joiner never has truth.

### Struct `RenderError`

An accumulation: `observe(f32)`, `merge(&RenderError)`, `mean() -> f32`, `worst() -> f32`, `samples() -> u32`, `is_empty() -> bool`. Holds the sum rather than the mean so results from several clients or frames fold together without weighting a client that can see two entities the same as one that can see two hundred. Non-finite distances are ignored rather than poisoning the mean.

### Struct `TimedState<ServerTime, State>`

One recorded entry: `time`, `state`. Exposed for callers that inspect raw history.

### Shared traits

`Interpolatable` and `ToF32` are re-exported from `plaza_client_utils`. Implement `Interpolatable<ServerTime>` on your snapshot type to allow interpolated rewinds; it is the same trait a client implements for its `SnapshotBuffer`, so a single impl covers both.

## 2. Sizing the buffer

`max_snapshots_per_entity` should cover the largest rewind you will ask for: roughly `(max expected client latency + interpolation delay) / server tick interval`, plus a margin. Too small and old-enough shots clamp to the oldest retained state instead of rewinding accurately.

## 3. Relevance (module `relevance`)

Interest management: sending each client only the entities near it, and the change since last tick. Building blocks, not a policy, the cell size, the relevance rule, and the wire encoding stay yours.

### Module `relevance::morton`

Z-order curve encoding, integer coordinates interleaved into one locality-preserving key.

*   **`encode_2d(x: u32, y: u32) -> u64`** / **`decode_2d(u64) -> (u32, u32)`**: full 32-bit coordinates.
*   **`encode_3d(x, y, z: u32) -> u64`** / **`decode_3d(u64) -> (u32, u32, u32)`**: each coordinate uses its low **21 bits** (three axes share 63 bits).

### Struct `GridQuantizer`

Maps continuous world coordinates onto a uniform integer grid.

*   **`new(origin: (f32, f32), cell_size: f32)`**: `origin` is the world's minimum corner (points at or below it clamp to cell 0); `cell_size` near a viewer's relevance radius keeps a view a few cells wide. **Panics if `cell_size` is not positive.**
*   **`cell(x, y) -> (u32, u32)`**, **`key(x, y) -> u64`** (Morton key of the cell), **`cells_for_radius(radius) -> u32`**, **`cell_size() -> f32`**.
*   **`corner(&self, cx, cy) -> (f32, f32)`**: the world-space minimum corner of a cell. What lets a payload that knows which cell it describes quantise over one cell's width instead of the world's, several bits an axis at the same step.
*   **`cells_in_radius(&self, x, y, radius) -> impl Iterator<Item = (u32, u32)>`**: the region walk yielding cell coordinates rather than Morton codes, for a bounded world indexing a flat `Vec`.
*   **`keys_in_radius(&self, x, y, radius) -> impl Iterator<Item = u64>`**: the Morton keys of every cell overlapping the square of half-width `radius`, row by row. The region walk itself, for anything keyed by cell rather than by entity: one payload per occupied cell, a viewer's cell subscription set, diffing that set as the viewer moves.

### Struct `CellSpace`

A `GridQuantizer` over a world with **known bounds**, so every cell has a dense integer index and anything keyed by cell can live in a flat `Vec`. A Morton key is right for an unbounded or sparse world, because only a hash map can hold it; a bounded world pays an array index instead of a hash. Publishing one payload per cell and handing each viewer the cells its view touches does roughly `viewers x cells-per-view` lookups a tick, measured at 1.39x of a whole tick in one consumer and 8x on the path that also builds a per-cell recipient list.

Deliberately not a container: it is the *addressing scheme*, and the containers are whatever the caller keys by place. `CellTable<Vec<Id>>` is a dense spatial grid, `CellTable<Option<Bytes>>` is one published payload per cell, `CellTable<Vec<ClientId>>` is the inverse index naming who listens to each cell. All three occur in one consumer, which is why this is the primitive rather than any of them.

*   **`new(GridQuantizer, extent: f32)`**: a space covering `[origin, origin + extent]` on both axes. Panics if `extent` is not positive.
*   **`side() -> u32`**, **`len() -> usize`** (cells in the space, the length a keyed `Vec` needs), **`is_empty()`**, **`quantizer() -> &GridQuantizer`**.
*   **`index_at(cx, cy) -> usize`**, **`index_of(x, y) -> usize`**: the dense index of a cell. **Clamped**, matching `GridQuantizer::cell`, so a point outside the bounds is filed at the edge rather than rejected: a space smaller than its world piles the outside into its border cells. Size it to the world.
*   **`cell_at(index) -> (u32, u32)`**, **`corner(index) -> (f32, f32)`**.
*   **`indices_in_radius(&self, x, y, radius) -> impl Iterator<Item = usize>`**: the dense indices of every cell overlapping the region, clamped at the bounds. Cell-granular, so it reaches past the radius at the corners.

### Struct `CellTable<T>`

A `Vec` keyed by `CellSpace`, holding one `T` per cell. The container half of the pair; reused rather than reallocated so a per-tick rebuild does not churn the heap.

*   **`new(CellSpace)`** (requires `T: Default`), **`reset()`** (every cell back to `T::default()`).
*   **`get(index)`**, **`get_mut(index)`**, **`at(x, y)`**, **`at_mut(x, y)`**, **`iter()`**, **`iter_mut()`**, **`len()`**, **`is_empty()`**, **`space() -> &CellSpace`**.
*   With `T: Clearable`: **`clear_each()`** empties every cell while keeping each one's own allocation, and **`occupied() -> impl Iterator<Item = (usize, &T)>`** yields only the non-empty ones.

### Trait `Clearable`

Emptied in place rather than replaced, so a per-tick rebuild keeps its allocations. **`clear_in_place()`**, **`is_empty_slot()`**. Implemented for `Vec<T>` and `Option<T>`.

### Struct `SpatialGrid<Id: Copy>`

Buckets entity ids into cells for range queries. Rebuild each tick.

*   **`new(GridQuantizer)`**, **`clear(&mut self)`** (empties buckets, keeps capacity), **`insert(&mut self, id, x, y)`**.
*   **`query_radius(&self, x, y, radius, out: &mut Vec<Id>)`**: appends every id in the cells overlapping the square of half-width `radius`, a cell-granular *superset*, so apply an exact distance test after. `out` is not cleared, so reuse one `Vec` to avoid per-query allocation. Equivalent to `keys_in_radius` followed by `members` per key.
*   **`members(&self, key: u64) -> &[Id]`**: the ids in one cell; empty for a cell nothing occupies.
*   **`occupied(&self) -> impl Iterator<Item = (u64, &[Id])>`**: every occupied cell and its ids, in no particular order. The grid seen cell-first instead of viewer-first: build one payload per occupied cell, then hand each viewer the payloads for `keys_in_radius` of its position.
*   **`quantizer(&self) -> &GridQuantizer`**.

**Two axes, and in three dimensions that over-returns rather than missing.** The exact distance test the previous point asks for is what makes it correct on a plane; in open volume a query on `(x, z)` answers with a disc where a sphere was wanted, so nobody is missed and a great deal is sent that should not have been. `spacemo` prices that at 7.1x the bandwidth per client, with the game looking entirely right. Filter the returned set on `|dy|` as well: exact, at the same query cost, since it touches the same cells and examines the same candidates. Most 3D games never need more, because a landscape is locally 2.5D; a genuinely volumetric grid sends exactly what the filter sends and has to earn its place on query cost alone.

### Struct `TierBoundary`

A membership boundary with hysteresis: it takes less distance to stay in than to get in. Any threshold that switches what the wire carries (a near tier at full precision and rate against a far tier quantised and slow, a relevance radius, an aggro range) flaps when an entity loiters on it: membership changes every few frames, and every change is a precision or rate step the receiver has to absorb, visible as a peer marker twitching between two qualities of motion. The cure is two radii with a gap, judged against where the entity stood last time. The memory is the caller's own previous membership set, which it already keeps for diffing, so this costs no state: pass `was_inside` from it.

*   **`const fn new(enter: f32, leave: f32)`**: `enter` is the radius that admits a newcomer; `leave`, which must not be smaller, is the one past which a member is dropped. The gap between them is the loitering band, sized to the wobble the boundary actually sees (a step or two of movement per update interval). Debug-asserts `enter <= leave` and clamps `leave` up to `enter` otherwise.
*   **`admits(&self, was_inside: bool, distance: f32) -> bool`**: whether an entity at `distance` is a member this round, given whether it was one last round.

### Struct `VisibilitySet`

A dense bitset of which entities (by `u32` index) are visible to one client, with a fast diff.

*   **`new()`** / **`with_capacity(max_index: u32)`**, **`clear`**, **`insert(index)`**, **`contains(index) -> bool`**, **`count() -> usize`**, **`iter()`** (ascending).
*   **`remove(index)`**: marks an entity not visible. Useful when an entity is destroyed rather than merely leaving range: clear it explicitly so the next diff treats a *reused* slot as a fresh arrival instead of silently carrying the old occupant's membership forward.
*   **`diff(&self, previous, entered: &mut Vec<u32>, left: &mut Vec<u32>)`**: appends newly-visible indices to `entered` (`self & !previous`) and no-longer-visible to `left` (`previous & !self`), word at a time, the spawn/despawn stream. Vectors are not cleared, so reuse them.
*   **`digest() -> u64`**: an order-independent digest of the visible indices, the same fold as `SetDigest` below.

For sparse handles (`Uuid`), map to dense indices first, or diff two sorted lists; this is the dense-index fast path. See the [`relevance_demo`](examples/relevance_demo.rs) example.

### Struct `SetDigest` (re-exported)

An order-independent checksum of a set of `u64` keys, for giving a delta-relevance stream a liveness check. A client that streams only *entered* and *left* events has no way to notice it has drifted out of step; sending a digest of what the server believes the client's set to be lets the client compare against its own and ask for a resync.

*   **`new()`**, **`from_keys(keys)`**, **`insert(key)`**, **`remove(key)`**, **`clear()`**, **`len() -> u32`**, **`is_empty() -> bool`**.
*   **`digest(&self) -> u64`**: the value to compare. Summation of a mixed hash per key, so it is order independent and maintainable incrementally: adding then removing the same key returns to the previous digest exactly.

`VisibilitySet::digest()` computes the same value over a bitset's membership. Defined in `plaza_client_utils::digest` and re-exported here, so both ends of a stream fold the same arithmetic; its full documentation is in that crate's reference.

**An integrity check that only detects is half a tool.** A digest says *that* a mirror is wrong and never *how*, and one horde bug it caught still took days, because the counter was unactionable. Pair it with an opt-in mode that ships the ground truth it is a checksum of (`DeltaMirror::divergence_from` is the consumer side); the cost is a switch and some bandwidth while it is on. In that case the diagnosis was immediate: every missing key was generation zero and spread evenly across the whole slot range, which is the signature of a client that joined an arena already in progress, not of a drift.

Attach the comparison to the **report**, not to the state change. Running it only when an acknowledgement advances the frontier skips the one case it is for: a client re-acknowledging the same sequence still tells you what it holds, and a mirror that loses something *without* losing a packet reports it exactly then.

### Module `field`: `Field`, `Strategy`, `Query`

The third axis, priced instead of assumed. A flat grid indexed on `(x, z)` returns the **disc** containing the sphere it was asked for, so nothing is missed and altitude is paid for in false positives: bandwidth quietly spent telling stacked strangers about each other. `Field` is one uniform grid with a `Strategy` mode (**`Flat`**, **`FlatBand`** which filters the flat answer on `|dy|`, **`Volume`**), so a measurement changes one enum and nothing else.

*   **`Field::new(cell, strategy)`**, **`insert(id, at: Vec3)`**, **`rebuild(&[Vec3])`**, **`clear`**, **`strategy()`**, **`cell()`**.
*   **`query(&self, at, radius, out, truth) -> Query`**: everyone within `radius`, by the strategy. `truth` is the brute-force answer (from **`field::truth`**) so one sphere test scores every strategy; serving paths pass `&[]`.
*   **`Query`**: what the query *did*: `returned`, `examined` (the candidates pulled from cells and tested, which a result set cannot show), `cells`, `false_positives`, `missed` (any value above zero is a bug rather than a trade).

Measured both ways in the examples it came from: spacemo's open volume had the flat disc funding 7.1x the bandwidth of the same query with the band filter, and gow_3d's stacked fliers had the band examining 2.7x what a volume grid did. Filter on height when things are spread out; index the third axis when they stack.

## 4. Priority (module `priority`)

[Relevance](#3-relevance-module-relevance) answers *who can see what*, which is a yes or no. It does not answer what follows: a hundred entities are relevant, the budget holds twenty, so which twenty go this tick? Sending the first twenty by id starves the tail forever, and so does sending the nearest twenty. That is what turns a bandwidth budget into a bandwidth *outcome*.

### Struct `PriorityAccumulator`

Per-entity priority that survives the ticks an entity is not sent on. Indexed densely, so a [`SlotKey`](../client_utils/API_REFERENCE.md#struct-slotkey) is already the index.

*   **`new(entities)`**, **`resize(entities)`**, **`len()`**, **`is_empty()`**, **`clear()`**.
*   **`bump(&mut self, index, priority: f32)`**: adds this tick's priority. An index past the end grows the space rather than panicking, since an allocator handing out a fresh slot is ordinary.
*   **`fill(&mut self, budget: usize, cost: impl Fn(usize) -> usize, out: &mut Vec<usize>)`**: fills `budget` with the highest scorers, clearing `out` first and returning indices highest-priority first. **Chosen entities reset to zero; skipped ones keep what they had**, which is what stops anything starving. The walk continues past an entity that does not fit rather than stopping, so one large entity near the front cannot leave the rest of the packet empty; its priority keeps climbing until it wins outright. Ties break by index, so a server and a replay of it choose alike.
*   **`score(index) -> f32`**, **`forget(index)`**: drop an entity to zero without sending it, for a despawn or for something that has gone irrelevant and should not return holding a hoard.

Entities at zero or below are never chosen, so a negative score is how you say "not this one" without removing it. The per-tick priority is yours: distance, ownership, whether it is [at rest](#5-at-rest-module-rest), how long since it changed.

## 5. At rest (module `rest`)

In a settled scene most things are not moving, and saying so costs one bit against the thirty-three a velocity costs.

### Struct `RestDetector`

Knowing is the part worth a type. One quiet tick means nothing: a body at the top of its arc has zero velocity and is about to fall, and a body on the floor jitters by an epsilon forever. So rest is a **run** of quiet ticks, while waking is immediate, because being slow to notice motion is visible and being slow to notice stillness only costs bandwidth.

*   **`new(threshold)` / `with_capacity(entities, threshold)`**: `threshold` is the run of quiet ticks that counts as rest.
*   **`observe(&mut self, index, moving: bool)`**: one tick of evidence. What counts as moving stays yours. A solver already knows, but check the granularity before you take it: rapier sleeps an **island**, meaning every body in a chain of contacts, so one body still jostling in a heap reports the whole heap as moving. Prefer a per-body test, a speed against an epsilon, and let this type supply the run-of-quiet-ticks part.
*   **`at_rest(index) -> bool`**, **`ticks_still(index) -> u32`** (for scaling priority smoothly instead of switching on a threshold), **`wake(index)`** for a teleport or respawn that no velocity test would catch.
*   **`resize`**, **`len`**, **`is_empty`**.

## 6. Subscription (module `subscription`)

[Relevance](#3-relevance-module-relevance) answers *who is near me*, over a set that changes every tick. This answers *who have I chosen to care about, wherever they are*, over a set that changes every few hours. Neither expresses the other: a party as a relevance radius is an infinite radius, and a grid query as a subscription is resubscribing everybody every tick.

### Enum `Because`

Why an entity is in an audience: `Near`, `Subscribed`, or `Either`. Helpers **`is_near()`** and **`is_subscribed()`**.

The distinction belongs on the wire, but **this type does not**, and the copy in your protocol is deliberate rather than an oversight. This crate carries no serde, and the coupling is worse than the duplication: a protocol version is a hash of the types on the wire, so a wire type owned by a library means upgrading the library silently re-versions every application using it, and a patch release disconnects clients. Spell the three variants again in your own protocol under a name you chose.

What has to reach the client either way is the distinction itself. The two are different promises with different lifetimes, and a client that cannot tell them apart cannot draw a party frame for somebody out of view: absence from a later frame means "walked away" for `Near` and "left the world" for `Subscribed`.

### Struct `Subscriptions<K>`

A directed subscription set with a reverse index. Directed because a spectator following a player does not make the player follow the spectator.

*   **`new(limit)`** / **`default()`**: `limit` caps outgoing subscriptions per key (`usize::MAX` by default). Bounded on purpose: a radius is limited by how many entities fit in it, and a subscription is limited by nothing unless something says so.
*   **`subscribe(who, to) -> bool`**: one direction. False if it would pass the limit, or if `who == to`.
*   **`pair(a, b) -> bool`**: both directions, **all or nothing**. A half-applied symmetric relationship is worse than a refused one, since one side draws a party frame and the other does not.
*   **`group(a, b) -> bool`**: merges the two symmetric groups into one, everyone subscribed to everyone. Refused whole if the result would pass the limit, changing nothing. This is the party-joins-party operation, and the one that is easy to get wrong by adding one person to one side.
*   **`group_of(&key) -> Vec<K>`**: the symmetric group holding `key`, `key` included; a key with no subscriptions is a group of one. One-sided subscriptions are excluded, or following somebody would drag them into your party.
*   **`leave_group(&key) -> Vec<K>`**: takes the key out of its symmetric group and returns who was told, leaving directed subscriptions (a spectator watching you) alone. **Leaving a party is not the same event as leaving the world**, and an application with only `remove` ends up spelling this out of `unsubscribe` calls in both directions and getting the dissolve wrong. A group of one left behind is dissolved, since it costs a lookup for ever to answer a question nobody is asking.
*   **`remove(&key) -> Vec<K>`**: drops the key both directions and **returns everyone who was subscribed to it**. Those are the clients whose interface still has an entry for something no longer present, and finding them by scanning every subscriber is the alternative. Call it on departure: a subscription that outlives its subject is a health bar that keeps updating for somebody who left.
*   **`unsubscribe(&who, &from)`**, **`of(&key)`**, **`watchers(&key)`**, **`count_of(&key)`**, **`is_subscribed(&who, &to)`**, **`subscribers()`**, **`clear()`**.

### Struct `Audience<K>`

*   **`of(near: &[K], subs, viewer) -> Audience<K>`**: unions a spatial answer with the viewer's subscriptions. `near` may be in any order; `entries` comes back sorted, so a diff against the previous tick means something.
*   **`entries: Vec<(K, Because)>`**, **`near: usize`**, **`added: usize`**.
*   **`added`** is the measurement worth reporting: what the second channel cost, which is only the members distance missed. A subscriber standing beside the viewer costs nothing extra, and in a game where parties stay together that is most of the time.
*   **`keys()`**, **`visible()`** (near only, the ones there is a body to draw), **`why(&key)`**, **`len()`**, **`is_empty()`**.

## 7. Aggregation (module `aggregate`)

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

## 8. Delta streaming (module `delta`)

`relevance` tells you what changed; this owns the bookkeeping that gets it to a subscriber and keeps it there. One `DeltaBaseline` per subscriber. It is deliberately set-theoretic and knows nothing about what a key means: keys are `u64`, entering and leaving are the only events, and mapping a key back to a spawn payload or a despawn reason is the application's job. The client's half is `plaza_client_utils::DeltaMirror`, and the two are keyed by the same `SlotKey` and checked by the same `SetDigest`.

### The two failure modes this exists to prevent

Both were shipped, in a real example, and both took days to find because the symptom was far from the cause.

*   **A subscriber that joins mid-session.** Servers usually track relevance for every slot from startup, occupied or not. When a real client finally arrives, that slot's baseline already describes most of the world, so the client's first packet is a difference against a state it never received: it is sent almost nothing and converges only as pieces of the world happen to become newly relevant. Call `reset` when a subscriber takes the slot and the first packet is a full baseline instead.
*   **A mirror that diverges for any reason at all.** Once the server believes a subscriber holds a key, that key is only ever sent as an update, and an update for something you do not have is discarded. There is no path back, so a single divergence is permanent no matter how much traffic follows. Carrying the subscriber's own digest on its acknowledgement (see `observe_ack`) lets the server notice that the two disagree and rebuild from nothing.

### The digest is also the resume story

The drift check has a second reading that matters as much as the first: it is a **permission the client side builds on**. A client may discard any stretch of the stream unread (a backgrounded tab's backlog, most commonly) provided it also drops its mirror, because its next acknowledgement then carries the digest of nothing and this type answers with a full baseline. No resync-request message exists anywhere, and none is needed: dropping the mirror is the request. The client half of that bargain is `plaza_client_utils`' playout buffer and `plaza_ws`' backlog trim; the server half is `DeltaBaseline` plus `with_flow`, which stops streaming full-rate full baselines to a subscriber that has provably stopped reading.

### Two invariants, both load bearing

*   **The key must be the key the digest hashes.** If the application digests `(index, generation)` pairs, that is what it must hand to this type, or the drift check compares two unrelated numbers and either never fires or always does. Encoding both into one `u64` is the usual answer; this is why `SlotKey` exists rather than a bare index, and why the key space is documented as `(index << 16) | generation`. A retraction re-derived after a slot has been recycled would otherwise name the slot's *current* occupant, and the entity the subscriber actually holds is never mentioned again.
*   **Acknowledge states, not packets.** The frontier this walks is the newest *contiguous* acknowledged sequence, not the newest bit set. Receiving packet N+1 after losing N does not put a subscriber in the state N+1 implies, because whatever N announced and N+1 had no reason to repeat is simply gone. Taking the newest set bit hands the diff a state that never existed, and it made recovery statistically indistinguishable from no recovery at every loss rate. That walk is `AckWindow::contiguous_base` (from `plaza_client_utils::ack`), not something re-derived here: it was re-derived here once, and wrongly, which is how the primitive came to exist.

### Enum `RecoveryPolicy`

*   **`Naive`**: diff against the last state *sent*. Simple, and wrong the moment a packet is lost: whatever it carried is never mentioned again, so the subscriber is permanently short of it while every readout looks healthy. Selectable because the failure is worth being able to demonstrate; a block that only knew how to be correct would make the failure it prevents invisible.
*   **`AckRecovery`** (the default): diff against the newest state the subscriber has *acknowledged*, so a lost packet's contents are re-derived by the next difference. Also enables the digest drift check and the stale-baseline rebuild.

### Struct `DeltaPlan`

What to send this round. `full_baseline: bool` (the subscriber must clear its mirror first, because what follows is the whole visible set; set when a subscriber is new, when its acknowledged baseline has aged out of history, or when its digest proved the mirror had drifted), `baseline_seq: Option<u64>` (the sequence the differences were computed against, or `None` for a difference from nothing, worth putting on the wire), `entered: Vec<u64>` (keys the subscriber does not hold and should), `left: Vec<u64>` (keys the subscriber may hold and should not).

### Struct `DeltaBaseline`

*   **`new(history: usize)`**: how many recent sent states to retain (clamped to at least 1). Cover the most packets that can be in flight plus the acknowledgement's return trip; a state older than the window cannot be recovered by re-derivation and forces a full rebuild rather than a wrong diff.
*   **`with_policy(policy) -> Self`** / **`set_policy(&mut self, policy)`**: selects the reliability policy; `set_policy` on a live subscriber resets, forgetting any state the new policy cannot honour.
*   **`with_flow(stalled_after: u64, keepalive_every: u64) -> Self`**: enables flow control, in the application's own clock units: a subscriber silent for `stalled_after` is throttled to one send every `keepalive_every` until it acknowledges again. This belongs to the delta stream and not to the transport: once a subscriber's acknowledged baseline ages out of history, **every** plan for it is a full baseline, so a reader that has stopped reading (a background browser tab: its socket keeps receiving while its frame loop does not run) is streamed the whole visible set at full rate, into a buffer it must pay for all at once on resume; measured in the horde example that was tens of megabytes a minute and a several-second freeze on refocus. The keepalive keeps the stream discoverable: the resumed client applies it, acknowledges it, and full rate resumes on the next round. Choose `stalled_after` to match the client side's own discontinuity threshold, and keep it several times the acknowledgement interval so ordinary loss cannot trip it.
*   **`stalled(&self, now: u64) -> bool`**: whether the subscriber has stopped acknowledging. Always `false` without `with_flow`, and during a fresh subscriber's grace period (silence is measured from the first send decision, so a joiner is not born stalled).
*   **`should_send(&mut self, now: u64) -> bool`**: whether to build and send a packet this round. `true` for a live subscriber; for a stalled one, `true` once per `keepalive_every` and `false` otherwise, in which case skip the `plan` call entirely: not planning also leaves the sent history exactly where the last acknowledgement can still name it.
*   **`reset(&mut self)`**: forgets everything and makes the next plan a full baseline. **Call this when a subscriber takes the slot.** A slot nobody has ever acknowledged is covered anyway, because an unacknowledged baseline is treated as unknown and sent in full; the case that still needs this call is a **reused** slot, where the previous occupant's acknowledged state is a perfectly plausible baseline for a subscriber that has never seen any of it. Also restarts the flow-control grace period: the new occupant's silence starts now.
*   **`plan(&mut self, current: &BTreeSet<u64>, seq: u64) -> DeltaPlan`**: what to send, given the visible set now. `seq` numbers this packet and must be what the subscriber acknowledges.
*   **`observe_ack(&mut self, newest: u64, mask: u64, digest: u64)`**: the return path. A no-op under `Naive`. `digest` is the subscriber's own `SetDigest` over the keys it is actually holding; when it disagrees with the digest of the state the server believes it reached, the next plan is a full rebuild. Passing the digest of an empty set disables the drift check. The digest is only compared when the frontier has reached the newest packet the subscriber reports, so a disagreement means "wrong" rather than "further ahead", and ordinary packet loss does not trigger rebuilds.
*   **`observe_ack_at(&mut self, newest: u64, mask: u64, digest: u64, now: u64)`**: `observe_ack`, plus the acknowledgement's arrival time for flow control. Use this form whenever `with_flow` is on; the timestamp records under **either** policy, because liveness is a property of the subscriber, not of the recovery arithmetic. An ack from a subscriber currently stalled is **the resume signal**, and it starts a fresh epoch (a `reset`) instead of being folded in: its window spans the silence, and the keepalives inside the silence are sparse in the sequence space, so folding it in pins the baseline at the first keepalive and every plan after resume diffs against a state as old as the stall, until staleness notices a second time (measured as ~25 consecutive full baselines over 1.5 s). Resetting makes the next plan one full baseline in a clean epoch, and the stream is deltas again after a single round trip.
*   **`request_full_baseline(&mut self)`**: forces the next plan to be a full baseline; the application's own escape hatch for a divergence it detected by some other means.
*   **`full_rebuilds() -> u64`**: how many times this subscriber has needed a full rebuild; the cost of recovery, and the number that says whether the history window is long enough for the loss and latency actually being seen.
*   **`unacked() -> usize`**: packets sent whose fate is still unknown, the true in-flight count whether the last event was a plan or an acknowledgement.
*   **`acked_seq() -> Option<u64>`**: the newest sequence the subscriber has acknowledged reaching the state of.

**Three more details in here are load-bearing**, and each was wrong in a shipped version:

- **The two halves of the diff need baselines built by opposite operations.** What to *send* must assume the least the subscriber holds: the acknowledged state **intersected** with every state sent since, because anything a later packet may have retracted might already be gone. What to *retract* must assume the most: the acknowledged state **unioned** with everything announced since, because an entity that entered and left inside that gap appears in neither the baseline nor the current set and a single diff never mentions it. Getting the union right and leaving the other half as the raw acknowledged state trades one silent failure for its mirror image, and did: the corpses became omissions.
- **Cold start is a decision, not an accident.** Recovery diffs against the acknowledged state, and there is no acknowledged state before the first acknowledgement, so under `AckRecovery` an unknown baseline means full sets for the first round trip, then incremental for the rest of the session. The accidental fallback here was silently the naive behaviour it exists to replace.
- **A rebuild starts a new epoch.** A rebuild drops the acknowledged baseline, the last-sent state, *and* the sent history, because the history is part of the old epoch twice over: a stale in-flight acknowledgement could name a pre-rebuild state as the baseline for a subscriber about to hold something else entirely, and after a subscriber restart its acknowledgement window has a gap at the stall boundary that `contiguous_base` can never cross, so with the old history in place the baseline stayed unknown and unknown means a full set every round until the gap aged out (measured at ~25 consecutive full baselines over 1.5 s). Cleared, the frontier restarts at the next packet and one acknowledgement round trip ends the full sets.

**Measured**, `horde_playground`, 3000 enemies over a real socket:

| loss | policy | mismatches | phantoms | missing | KiB/s | err px |
|---|---|---|---|---|---|---|
| 0% | either | 0 | 4 | 107 | 33.0 | 7.2 |
| 10% | last sent | 227 | 364 | 174 | 33.0 | 19.1 |
| | last acked | 70 | 4 | 107 | 40.1 | 6.7 |
| 25% | last sent | 234 | 188 | 313 | 33.0 | 44.7 |
| | last acked | 126 | 7 | 263 | 39.7 | 9.2 |

**A phantom count alone cannot verify any of this**, which is why the `missing` column is there. A starved mirror agrees with everything: zero corpses, digest agreement, and a flattering render error, because error only averages over entities both sides have. Every metric of the form "wrong things present" needs its "right things absent" twin, or a change is free to satisfy one by breaking the other. `missing` never reaches zero, because entities that just became relevant are still in flight; the number to watch is whether it stays at that floor.

## 9. What each viewer was told (module `told`)

### Struct `Told<Viewer, K, V>`

The state half of a change-only stream: a per-viewer memory of what was last said, diffed against what is now in their view, so a still world costs nothing to keep describing. `V = ()` is announce-once (a spawn said the tick it appears and never again until it leaves and returns).

*   **`new()`**, **`diff(viewer, current, say)`**: diffs `current` (the `(key, value)` pairs now true in this viewer's view) against the record, updates it, and calls `say(key, Some(&value))` for anything new or changed and `say(key, None)` for anything they hold that is gone from `current`. Whether a `None` goes on the wire is the caller's question, because "no longer true" and "no longer visible" arrive as the same absence and only the application knows which: a prop that reverted while still in view must be said, one that fell out of view is forgotten silently. Forgotten either way, so a return is a fresh introduction, which is also what lets a reused slot re-announce. The `None` keys arrive sorted, so a run produces the same wire twice.
*   **`forget(&viewer)`** (departure, or switching them to a repeat-everything stream), **`holdings(&viewer)`**, **`viewers()`**, **`clear()`**.

The prerequisite is a **stable state to diff against**: a value that jitters is said every tick and the saving evaporates. This is the state half of a private channel; the transcript half ("what just happened, said once to its one audience") is deliberately not a block, being a `Vec` drained into the frame, but a channel needs both, or "who is this for" becomes a field somebody forgets.

## 10. Input scheduling (module `input_schedule`)

Tick-addressed input buffering: the server side of "two players who pressed together execute together, whatever their ping". A client does not send "move now". It names **the server's own tick** its input is meant for, computed from its clock estimate plus the playout depth the server advertised, and the server buffers the input until that tick runs. Applied on arrival instead, the nearer player gets its ping difference as free head start, and anything decided by who-was-where-first is decided by the network.

Why a tick and not a timestamp: authority. A timestamp is the client naming a moment, which the server then has to judge plausible, and the judgement needs a shared clock whose error is exactly the slack a liar hides in. A tick is the client naming the server's own unit of time, which is either still open or is not.

**The window rejects; it never corrects.** An input for a tick already simulated is **dropped**, not shifted into the window. Correcting a backdated tick still executes the input, so a lag switch loses the lie and keeps the steering; dropping it means backdating costs the input, and it replaces "how much lying is tolerable" (no good answer) with "is this tick open" (a setting). The cost is real and lands on honest clients too: a link slower than the window loses inputs and rubber-bands, which is why the window is a parameter and `late` is a counter worth watching, not a curiosity.

**Which input model**, because there are two and this block is one of them. Mirroring the "which predictor" table in `plaza_client_utils`: how the server consumes input decides the machinery, and choosing wrong is silent.

| the server | use |
|---|---|
| executes inputs on the tick the client *named* (fairness across pings) | this block |
| applies inputs as they arrive, newest sequence wins | a sequence frontier and a bound, not this block |

The second model is what plain client-side prediction demos run (plaza's netcode and blackhole examples both do): commands carry a sequence number only, the server applies anything newer than the last applied and echoes the frontier back for reconciliation. It was deliberately not absorbed here, because tick addressing is the entire point of this type. What that model does share with this one is the obligation to bound its inbox.

**Derive the current tick, never count it.** Everything here takes `current` as a parameter because the schedule must not own a tick counter. A counter kept beside a clock has to be kept in step through every path that touches either, and a world rebuild is such a path: the example this was extracted from preserved the clock and reset the counter, after which every input any client aimed was hundreds of ticks stale and silently refused, permanently. Derive `current` from the simulation clock at the call site.

### Struct `InputWindow`

The accepting window, in ticks either side of the current one: public fields `max_late: u64` (how many ticks past its named tick an input is still accepted; it then executes on the next tick to run, and is counted late) and `max_early: u64` (how far ahead of the current tick an input may aim before it reads as parking inputs in the future). A live setting rather than a construction-time constant, because tuning it *is* the experiment: tighter is fairer and rubber-bands slow links sooner.

### Enum `Submission`

What `submit` decided: **`Scheduled`** (buffered for its tick), **`Late`** (buffered, but its tick has already passed inside the window: it executes on the next tick to run; a steady stream of these says the window is too tight for whoever is connected), **`TickClosed`** (named a tick already simulated and outside the window; dropped, because reopening a closed tick is exactly the rewrite of history a lag switch is trying to buy), **`TooFarAhead`** (named a tick too far ahead; dropped). Plus **`accepted(&self) -> bool`**: whether the input was buffered at all.

### Struct `InputSchedule<Input>`

One seat's buffered inputs and the counters that judge the window. Keep one per seat, like `DeltaBaseline`.

*   **`new()`**.
*   **`submit(&mut self, tick: u64, input: Input, current: u64, window: InputWindow) -> Submission`**: offers an input naming `tick`, judged against `current` (the tick the simulation is on now, derived from its clock) and the window. The execution tick is clamped to never fall behind anything already executed for this seat, so a client cannot reorder its own history by walking its named ticks backwards.
*   **`execute_due(&mut self, current: u64) -> Option<Input>`**: the input to apply on tick `current`, if any has come due: **level semantics**, for an input that is a held state (a direction, a throttle). Call once per simulation step, **per step and not per network frame**: consuming the queue once per frame collapses everything that arrived between two ticks onto whichever one happened to run next. Within one step the newest due input wins, because a held direction is a level rather than an edge. **Wrong for discrete actions**: a "fire" and a "jump" coming due on the same step would collapse to one here.
*   **`drain_due(&mut self, current: u64) -> impl Iterator<Item = Input> + '_`**: every input due on tick `current`, in scheduled order: **event semantics**, for inputs that are discrete actions where each one matters. The counterpart of `execute_due`. A game with both kinds keeps two schedules, because the two kinds have different loss semantics and mixing them in one queue forces one of them to be wrong.
*   **`clear(&mut self)`**: drops everything buffered, for a seat being vacated. Counters survive: they describe the session, not the occupant.
*   **`accepted() -> u64`** (inputs buffered, scheduled plus late; the denominator every other count needs), **`late() -> u64`** (inputs accepted after their tick had passed), **`rejected() -> u64`** (inputs dropped for naming a closed or far-future tick).
*   **`rejected_split() -> (u64, u64)`**: the same drops by side, `(closed tick, too far ahead)`.
*   **`last_reject_margin() -> Option<i64>`**: `named - current` at the most recent rejection, in ticks. Negative is behind the simulation, positive is ahead of it; `None` until something has been rejected.

**Why the split and the margin are worth keeping.** A single `rejected` total says a client cannot act and nothing about why, and the two sides have opposite causes: a steady negative margin means everything feeding that client's aim (its clock estimate, and the newest server stamp it has actually received) trails the simulation, which points at its downstream; a positive one means its clock runs fast. Diagnosing this from the client alone is not possible, because a rejected input is still acknowledged on arrival, so the client sees a healthy acknowledgement stream while nothing it does takes effect.

## 11. Seats (module `seats`)

### Enum `Seating`

**`Fresh(usize)`**, **`Existing(usize)`**, **`Full`**, plus **`index() -> Option<usize>`** and **`is_fresh()`**.

Returning this rather than a bare index is the entire reason the type exists. `Fresh` versus `Existing` is exactly "reset this seat's accumulated state" versus "do not", and a server usually keeps advancing per-seat state whether or not anybody is in the seat, which is the correct thing to do and is what makes the trap reachable. Collapsing the two is the warm-arena join bug: a joiner taking a seat whose relevance baseline was already most of the world got a first frame that was a delta against a baseline it never received, almost nothing arrived as `entered`, and the stream had no path back. Making the caller match on freshness turns a thing you have to remember into a thing the compiler asks about.

### Struct `SeatTable<Key>`

*   **`new(capacity)`**, **`seat(key) -> Seating`**, **`unseat(&key) -> Option<usize>`** (idempotent: a disconnect can be reported more than once), **`seat_of(&key)`**.
*   **`occupants()`**, **`keys()`**, **`by_seat() -> HashMap<usize, Key>`**, **`occupied_count()`**, **`capacity()`**, **`is_full()`**, **`clear()`**.
*   **`reseat_all(capacity) -> Vec<(Key, usize)>`**: resize, returning everyone's new seat. For a world rebuilt under a changed configuration. Anyone who does not fit is dropped from the table and is not in the returned list; every returned seat is fresh by definition, since the world behind it is new.

### Struct `SeatSlots<Key>`

The tri-state slot map: `SeatTable`'s sibling with one more state, `Held`, for a seat kept while its occupant is away. One of the two blocks `Roster` is composed of, and public for the same reason every prescription's blocks are: a seating policy `Roster` does not express is built from these directly.

*   **`new(capacity)`**, **`first_open()`**, **`seat(key, seat)`** (the seat must be open), **`hold(&key)`** / **`resume(&key)`** / **`open(&key)`** (each returns the seat, `None` when the transition does not apply), **`is_held(&key)`**, **`seat_of(&key)`**, **`state(seat) -> SeatState`**, **`capacity()`**, **`occupied_count()`**.

### Struct `RankedQueue<Key>`

A queue with priority bands: better (lower) ranks first, arrival order within a band, membership removal. The other block `Roster` composes.

*   **`new()`**, **`push(key, rank) -> position`**, **`remove(&key)`**, **`position(&key)`**, **`best()`** / **`pop_best()`** (next out, with its rank), **`iter()`**, **`len()`**, **`is_empty()`**.

### Struct `Roster<Key>`

Seating with the policies games actually vary; `SeatTable` stays the right choice for plain seat-on-arrival, free-on-leave. A `Roster` composes `SeatSlots` and `RankedQueue` into four orthogonal axes, each off by default: a **lock** for games that seat only between rounds (`lock()`/`unlock()`; a locked roster turns everyone away or queues them whatever the free count), a **waitlist** (`with_waitlist()`; turned-away keys queue for the next open seat), **held seats** (`holding_seats()`; a departure keeps the seat until you call `expire`), and **ranks** (`admit_ranked(key, rank)`, lower is better; the classic use is people at 0 and bots at 1, so a bot holds a seat only until a person wants one). A bot bench needs no axis at all: `SeatState::Open` says nothing about who drives the seat, so a game whose empties are bots reads every open seat as bot-driven and the roster does not know.

Three rules run through it. **Promotion happens on the tick**: `admit` and `depart` settle the arriving or leaving key immediately, but a freed seat reaches the waitlist only in `resolve()`, called from your `TimeStep` arm, because seating decided in two places is the bug the pong example spent a comment warning about. **Ranks displace only across bands**: at `resolve`, a waiter with a better rank takes the worst-ranked human seat (later seats first); equals never displace each other, and a held seat is never displaced, because the hold is a promise. **No clocks**: a held seat stays held until `expire(&key)`; how long that takes is between you and your `ReconnectTracker`.

*   **`new(capacity)`**, **`with_waitlist()`**, **`holding_seats()`**, **`lock()`**, **`unlock()`**, **`is_locked()`**.
*   **`admit(key) -> Admission`** (rank 0; a roster whose admissions all use this never displaces anyone), **`admit_ranked(key, rank) -> Admission`**: seats, resumes, queues or turns away, in that order of preference. `Admission` is **`Seated { seat, fresh }`** (same freshness contract as `Seating`), **`Resumed { seat }`** (their held seat is theirs again, everything in it intact: resend state, reset nothing), **`Waitlisted { position }`**, or **`Turned(Turnaway)`** with `Turnaway::{Full, Locked}`; whether a turnaway means spectating or refusal is the application's answer.
*   **`depart(&key) -> Departure`**: **`Freed { seat }`**, **`Held { seat }`** (start their clock), **`Unwaitlisted`**, or **`NotPresent`**. Idempotent, because a disconnect can be reported more than once; a repeat report of a held key reports the hold again rather than breaking it.
*   **`expire(&key) -> Option<usize>`**: releases a held seat whose grace ran out. **`resolve() -> Vec<Shuffle<Key>>`**: seats the waitlist into open seats in queue order, then settles rank displacement; a no-op while locked. `Shuffle` is **`Promoted { key, seat }`** (the seat is fresh) or **`Displaced { key, seat }`** (requeued at the tail of their own rank band).
*   **`seat_of(&key)`**, **`seat_state(seat) -> SeatState`** (`Human(&Key)` / `Held(&Key)` / `Open`), **`seats()`**, **`waiting()`**, **`capacity()`**, **`occupied_count()`** (held seats count: a held seat is not free), **`is_full()`**.

### Struct `Crew<Key>`

Bots in the roster: real seats, no connection. A bot must occupy a seat through the same admission as a person, so capacity, numbering and displacement stay one system, and must hold no agent, so it lives on the simulation path and never the send path. The keys are the caller's, usually carved from the top of the id space so a person's id can never collide.

*   **`new()`**, **`fill(&mut roster, count, rank, key_of) -> Vec<usize>`**: admits up to `count` bots (`key_of` names bot `0..count` and owns uniqueness) and says which seats they took; stops at the first non-seat, because a full roster stays full for every later bot.
*   **`holds(seat)`**, **`seats()`** (ascending, because bot thinking usually draws from one shared random stream and hash-map order would decide who draws what), **`len()`**, **`is_empty()`**, **`vacate(&mut roster, seat)`**.
*   **`prune(&mut roster) -> Vec<usize>`**: drops every bot the roster no longer seats, and withdraws their keys, because a displaced key is requeued and would otherwise re-seat itself as a stranger the moment a seat opened. Call after admissions when bots are ranked worse than people, and stand the pruned bots down in the simulation.

## 12. Rates (module `meter`)

A re-export of `plaza_client_utils::meter` (`RateMeter`), documented there. It lives in the client crate because a client panel needs it as much as a server does and a wasm bundle must not inherit the server crate to read its own bandwidth; the `plaza_server_utils::meter::RateMeter` path is unchanged.

## 13. One-shot ops (module `oneshot`)

### Struct `Pending<K, Op>`

Saying a one-shot thing until the other end proves it heard. Every server has a handful of ops with nothing behind them: a `Welcome` that hands out a seat, a `Refused` that explains why there is not one. The streams around them recover by themselves; losing a `Welcome` costs the session, because the client waits for a seat it already holds and nothing in the protocol will ever mention it again.

*   **`new()`** (`RETRY_MS` 400, `ATTEMPTS` 8), **`with_schedule(retry_ms, attempts)`**.
*   **`declare(key, op, now_ms) -> Op`**: records an op as sent and returns it for sending. A newer verdict for the same key supersedes the old.
*   **`due(now_ms, lossy: bool) -> Vec<(K, Op)>`**: whatever is due to be said again. `lossy` is false on a link that cannot lose a frame, where this forgets everything instead: on a reliable stream a lost segment is retransmitted, so repeating anything is noise on the wire. A peer past its attempts is dropped rather than retried for ever; the transport will confirm it is gone soon enough.
*   **`confirm(&key)`**: the peer said something, which proves it heard. Call it from the op path; no ack op has to exist, because a client that is talking has plainly received whatever let it talk.
*   **`is_empty()`**.

## 14. Error Handling

This crate defines no error type. `HistoricalStateBuffer::get_state_at_or_before` returns `Option`, `None` meaning the entity has no recorded history.
