# Usage Guide: plaza_server_utils

How to decide what each client is told: judging a shot against the world the shooter saw, gathering who is nearby, subscribing to entities no radius will return, filling a packet to a budget, summarising what is too far to send exactly and streaming the result so a lost packet recovers.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [Relevance for Every Client](#relevance-for-every-client)
*   [Judging a Shot Fairly](#judging-a-shot-fairly)
    *   [Recording History](#recording-history)
    *   [Rewinding to the Instant the Client Saw](#rewinding-to-the-instant-the-client-saw)
    *   [Refusing a Rewind Past the Buffer](#refusing-a-rewind-past-the-buffer)
    *   [Sizing the Buffer](#sizing-the-buffer)
*   [Gathering Who Is Nearby](#gathering-who-is-nearby)
    *   [Indexing the World](#indexing-the-world)
    *   [Querying a Radius](#querying-a-radius)
    *   [Diffing Into Spawn and Despawn Streams](#diffing-into-spawn-and-despawn-streams)
    *   [Three Dimensions](#three-dimensions)
    *   [Measuring What the Third Axis Costs](#measuring-what-the-third-axis-costs)
*   [Publishing by Cell](#publishing-by-cell)
    *   [Packing One Payload per Occupied Cell](#packing-one-payload-per-occupied-cell)
    *   [Addressing a Bounded World](#addressing-a-bounded-world)
    *   [Keeping State per Cell](#keeping-state-per-cell)
    *   [Swapping In a Dense Grid](#swapping-in-a-dense-grid)
*   [Subscribing Beyond Distance](#subscribing-beyond-distance)
    *   [Creating a Subscription](#creating-a-subscription)
    *   [Unioning Both Channels](#unioning-both-channels)
    *   [Leaving a Group Versus Leaving the World](#leaving-a-group-versus-leaving-the-world)
    *   [Telling the Client Why](#telling-the-client-why)
*   [Filling the Packet](#filling-the-packet)
    *   [Scoring and Fitting a Budget](#scoring-and-fitting-a-budget)
    *   [Packing Until the Packet Is Full](#packing-until-the-packet-is-full)
    *   [Not Paying for What Is Asleep](#not-paying-for-what-is-asleep)
*   [Summarising What Is Too Far to Send](#summarising-what-is-too-far-to-send)
    *   [Building the Tree](#building-the-tree)
    *   [Walking It per Viewer](#walking-it-per-viewer)
    *   [Choosing Theta](#choosing-theta)
*   [Streaming a Changing Set Reliably](#streaming-a-changing-set-reliably)
    *   [Sending a Delta](#sending-a-delta)
    *   [Taking an Acknowledgement](#taking-an-acknowledgement)
    *   [Choosing a Recovery Policy](#choosing-a-recovery-policy)
*   [Sending Only What Changed](#sending-only-what-changed)
    *   [Diffing What a Viewer Holds](#diffing-what-a-viewer-holds)
    *   [Deciding What an Absence Means](#deciding-what-an-absence-means)
    *   [Announcing Once](#announcing-once)
    *   [Forgetting a Viewer](#forgetting-a-viewer)
*   [Seating Players](#seating-players)
    *   [Admitting and Departing](#admitting-and-departing)
    *   [Waitlists and Displacement](#waitlists-and-displacement)
    *   [Filling Empty Seats With Bots](#filling-empty-seats-with-bots)
*   [Scheduling Input](#scheduling-input)
*   [Delivering a One-Shot Op](#delivering-a-one-shot-op)
*   [Putting Numbers on Screen](#putting-numbers-on-screen)
    *   [Measuring a Rate](#measuring-a-rate)
    *   [Measuring How Wrong a Client Was](#measuring-how-wrong-a-client-was)
*   [What the Measurements Settled](#what-the-measurements-settled)
*   [Error Handling](#error-handling)

## Core Concepts

*   **Relevance**: who a client is told about because of where they are. A yes-or-no answer, recomputed every tick.
*   **Subscription**: who a client is told about because it chose them, wherever they are. A handful of entries with a lifetime of hours.
*   **Audience**: the two unioned, plus why each entry is in it.
*   **`SpatialGrid`**: a flat `(x, z)` bucket index, rebuilt each tick, that answers "who is near this point" without scanning the world.
*   **`Field`**: a uniform grid with a `Strategy` of flat, flat with a height band or volumetric, for measuring and serving relevance in three dimensions.
*   **`CellSpace`**: the dense index of every cell in a world with known bounds, so per-cell data such as a `CellTable` lives in a flat `Vec` instead of a hash map.
*   **`VisibilitySet`**: a dense bitset of who is visible to one client, with a word-at-a-time diff that is the spawn and despawn stream.
*   **Priority**: which of the relevant entities fit this packet, ranked by a score that grows each tick an entity waits.
*   **At rest**: a run of quiet ticks. Sending it costs one bit where a velocity costs thirty-three.
*   **Aggregate**: a stand-in for a distant group at its weighted centroid, for entities a client computes with rather than only draws.
*   **Baseline**: what a subscriber has acknowledged, which is what a delta is diffed against.
*   **Digest**: an order-independent summary both ends compute, so a diverged mirror is detectable.
*   **`SlotKey`**: an index plus a generation, from `plaza_client_utils`, which both ends of a delta stream name entities by.
*   **Seat**: a bounded position in a room, where a fresh occupant must not inherit the last one's state.
*   **`Crew`**: the seats a fleet of bots holds, admitted through the same roster as people.
*   **`Told`**: a per-viewer record of what each client was last told, so a change-only stream sends nothing for a world that is not changing.

## Quick Start

### Relevance for Every Client

```rust,ignore
use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid, VisibilitySet};

let mut grid = SpatialGrid::new(GridQuantizer::new((0.0, 0.0), CELL));
let mut near: Vec<u32> = Vec::new();
let mut visible = VisibilitySet::with_capacity(MAX_ENTITIES);
let mut previous = VisibilitySet::with_capacity(MAX_ENTITIES);
let (mut entered, mut left) = (Vec::new(), Vec::new());

// Once a tick, over everything.
grid.clear();
for e in &entities {
  grid.insert(e.id, e.x, e.z);
}

// Once per client.
near.clear();
grid.query_radius(eye.x, eye.z, VIEW, &mut near);
visible.clear();
for id in &near {
  visible.insert(*id);
}
entered.clear();
left.clear();
visible.diff(&previous, &mut entered, &mut left);
send(Frame { entered: &entered, left: &left });
std::mem::swap(&mut visible, &mut previous);
```

## Judging a Shot Fairly

Clients render other entities slightly in the past, so a player aims at a target's past position. To judge a shot fairly the server rewinds to the instant the client saw.

### Recording History

```rust,ignore
use plaza_server_utils::HistoricalStateBuffer;

let mut history: HistoricalStateBuffer<EntityId, EntityState, u64> = HistoricalStateBuffer::new(64);

// Each server tick, for every entity:
history.record_state(entity_id, server_time, entity_state);
```

### Rewinding to the Instant the Client Saw

```rust,ignore
if let Some(past) = history.get_state_at_or_before(&target_id, shot.aim_time) {
  if hit_test(shot.aim, &past) {
    apply_damage(target_id);
  }
}
```

The state type is yours and the same type can feed both this buffer and a client's `SnapshotBuffer`, because both use the shared `Interpolatable` trait.

### Refusing a Rewind Past the Buffer

`get_state_at_or_before` clamps a time outside the retained window to the oldest or newest sample and the clamped answer looks the same as an exact one. `state_within` answers `None` instead.

```rust,ignore
let Some(past) = history.state_within(&target_id, shot.aim_time) else {
  return Verdict::TooOld;
};
if hit_test(shot.aim, &past) {
  apply_damage(target_id);
}
```

Use it wherever scoring against a guess would be wrong. `render_error_at` asks this way and skips entities whose samples have aged out.

The window's edges are readable per entity, so a rewind cap can come from what the buffer actually holds rather than from a sample count converted by hand:

```rust,ignore
let deepest_ms = players
  .iter()
  .filter_map(|p| history.oldest_time(&p.id))
  .max()
  .map(|oldest| now_ms.saturating_sub(oldest))
  .unwrap_or(0);

let rewind_ms = shot.rewind_ms.min(deepest_ms);
```

`newest_time` is the other edge. Full signatures are in the [API reference](API_REFERENCE.md#struct-historicalstatebufferentityid-entitystatesnapshot-servertime).

### Sizing the Buffer

Cover the deepest render delay a client may choose, at your tick rate. A buffer too shallow silently judges against the oldest sample it still holds rather than the instant asked for.

## Gathering Who Is Nearby

### Indexing the World

```rust,ignore
let quantizer = GridQuantizer::new((0.0, 0.0), 32.0);   // origin, cell width
let mut grid = SpatialGrid::new(quantizer);

grid.clear();
for e in entities.iter().filter(|e| e.alive) {
  grid.insert(e.id, e.x, e.z);
}
```

Rebuild each tick rather than tracking which cell an entity left: in a world where everything moves, the bookkeeping costs more than filling buckets that already have their capacity.

Cell width around a third of the view radius is what every example here uses.

### Querying a Radius

```rust,ignore
let mut candidates = Vec::new();
grid.query_radius(eye.x, eye.z, VIEW, &mut candidates);

for id in &candidates {
  if distance(eye, position_of(*id)) <= VIEW {
    out.push(*id);          // the grid over-returns; the exact test drops the extras
  }
}
```

For a locality sort or your own broadphase, the math primitive underneath is available alone:

```rust,ignore
use plaza_server_utils::relevance::morton;
let key = morton::encode_2d(cell_x, cell_z);
```

### Diffing Into Spawn and Despawn Streams

```rust,ignore
let mut visible = VisibilitySet::with_capacity(index_space);

visible.clear();
for id in &near {
  visible.insert(*id);
}
visible.diff(&previous, &mut entered, &mut left);
```

`diff` appends `visible & !previous` to `entered` and `previous & !visible` to `left`, a word at a time. It does not clear either `Vec`, so clear them yourself when you reuse them. Keep last tick's set as `previous` and swap the two afterwards. When an entity is destroyed rather than leaving range, call `previous.remove(index)` so the slot's next occupant shows up in `entered`.

`visible.digest()` computes the same value `plaza_client_utils::SetDigest` does over the client's membership, so both ends can check they still agree.

### Three Dimensions

`SpatialGrid` indexes two axes, which is right for a plane and for most 3D games, since a landscape is locally 2.5D. In open volume it over-returns rather than missing: a query on `(x, z)` answers with a disc where a sphere was asked for.

```rust,ignore
grid.query_radius(eye.x, eye.z, VIEW, &mut candidates);
candidates.retain(|id| (position_of(*id).y - eye.y).abs() <= VIEW);
```

The height filter is exact at the same query cost. It stops being free when entities **stack**: in a tower sharing one footprint, a flat cell holds every floor at once and most of what it returns is discarded. When entities stack, index the third axis instead.

### Measuring What the Third Axis Costs

`Field` is one uniform grid with a `Strategy` switch, so comparing a flat grid, a flat grid with a height band and a volumetric grid means changing one enum. `field::truth` is the brute-force sphere every strategy is scored against.

```rust,ignore
use plaza_client_utils::math::Vec3;
use plaza_server_utils::field::{self, Field, Query, Strategy};

for strategy in Strategy::ALL {
  let mut grid = Field::new(CELL, strategy);
  grid.rebuild(&points);

  let mut out = Vec::new();
  let mut total = Query::default();
  for eye in &observers {
    let want = field::truth(&points, *eye, VIEW);
    let q = grid.query(*eye, VIEW, &mut out, &want);
    total.examined += q.examined;
    total.false_positives += q.false_positives;
    total.missed += q.missed;
  }
  println!("{:<14} examined {:>8} over-sent {:>8}", strategy.name(), total.examined, total.false_positives);
}
```

`rebuild` files each point under its index in the slice. `examined` is how many candidates the query pulled out of cells and tested, which the result set cannot show. `missed` above zero is a bug.

Once a strategy is chosen, the same type serves. Pass `&[]` for the truth on a serving path, which skips the scoring:

```rust,ignore
let mut ships = Field::new(CELL, Strategy::FlatBand);

ships.rebuild(&positions);
ships.query(eye, VIEW, &mut visible, &[]);
```

Pick `FlatBand` when entities are spread out and `Volume` when they stack. The [API reference](API_REFERENCE.md#module-field-field-strategy-query) lists every field of `Query`.

## Publishing by Cell

Per-viewer relevance builds one frame per client. When many clients share a neighbourhood, pack each occupied cell once and hand every viewer the cells its view touches, so the build cost follows the occupied cells rather than the client count.

### Packing One Payload per Occupied Cell

```rust,ignore
use std::collections::HashMap;

let mut published: HashMap<u64, Bytes> = HashMap::new();
for (key, ids) in grid.occupied() {
  published.insert(key, pack_cell(ids));
}

for viewer in &viewers {
  for key in grid.quantizer().keys_in_radius(viewer.x, viewer.z, VIEW) {
    if let Some(payload) = published.get(&key) {
      send_to(viewer.id, payload.clone());
    }
  }
}
```

`keys_in_radius` is cell-granular, so a viewer receives the square of cells around it rather than the disc. `grid.members(key)` reads one cell on its own.

A payload that knows its cell can quantise positions over one cell's width instead of the world's:

```rust,ignore
use plaza_server_utils::relevance::morton;

let (cx, cz) = morton::decode_2d(key);
let (ox, oz) = grid.quantizer().corner(cx, cz);
for id in ids {
  let at = position_of(*id);
  write_offset(&mut w, at.x - ox, at.z - oz, CELL);
}
```

`cells_in_radius` is the same walk yielding `(cx, cz)` pairs rather than Morton keys.

### Addressing a Bounded World

A Morton key can only be looked up in a hash map. When the world has known bounds, `CellSpace` gives every cell a dense index, so anything keyed by cell can live in a flat `Vec`.

```rust,ignore
use plaza_server_utils::relevance::{CellSpace, GridQuantizer};

let space = CellSpace::new(GridQuantizer::new((-EDGE, -EDGE), CELL), EDGE * 2.0);

let here = space.index_of(eye.x, eye.z);
let (cx, cz) = space.cell_at(here);
let (ox, oz) = space.corner(here);

for index in space.indices_in_radius(eye.x, eye.z, VIEW) {
  subscribe_to_cell(viewer, index);
}
```

A point outside the bounds is filed in the border cell on that side rather than rejected, so size the space to the world. `CellSpace` is `Copy`, so every table built over it can hold its own copy. The [API reference](API_REFERENCE.md#struct-cellspace) has the full surface.

### Keeping State per Cell

`CellTable<T>` is a `Vec` of one `T` per cell of a `CellSpace`. The same addressing serves buckets of ids, one published payload per cell and the inverse index of who listens to each cell.

```rust,ignore
use plaza_server_utils::relevance::CellTable;

let mut buckets: CellTable<Vec<u32>> = CellTable::new(space);
let mut published: CellTable<Option<Bytes>> = CellTable::new(space);
let mut listeners: CellTable<Vec<PlayerId>> = CellTable::new(space);

// Once a tick.
buckets.clear_each();
for (i, ant) in ants.iter().enumerate() {
  if let Some(bucket) = buckets.get_mut(space.index_of(ant.x, ant.y)) {
    bucket.push(i as u32);
  }
}

published.clear_each();
for (index, ids) in buckets.occupied() {
  if let Some(slot) = published.get_mut(index) {
    *slot = Some(pack_cell(ids));
  }
}
```

Then invert the viewers' windows and send each payload once to everyone listening:

```rust,ignore
listeners.clear_each();
for viewer in &viewers {
  for index in space.indices_in_radius(viewer.x, viewer.y, VIEW) {
    if let Some(who) = listeners.get_mut(index) {
      who.push(viewer.id);
    }
  }
}

for (index, payload) in published.occupied() {
  if let (Some(payload), Some(who)) = (payload, listeners.get(index)) {
    if !who.is_empty() {
      send_to_all(who, payload.clone());
    }
  }
}
```

`clear_each` empties every cell while keeping each one's allocation and `occupied` skips the empty ones. Both need `T: Clearable`, which `Vec<T>` and `Option<T>` implement. A table of plain values uses `reset` and the positional accessors:

```rust,ignore
let mut food: CellTable<u16> = CellTable::new(space);

if let Some(amount) = food.at_mut(x, y) {
  *amount = amount.saturating_sub(1);
}
food.reset();
```

The [API reference](API_REFERENCE.md#struct-celltablet) lists every accessor and the [`Clearable`](API_REFERENCE.md#trait-clearable) trait.

### Swapping In a Dense Grid

`DenseGrid` is `SpatialGrid` over a `CellSpace`: the buckets sit in a flat `Vec` and a cell lookup is an index rather than a hash. `clear`, `insert`, `query_radius` and `quantizer` are spelled the same, so swapping is the type and its constructor.

```rust,ignore
use plaza_server_utils::relevance::DenseGrid;

let mut grid = DenseGrid::new(space);

grid.clear();
for e in &entities {
  grid.insert(e.id, e.x, e.z);
}
grid.query_radius(eye.x, eye.z, VIEW, &mut near);

for (index, ids) in grid.occupied() {
  if let Some(slot) = published.get_mut(index) {
    *slot = Some(pack_cell(ids));
  }
}
```

`members` and `occupied` name a cell by its dense index, so they line up with any `CellTable` over the same space. It is not re-exported at the crate root, so import it from `relevance`.

Measure before swapping. The dense grid pays where most cells hold something. In a bounded world that is sparsely filled, the hashed grid's few buckets stay in cache and it can be the faster of the two. See the [API reference](API_REFERENCE.md#struct-densegridid-copy).

## Subscribing Beyond Distance

`relevance` answers who is near a client. A party health bar, a raid frame through a wall, a spectator following one player and a guild roster need entities the client chose, which no radius expresses.

### Creating a Subscription

```rust,ignore
use plaza_server_utils::subscription::{Subscriptions, Audience, Because};

let mut subs: Subscriptions<Seat> = Subscriptions::new(MAX_SUBSCRIPTIONS);   // or default() for no limit

subs.subscribe(spectator, player);   // directed: the player does not follow back
subs.pair(a, b);                     // symmetric
subs.group(party_of_a, party_of_b);  // merges two groups whole
```

Subscriptions are indexed **both ways round**, because both directions are queried every tick: a sender needs the set it must include and a departing key needs everyone who has to be told.

### Unioning Both Channels

```rust,ignore
grid.query_radius(eye.x, eye.z, VIEW, &mut near);
let audience = Audience::of(&near, &subs, &viewer);

for (key, because) in &audience.entries {
  frame.push(entry_for(*key, *because));
}
let second_channel_cost = audience.added;   // only what distance missed
```

`entries` comes back sorted by key, so it can be diffed between ticks. `audience.keys()` iterates the keys alone and `audience.visible()` only the near ones, which are the ones with a body to draw.

### Leaving a Group Versus Leaving the World

These are different events. Treat them alike and a health bar keeps updating for somebody who has gone.

```rust,ignore
subs.leave_group(&player);            // left the party, kept their spectators
let watchers = subs.remove(&player);  // left the world; tell each of these
```

Both dissolve a group of one rather than keeping it.

### Telling the Client Why

`Audience` labels every entry with a `Because`. Spell the three reasons again in your own protocol rather than putting this crate's type on the wire:

```rust,ignore
let why = match audience.why(&key) {
  Some(Because::Near) => Reason::Near,
  Some(Because::Subscribed) => Reason::Subscribed,
  Some(Because::Either) => Reason::Either,
  None => return,
};
```

Send this reason on the wire. Absence from a later frame means "walked away" for a near entry and "left the world" for a subscribed one, so a client that cannot tell them apart drops a party member as soon as they leave view.

Subscriptions are bounded and a subscription over the limit is refused rather than truncated. Silently dropping an entry to fit leaves a client in a party it cannot fully see.

## Filling the Packet

Relevance answers who can see what. If a hundred entities are relevant and the budget holds twenty, something has to pick which twenty go this tick. Taking the first twenty by id starves the rest and so does taking the nearest twenty.

### Scoring and Fitting a Budget

```rust,ignore
use plaza_server_utils::priority::PriorityAccumulator;

let mut priority = PriorityAccumulator::new(index_space);

// Each tick: everything relevant gains, by whatever rate you choose per entity.
for key in audience.keys() {
  priority.bump(index_of(*key), rate_for(*key));
}

// Then fit the budget. Take the cost per entity from your encoding rather than an estimate.
let mut chosen = Vec::new();
priority.fill(budget_bytes, |index| encoded_size(index), &mut chosen);
for index in &chosen {
  frame.push(entry_at(*index));
}
```

`fill` clears `chosen` first and returns indices highest first. The chosen entities reset to zero. Whatever did not fit **keeps its accumulated score**, so it ranks higher next tick. The walk continues past an entity too large to fit, so one big one near the front cannot leave the rest of the packet empty. Ties break by index, so a server and a replay of it agree.

### Packing Until the Packet Is Full

`fill` plans against a cost known up front. With delta encoding one entity can cost anything from a few bits to a full absolute and no single estimate covers that range. Split `fill` in two instead: `order` ranks without touching a score, you pack until the packet is actually full and `sent` clears the scores of what went out.

```rust,ignore
let mut order = Vec::new();
priority.order(&mut order);

let mut sent = Vec::new();
for &id in &order {
  if packer.try_push(entry_for(id)) {
    sent.push(id);
  }
}
priority.sent(&sent);
```

`order` writes every entity scoring above zero, highest first, with ties broken by index. Pass `sent` only what actually travelled: clearing an entity that was not sent starves it. See the [API reference](API_REFERENCE.md#struct-priorityaccumulator).

### Not Paying for What Is Asleep

In a settled scene most things are not moving. Marking an entity at rest costs one bit, where a velocity costs thirty-three.

```rust,ignore
use plaza_server_utils::rest::RestDetector;

let mut rest = RestDetector::with_capacity(index_space, QUIET_TICKS);

rest.observe(id, moving);            // your own test for "moving"
if rest.at_rest(id) {
  frame.push_at_rest(id);            // one bit
} else {
  frame.push_with_velocity(id, v);
}
```

An entity is at rest after a **run** of `QUIET_TICKS` quiet ticks and wakes on the first moving tick. A single quiet tick is not enough, since a body at the top of its arc has zero velocity and is about to fall. Waking late shows on screen, while resting late only costs bandwidth. `RestDetector::new(threshold)` starts empty and grows as indices are observed. `wake(id)` covers a teleport or respawn no speed test would catch.

Feed it a per-body speed test rather than a solver's own flag: a solver sleeps an *island*, so one cube jostling in a heap holds the whole heap awake.

Both are indexed densely, so a `SlotKey` index works for both. To update at-rest entities less often, give them a lower score.

## Summarising What Is Too Far to Send

Relevance gives a yes-or-no answer. That works for entities a client only *draws*. For entities it has to *compute* with, dropping one silently changes the result.

### Building the Tree

```rust,ignore
use plaza_server_utils::aggregate::AggregateTree;

// Once per tick. Prefer build_in with the world's own bounds: build fits the
// cell to the current extent, so one entity drifting outward re-centres the
// whole subdivision and clusters re-form for reasons unrelated to their members.
let tree = AggregateTree::build_in(&points, world_center, world_size, 10);
```

Build cost is O(n log n) once per tick regardless of how many clients ask.

### Walking It per Viewer

```rust,ignore
let mut out = Vec::new();
tree.summarize(eye.x, eye.y, 0.5, &mut out);

for summary in &out {
  if summary.count == 1 {
    send_exactly(tree.members(summary)[0]);
  } else {
    send_summary(summary.x, summary.y, summary.weight);
  }
}
```

O(log n) summaries per viewer. The module does not interpret the weight. It can be mass for a gravity field, a headcount for a crowd, a cluster's threat for target selection or an accumulated noise level. The only requirement is that the quantity be additive and that a distant group be adequately described by its weighted centroid.

### Choosing Theta

```rust,ignore
tree.summarize(eye.x, eye.y, 0.0, &mut out);   // accepts nothing: every point, exactly
tree.summarize(eye.x, eye.y, 0.5, &mut out);   // a simulation consuming the summaries
tree.summarize(eye.x, eye.y, 1.5, &mut out);   // a drawing consuming them
```

`theta = 0.0` is "aggregation off" through the same code path rather than a second implementation.

Past about `1.0` the criterion starts accepting cells the viewer is sitting close to, dropping a whole quadrant's weight onto a single nearby point. How coarse the approximation can be depends on the consumer: a simulation compounds it into error and a drawing does not.

## Streaming a Changing Set Reliably

`VisibilitySet::diff` gives *entered* and *left* and the obvious next step is to send those and let each client keep a mirror. That diffs against **what the server last sent**, which assumes every packet arrives.

### Sending a Delta

```rust,ignore
use plaza_server_utils::delta::{DeltaBaseline, RecoveryPolicy};

let mut baselines: Vec<DeltaBaseline> = (0..MAX_SEATS).map(|_| DeltaBaseline::new(HISTORY)).collect();

// When a subscriber takes a seat.
baselines[seat].reset();

// Each send round, with `current` the subscriber's visible keys as a BTreeSet<u64>.
let plan = baselines[seat].plan(&current, seq);
send(Delta {
  seq,
  baseline_seq: plan.baseline_seq,
  full_baseline: plan.full_baseline,
  entered: plan.entered,
  left: plan.left,
});
```

`DeltaBaseline` diffs against **what the client acknowledged** instead. Keep one per subscriber. It owns that subscriber's baseline, the acknowledgement frontier, the staleness rebuild and the digest drift check and does not interpret the keys. `history` is how many sent states it keeps: cover the packets in flight plus the acknowledgement's return trip.

`full_baseline` tells the client to clear its mirror before applying the packet. `reset` matters for a reused seat, where the previous occupant's acknowledged state would otherwise pass as a baseline for somebody who never saw it.

### Taking an Acknowledgement

```rust,ignore
baselines[seat].observe_ack(ack.newest, ack.mask, ack.digest);
```

`ack.digest` is the client's own `SetDigest` over the keys its mirror holds. When it disagrees with what the server believes the client reached, the next plan is a full rebuild. With `with_flow` on, use `observe_ack_at(newest, mask, digest, now)` so the arrival time is recorded.

The baseline is the newest **contiguous** acknowledgement rather than the newest bit set, because receiving packet N+1 after losing N does not put a client in the state N+1 implies.

The keys carry generations, which loss recovery depends on: a retraction re-derived after a slot was recycled would otherwise name the slot's current occupant, so the client's lookup misses and the entity it actually holds is never mentioned again.

### Choosing a Recovery Policy

```rust,ignore
let baseline = DeltaBaseline::new(HISTORY).with_policy(RecoveryPolicy::AckRecovery);   // the default
let baseline = DeltaBaseline::new(HISTORY).with_policy(RecoveryPolicy::Naive);         // diff against what was last sent

baselines[seat].set_policy(RecoveryPolicy::Naive);   // on a live subscriber, which resets it
```

`Naive` keeps the broken behaviour available so the failure can be demonstrated. `observe_ack` does nothing under it. Under `AckRecovery` there is no acknowledged state before the first acknowledgement, so the first round trip sends full sets and the stream is incremental after that.

The client's half is `plaza_client_utils::DeltaMirror`.

## Sending Only What Changed

A frame that repeats everything in view every tick suits movers and wastes bandwidth on anything that holds still: a depleted resource, a door, a spawn announced once. `Told` remembers what each viewer was last told and diffs it against what is true now.

### Diffing What a Viewer Holds

```rust,ignore
use plaza_server_utils::Told;

let mut told: Told<Seat, u32, u32> = Told::new();

let mut changed = Vec::new();
told.diff(seat, visible.iter().map(|p| (p.id, p.ready_at)), |id, value| {
  if let Some(ready_at) = value {
    changed.push(PropState { id, ready_at: *ready_at });
  }
});
```

A fresh viewer is told everything on the first call. After that `say` receives `Some` only for a key that is new to the viewer or whose value changed. The value needs to be stable: one that jitters is said every tick and the saving is gone.

### Deciding What an Absence Means

`say(key, None)` is a key the viewer held that is missing from `current`. "No longer true" and "no longer visible" arrive as the same absence and only the application can tell them apart.

```rust,ignore
told.diff(seat, visible.iter().map(|p| (p.id, p.ready_at)), |id, value| match value {
  Some(ready_at) => changed.push(PropState { id, ready_at: *ready_at }),
  None if in_view(eye, prop_tile(id)) => changed.push(PropState { id, ready_at: 0 }),
  None => {}
});
```

A prop that reverted while still in view has to be said. Otherwise the client keeps drawing the depleted version. One that fell out of view can be dropped silently. The key is forgotten either way, so if it comes back it is announced from scratch, which is also what lets a reused slot be re-announced. The `None` keys arrive sorted, so two identical runs produce identical bytes.

### Announcing Once

With `V = ()` a key is said the tick it first enters the viewer's view and never again until it leaves and returns.

```rust,ignore
let mut told: Told<PlayerId, u32, ()> = Told::new();

told.diff(player, bolts.iter().map(|b| (b.id, ())), |id, fresh| {
  if fresh.is_some() {
    announce.push(id);
  }
});
```

### Forgetting a Viewer

```rust,ignore
told.forget(&seat);
```

Call it when a viewer departs or switches to a stream that repeats everything. A stale record would make the first change-only frame after switching back wrong. `holdings(&seat)` says how many keys a viewer holds.

`Told` is the state half of a private channel. The transcript half ("what just happened", said once to its one audience) is a `Vec` drained into the frame. See the [API reference](API_REFERENCE.md#struct-toldviewer-k-v).

## Seating Players

### Admitting and Departing

```rust,ignore
use plaza_server_utils::{Roster, Admission, Departure};

let mut roster: Roster<PlayerId> = Roster::new(MAX_SEATS);

match roster.admit(player) {
  Admission::Seated { seat, fresh } => world.spawn(seat, fresh),
  Admission::Turned(_) => refuse(player),
  _ => {}
}

if let Departure::Freed { seat } = roster.depart(&player) {
  world.remove(seat);
}
```

A fresh occupant must not inherit the last one's accumulated state. `fresh` on `Admission::Seated` says whether to reset the seat's per-seat state. `Resumed` means a held seat came back to its occupant with everything intact, so resend state and reset nothing. For plain seat-on-arrival without a `Roster`, `SeatTable::seat` returns `Seating::Fresh` or `Seating::Existing` with the same meaning.

### Waitlists and Displacement

`Roster` is composed of `SeatSlots` and `RankedQueue`, both public, so the policies compose: a lock for games that seat only between rounds, a ranked waitlist, displacement where a bot holds a seat only until a person wants one, seats held across an absence and bot-driven empties.

### Filling Empty Seats With Bots

A bot takes a seat through the same admission as a person, so capacity, numbering and displacement stay one system. `Crew` remembers which seats are bots', which the roster cannot.

```rust,ignore
use plaza_server_utils::{Crew, Roster};

let mut roster: Roster<PlayerId> = Roster::new(MAX_SEATS).with_waitlist();
let mut crew: Crew<PlayerId> = Crew::new();

for seat in crew.fill(&mut roster, BOTS, 1, |i| PlayerId::MAX - i as PlayerId) {
  world.spawn_bot(seat);
}
```

`key_of` names bot `0..count` and owns uniqueness. Carving keys from the top of the id space keeps them clear of people. A second `fill` needs its own range or it resumes the first fill's seats. `fill` stops at the first bot that is not seated.

Bots hold no agent, so they live on the simulation path and never the send path. Drive them in seat order:

```rust,ignore
for seat in crew.seats() {
  think(seat, &mut rng);
}
```

`seats()` is ascending, so bots drawing from one shared random stream draw in the same order every run.

With bots at rank 1 and people at rank 0, a person arriving at a full roster is waitlisted and displaces a bot at `resolve`. Call `prune` afterwards and stand the displaced bots down:

```rust,ignore
roster.admit(person);
for shuffle in roster.resolve() {
  apply(shuffle);
}
for seat in crew.prune(&mut roster) {
  world.stand_down(seat);
}
```

`prune` also withdraws the displaced keys from the waitlist, which would otherwise re-seat a bot the crew no longer knows about as soon as a seat opened. `crew.vacate(&mut roster, seat)` stands one bot down by choice. See the [API reference](API_REFERENCE.md#struct-crewkey).

## Scheduling Input

```rust,ignore
use plaza_server_utils::input_schedule::{InputSchedule, InputWindow, Submission};

let mut schedules: Vec<InputSchedule<Dir>> = (0..MAX_SEATS).map(|_| InputSchedule::new()).collect();
let window = InputWindow { max_late: 2, max_early: 30 };

// When an input arrives naming the server tick it is meant for.
let current = tick_of(sim_clock);
match schedules[seat].submit(msg.tick, msg.dir, current, window) {
  Submission::TickClosed | Submission::TooFarAhead => report_refused(seat),
  Submission::Scheduled | Submission::Late => {}
}
```

Keep one schedule per seat and derive `current` from the simulation clock at the call site rather than keeping a counter beside it. An input naming a tick already closed is dropped rather than shifted into the window.

Take due inputs once per simulation step. `execute_due` is for a held state such as a direction, where the newest due input wins. `drain_due` is for discrete actions, where each one counts:

```rust,ignore
if let Some(dir) = schedules[seat].execute_due(current) {
  steer(seat, dir);
}
for action in actions[seat].drain_due(current) {
  perform(seat, action);
}
```

A game with both kinds keeps two schedules. The counters say why inputs are refused: `accepted()`, `late()`, `rejected()`, `rejected_split()` for closed against too far ahead and `last_reject_margin()` for how far off the last one was.

## Delivering a One-Shot Op

An op with nothing behind it, a `Welcome` or a `Refused`, is lost on a lossy link, because nothing in the protocol sends it again.

```rust,ignore
use plaza_server_utils::oneshot::Pending;

let mut pending: Pending<PlayerId, Op> = Pending::new();

// On join: record the op and send it now.
let welcome = pending.declare(player, Op::Welcome { seat }, now_ms);
send_to(player, welcome);

// Each tick: resend whatever is due. `lossy` is false on a link that cannot drop a frame.
for (player, op) in pending.due(now_ms, lossy) {
  send_to(player, op);
}

// Any op from the peer shows the one-shot arrived. Call it on departure too.
pending.confirm(&player);
```

A newer op for the same key supersedes the old one. A peer still silent after the last attempt is dropped rather than retried for ever. `Pending::new()` retries every 400 ms up to 8 times and `with_schedule(retry_ms, attempts)` changes both.

## Putting Numbers on Screen

### Measuring a Rate

```rust,ignore
use plaza_server_utils::meter::RateMeter;

let mut meter = RateMeter::new();

meter.add(bytes_sent);
meter.elapsed(sim_clock_ms);

hud.line(format!("{:.1} KiB/s", meter.per_sec() / 1024.0));
```

The clock is supplied rather than read, so a simulation running on its own time measures itself correctly. Use the windowed `per_sec` rather than `lifetime_per_sec`: a session average keeps climbing toward the current rate without reaching it.

`RateMeter` lives in `plaza_client_utils` and is re-exported here, so a client panel measures with the same type. Its surface is in [that crate's reference](../client_utils/API_REFERENCE.md#struct-ratemeter).

### Measuring How Wrong a Client Was

```rust,ignore
use plaza_server_utils::render_error::render_error_at;

let drawn = client.drawn().map(|(id, state)| (id, state.clone()));
let err = render_error_at(&history, client_render_time, drawn, |a, b| a.pos.dist(b.pos));

hud.line(format!("mean {:.1} px, worst {:.1} px", err.mean(), err.worst()));
```

`drawn` is every `(id, state)` the client put on screen and the closure is your distance between two states. Entities missing from the history or aged out of it are skipped rather than scored. `RenderError::merge` folds results from several clients or frames together. It needs the true positions, so it is a host or harness measurement.

Measure at the instant the client drew. Measured against the present, the figure charges a client for a render delay it chose, so it grows with buffer depth even when nothing is wrong.

## What the Measurements Settled

**A flat grid in open volume costs 7.1x the bandwidth per client**, with the game looking entirely correct, because a query on `(x, z)` returns the disc rather than the sphere. The height filter is exact at the same query cost.

**A height filter stops being free when entities stack.** Thirty people on each of twenty-four floors sharing one footprint: the filter is still exact but examines 2.7x what a volumetric grid does, because a flat cell holds every floor at once and 72% of what it pulls out is thrown away. The same people on one floor put the two back level.

**Priority plus rest took 901 cubes from 4.20 Mbit/sec to 0.25** under a 256 kbit budget and adding delta encoding bought 206 cubes refreshed per tick instead of 46 inside that same budget. Derive the per-entity cost from your encoding: the guessed figure overran by 20%.

**A per-body speed test beats a solver's own sleep flag.** A solver sleeps an island, so one cube jostling in a heap holds the whole heap awake. Feeding a per-body test to `RestDetector` took cube_yard from 205 bodies claiming to be awake to 56, against 57 that had actually moved.

**Culling simulation inputs changes the answer.** With 64 gravitational attractors, culling the distant ones by view distance cut the field's share from 280 to 33 KiB/s and multiplied the client's simulation error by 2.4x, because each attractor the client was not told about still pulls on every pellet it simulates.

**Theta has a ceiling.** At `1.2` the black hole example is worse than culling, because a spurious concentration does more damage than a missing force. The crowd version is comfortable at `1.5`, because a drawing does not compound the approximation.

**Diffing against what was sent fails silently under loss.** At 25% loss: 185 corpses a client can never be told about and render error at 73.7 px. Diffing against what was acknowledged put corpses into single digits, render error at 0.5 px and digest mismatches at zero, for roughly three times the bandwidth at that rate.

**Taking the newest set bit rather than the contiguous run** made loss recovery statistically indistinguishable from no recovery at every loss rate.

## Error Handling

Most of this crate returns values or `Option` rather than `Result`: an index that names nobody answers `None` and a query with no hits returns an empty set.

Two places refuse instead of degrading:

*   **A subscription over its limit is refused** rather than truncated. Silently dropping an entry to fit leaves a client in a party it cannot fully see.
*   **`InputSchedule::submit` drops an input outside the window** and says why: `Submission::TickClosed` for a tick already simulated and `Submission::TooFarAhead` for one too far in the future. `Late` is still accepted and executes on the next tick. The schedule counts the drops in `rejected()` and `rejected_split()`.

`DeltaBaseline` carries the counters that say a stream is degrading rather than failing: `full_rebuilds()` for how often the subscriber needed a full baseline, `unacked()` for packets still in flight and `acked_seq()` for the newest state acknowledged. A digest shows that a mirror is wrong but not what is wrong with it, so in a debug build send the ground truth beside it and compare.
