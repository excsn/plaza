# Usage Guide: plaza_server_utils

How to decide what each client is told: judging a shot against the world the shooter saw, gathering who is nearby, subscribing to entities no radius will return, filling a packet to a budget, summarising what is too far to send exactly and streaming the result so a lost packet recovers.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [Relevance in Ten Lines](#relevance-in-ten-lines)
*   [Judging a Shot Fairly](#judging-a-shot-fairly)
    *   [Recording History](#recording-history)
    *   [Rewinding to the Instant the Client Saw](#rewinding-to-the-instant-the-client-saw)
    *   [Sizing the Buffer](#sizing-the-buffer)
*   [Gathering Who Is Nearby](#gathering-who-is-nearby)
    *   [Indexing the World](#indexing-the-world)
    *   [Querying a Radius](#querying-a-radius)
    *   [Diffing Into Spawn and Despawn Streams](#diffing-into-spawn-and-despawn-streams)
    *   [Three Dimensions](#three-dimensions)
*   [Subscribing Beyond Distance](#subscribing-beyond-distance)
    *   [Creating a Subscription](#creating-a-subscription)
    *   [Unioning Both Channels](#unioning-both-channels)
    *   [Leaving a Group Versus Leaving the World](#leaving-a-group-versus-leaving-the-world)
    *   [Telling the Client Why](#telling-the-client-why)
*   [Filling the Packet](#filling-the-packet)
    *   [Scoring and Fitting a Budget](#scoring-and-fitting-a-budget)
    *   [Not Paying for What Is Asleep](#not-paying-for-what-is-asleep)
*   [Summarising What Is Too Far to Send](#summarising-what-is-too-far-to-send)
    *   [Building the Tree](#building-the-tree)
    *   [Walking It per Viewer](#walking-it-per-viewer)
    *   [Choosing Theta](#choosing-theta)
*   [Streaming a Changing Set Reliably](#streaming-a-changing-set-reliably)
    *   [Sending a Delta](#sending-a-delta)
    *   [Taking an Acknowledgement](#taking-an-acknowledgement)
    *   [Choosing a Recovery Policy](#choosing-a-recovery-policy)
*   [Seating Players](#seating-players)
    *   [Admitting and Departing](#admitting-and-departing)
    *   [Waitlists and Displacement](#waitlists-and-displacement)
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
*   **`VisibilitySet`**: a dense bitset of who is visible to one client, with a word-at-a-time diff that is the spawn and despawn stream.
*   **Priority**: which of the relevant entities fit this packet, ranked by a score that grows each tick an entity waits.
*   **At rest**: a run of quiet ticks. Sending it costs one bit where a velocity costs thirty-three.
*   **Aggregate**: a stand-in for a distant group at its weighted centroid, for entities a client computes with rather than only draws.
*   **Baseline**: what a subscriber has acknowledged, which is what a delta is diffed against.
*   **Digest**: an order-independent summary both ends compute, so a diverged mirror is detectable.
*   **`SlotKey`**: an index plus a generation, from `plaza_client_utils`, which both ends of a delta stream name entities by.
*   **Seat**: a bounded position in a room, where a fresh occupant must not inherit the last one's state.

## Quick Start

### Relevance in Ten Lines

```rust,ignore
use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid, VisibilitySet};

let mut grid = SpatialGrid::new(GridQuantizer::new((0.0, 0.0), CELL));
let mut near = Vec::new();
let mut visible = VisibilitySet::new(MAX_ENTITIES);

// Once a tick, over everything.
grid.clear();
for e in &entities {
  grid.insert(e.id, e.x, e.z);
}

// Once per client.
grid.query_radius(eye.x, eye.z, VIEW, &mut near);
let (entered, left) = visible.diff(near.iter().copied());
send(Frame { entered, left });
```

## Judging a Shot Fairly

Clients render other entities slightly in the past, so a player aims at a target's past position. To judge a shot fairly the server rewinds to the instant the client saw.

### Recording History

```rust,ignore
use plaza_server_utils::HistoricalStateBuffer;

let mut history: HistoricalStateBuffer<EntityId, u64, EntityState> = HistoricalStateBuffer::new(64);

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
let mut visible = VisibilitySet::new(index_space);

let (entered, left) = visible.diff(near.iter().copied());
// entered = new & !old, left = old & !new, a word at a time
```

`visible.digest()` computes the same value `plaza_client_utils::SetDigest` does over the client's membership, so both ends can check they still agree.

### Three Dimensions

`SpatialGrid` indexes two axes, which is right for a plane and for most 3D games, since a landscape is locally 2.5D. In open volume it over-returns rather than missing: a query on `(x, z)` answers with a disc where a sphere was asked for.

```rust,ignore
grid.query_radius(eye.x, eye.z, VIEW, &mut candidates);
candidates.retain(|id| (position_of(*id).y - eye.y).abs() <= VIEW);
```

The height filter is exact at the same query cost. It stops being free when entities **stack**: in a tower sharing one footprint, a flat cell holds every floor at once and most of what it returns is discarded. When entities stack, index the third axis instead.

## Subscribing Beyond Distance

`relevance` answers who is near a client. A party health bar, a raid frame through a wall, a spectator following one player and a guild roster need entities the client chose, which no radius expresses.

### Creating a Subscription

```rust,ignore
use plaza_server_utils::subscription::{Subscriptions, Audience, Because};

let mut subs: Subscriptions<Seat> = Subscriptions::new();

subs.subscribe(spectator, player);   // directed: the player does not follow back
subs.pair(a, b);                     // symmetric
subs.group(party_of_a, party_of_b);  // merges two groups whole
```

Subscriptions are indexed **both ways round**, because both directions are queried every tick: a sender needs the set it must include and a departing key needs everyone who has to be told.

### Unioning Both Channels

```rust,ignore
grid.query_radius(eye.x, eye.z, VIEW, &mut near);
let audience = Audience::of(&near, &subs, viewer);

for key in &audience.seats {
  frame.push(entry_for(*key));
}
let second_channel_cost = audience.added;   // only what distance missed
```

The entries come back sorted, so they can be diffed between ticks.

### Leaving a Group Versus Leaving the World

These are different events. Treat them alike and a health bar keeps updating for somebody who has gone.

```rust,ignore
subs.leave_group(player);            // left the party, kept their spectators
let watchers = subs.remove(player);  // left the world; tell each of these
```

Both dissolve a group of one rather than keeping it.

### Telling the Client Why

```rust,ignore
let because = match (near.contains(&key), subs.of(viewer).any(|m| m == key)) {
  (true, true) => Because::Either,
  (true, false) => Because::Near,
  (false, _) => Because::Subscribed,
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
for id in &audience.seats {
  priority.gain(*id, rate_for(*id));
}

// Then fit the budget. Take the cost per entity from your encoding rather than an estimate.
let chosen = priority.fill(budget_bytes, |id| encoded_size(id));
for id in &chosen {
  frame.push(entry_for(*id));
}
```

Whatever did not fit **keeps its accumulated score**, so it ranks higher next tick. The walk continues past an entity too large to fit, so one big one near the front cannot leave the rest of the packet empty. Ties break by index, so a server and a replay of it agree.

### Not Paying for What Is Asleep

In a settled scene most things are not moving. Marking an entity at rest costs one bit, where a velocity costs thirty-three.

```rust,ignore
use plaza_server_utils::rest::RestDetector;

let mut rest = RestDetector::new(index_space);

rest.observe(id, moving);            // your own test for "moving"
if rest.at_rest(id) {
  frame.push_at_rest(id);            // one bit
} else {
  frame.push_with_velocity(id, v);
}
```

An entity is at rest after a **run** of quiet ticks and wakes on the first moving tick. A single quiet tick is not enough, since a body at the top of its arc has zero velocity and is about to fall. Waking late shows on screen, while resting late only costs bandwidth.

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

let mut baseline: DeltaBaseline<SubscriberId> = DeltaBaseline::new();

let plan = baseline.plan(subscriber, &current_keys);
send(Delta {
  seq: plan.seq,
  full_baseline: plan.is_full,
  entered: plan.entered,
  left: plan.left,
  digest: plan.digest,
});
```

`DeltaBaseline` diffs against **what the client acknowledged** instead. It owns the per-subscriber baselines, the acknowledgement frontier, the staleness rebuild and the digest drift check and does not interpret the keys.

### Taking an Acknowledgement

```rust,ignore
baseline.acknowledge(subscriber, ack.newest, ack.mask);
```

The baseline is the newest **contiguous** acknowledgement rather than the newest bit set, because receiving packet N+1 after losing N does not put a client in the state N+1 implies.

The keys carry generations, which loss recovery depends on: a retraction re-derived after a slot was recycled would otherwise name the slot's current occupant, so the client's lookup misses and the entity it actually holds is never mentioned again.

### Choosing a Recovery Policy

```rust,ignore
baseline.with_policy(RecoveryPolicy::Acked);   // diff against what was acknowledged
baseline.with_policy(RecoveryPolicy::Naive);   // diff against what was last sent
```

`Naive` keeps the broken behaviour available so the failure can be demonstrated. Either policy needs a cold-start rule, because there is no acknowledged state before the first acknowledgement.

The client's half is `plaza_client_utils::DeltaMirror`.

## Seating Players

### Admitting and Departing

```rust,ignore
use plaza_server_utils::{Roster, Admission, Departure};

let mut roster: Roster<PlayerId> = Roster::new(MAX_SEATS);

match roster.admit(player) {
  Admission::Seated { seat, .. } => world.spawn(seat),
  Admission::Turned { .. } => refuse(player),
}

if let Departure::Freed { seat } = roster.depart(&player) {
  world.remove(seat);
}
```

A fresh occupant must not inherit the last one's accumulated state, which is what `SeatTable` and `Seating` underneath guarantee.

### Waitlists and Displacement

`Roster` is composed of `SeatSlots` and `RankedQueue`, both public, so the policies compose: a lock for games that seat only between rounds, a ranked waitlist, displacement where a bot holds a seat only until a person wants one, seats held across an absence and bot-driven empties.

## Scheduling Input

```rust,ignore
use plaza_server_utils::input_schedule::InputSchedule;

let mut schedule = InputSchedule::new(depth);
schedule.submit(player, tick, input);
let due = schedule.take(tick);
```

Rejection diagnostics say why an input was refused rather than dropping it silently.

## Delivering a One-Shot Op

An op with nothing behind it, a `Welcome` or a `Refused`, is lost on a lossy link, because nothing in the protocol sends it again.

```rust,ignore
use plaza_server_utils::oneshot::Pending;

let mut pending: Pending<PlayerId, Op> = Pending::new();

pending.push(player, Op::Welcome { seat });
for (player, op) in pending.due(now) {
  send_to(player, op);
}
pending.acknowledge(player, seq);
```

## Putting Numbers on Screen

### Measuring a Rate

```rust,ignore
use plaza_server_utils::meter::RateMeter;

let mut meter = RateMeter::new(Duration::from_secs(2));
meter.record(bytes_sent, now);

hud.line(format!("{:.1} KiB/s", meter.per_sec(now) / 1024.0));
```

Use the windowed `per_sec` rather than `lifetime_per_sec`: a session average keeps climbing toward the current rate without reaching it.

### Measuring How Wrong a Client Was

```rust,ignore
use plaza_server_utils::render_error::render_error_at;

let err = render_error_at(&history, entity, client_render_time, drawn_position);
```

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
*   **`InputSchedule` reports why an input was rejected**, rather than dropping it, so a client that is early, late or over its allowance can be told which.

`DeltaBaseline` carries the counters that say a stream is degrading rather than failing: sequence gaps, staleness rebuilds and digest drift. A digest shows that a mirror is wrong but not what is wrong with it, so in a debug build send the ground truth beside it and compare.
