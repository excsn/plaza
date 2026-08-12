# Usage Guide: plaza_client_utils

How to build the client half of a networked game with `plaza_client_utils`: predicting your own entity, drawing everyone else, keeping a render clock, holding a streamed entity set, and measuring the link.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [A Predicting Client](#a-predicting-client)
    *   [A Rollback Peer](#a-rollback-peer)
*   [Choosing Your Pieces](#choosing-your-pieces)
    *   [Which Predictor](#which-predictor)
    *   [Drawing an Entity You Do Not Control](#drawing-an-entity-you-do-not-control)
*   [Your Own Entity](#your-own-entity)
    *   [Creating the Predictor](#creating-the-predictor)
    *   [Sending an Input](#sending-an-input)
    *   [Reconciling a Packet](#reconciling-a-packet)
    *   [Advancing and Drawing](#advancing-and-drawing)
    *   [Pausing, Freezing and Teleporting](#pausing-freezing-and-teleporting)
    *   [Sending Inputs Only When They Change](#sending-inputs-only-when-they-change)
*   [Everyone Else](#everyone-else)
    *   [Pushing Snapshots and Rendering](#pushing-snapshots-and-rendering)
    *   [Low Send Rates](#low-send-rates)
    *   [Running an Entity's Own Rule](#running-an-entitys-own-rule)
*   [The Render Clock](#the-render-clock)
    *   [Driving the Target](#driving-the-target)
    *   [Keeping It Aligned](#keeping-it-aligned)
    *   [Which Clock Drives What](#which-clock-drives-what)
*   [Corrections](#corrections)
    *   [Smoothing What You Draw](#smoothing-what-you-draw)
    *   [Decaying Big Errors Faster](#decaying-big-errors-faster)
    *   [Knowing Whether a Correction Was Abnormal](#knowing-whether-a-correction-was-abnormal)
*   [Fixed Steps and Periods](#fixed-steps-and-periods)
*   [Streamed Entity Sets](#streamed-entity-sets)
    *   [Applying a Delta Packet](#applying-a-delta-packet)
    *   [Allocating Keys](#allocating-keys)
    *   [Diagnosing a Divergence](#diagnosing-a-divergence)
*   [Surviving a Resume](#surviving-a-resume)
    *   [The Playout Queue](#the-playout-queue)
    *   [The Resume Contract](#the-resume-contract)
*   [Measuring the Link](#measuring-the-link)
    *   [Round Trip and Server Time](#round-trip-and-server-time)
    *   [What Render Delay This Stream Needs](#what-render-delay-this-stream-needs)
    *   [Acknowledging What Arrived](#acknowledging-what-arrived)
*   [Deterministic Arithmetic](#deterministic-arithmetic)
*   [Testing Without a Network](#testing-without-a-network)
*   [Four Principles](#four-principles)
*   [What the Measurements Settled](#what-the-measurements-settled)

## Core Concepts

*   **Authoritative state**: what the server said, and the only thing a rule both sides run may read. Older than what you draw, and correct.
*   **Predicted state**: your own entity simulated ahead of the server so input feels instant. Presentation only.
*   **Reconciliation**: folding an authoritative packet into the predicted state. `PredictedPlayer` replays the inputs the server had not seen; `HeldInputPredictor` eases toward the sample instead.
*   **Sequence number**: `SequenceNumber`, one per input you send. The server echoes the last one it processed, which is what says how much to replay.
*   **Render target**: the single instant `T` a frame is drawn at, produced by `InterpolationClock::target`. Everything a frame reads is evaluated at `T`.
*   **Render delay**: how far behind estimated server time `T` sits. Large enough that two snapshots bracket it, small enough not to feel laggy.
*   **Snapshot**: one timestamped state for an entity you do not control, pushed into a `RemoteView` or a `SnapshotBuffer`.
*   **Correction**: what a reconciliation did, returned as `Correction { seen, settled }` for `CorrectionMonitor` to judge.
*   **Mirror**: the client's copy of a server-streamed entity set, held by `DeltaMirror` and checked against the server's `SetDigest`.
*   **`SlotKey`**: an index plus a generation, the key both ends of that stream name entities by.
*   **Epoch**: a `Timeline` generation. A probe answered in a later epoch is discarded rather than recorded.

## Quick Start

### A Predicting Client

The whole client-side job, against a server that consumes one input per step.

```rust,ignore
use plaza_client_utils::{PredictedPlayer, PlayerConfig, RemoteView, RenderOpts, InterpolationClock};
use std::collections::HashMap;

// The rule the server runs. The same function, not a copy of it.
fn apply_move(state: &mut Pos, input: &Move, _ctx: &()) {
  state.x += input.dx * SPEED;
  state.y += input.dy * SPEED;
}
fn lerp_pos(a: &Pos, b: &Pos, t: f32) -> Pos {
  Pos { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
}

let mut me = PredictedPlayer::new(start, PlayerConfig::default(), apply_move, lerp_pos);
let mut others: HashMap<Id, RemoteView<Pos, Vel>> = HashMap::new();
let mut clock = InterpolationClock::new(100); // render 100ms behind the server

// On local input: predict now, send the numbered input.
let seq = me.input(mv);
send(SequencedClientInput { sequence_number: seq, input_data: mv });

// On a packet: reconcile yourself, push the others, start the clock.
me.reconcile(packet.authoritative_player_state, packet.last_processed_input_seq);
clock.observe(packet.server_time);
for e in packet.entities {
  others.entry(e.id)
    .or_insert_with(|| RemoteView::new(12, 500))
    .push(packet.server_time, e.state, e.velocity);
}

// Each frame: advance everything, then draw at one instant.
me.advance(dt_secs);
clock.advance(dt_ms);
draw(&me.render());
for view in others.values() {
  if let Some(state) = view.render(clock.target(), RenderOpts::default()) {
    draw(&state);
  }
}
```

### A Rollback Peer

No server. Peers exchange inputs and run the same deterministic step.

```rust,ignore
use plaza_client_utils::rollback::{RollbackSession, RollbackConfig};

// Deterministic, identical on every peer.
fn step(world: &World, inputs: &[Stick]) -> World {
  let mut next = world.clone();
  for (i, stick) in inputs.iter().enumerate() {
    next.players[i].advance(*stick);
  }
  next
}

let mut session = RollbackSession::new(
  World::start(),
  vec![Stick::neutral(), Stick::neutral()],  // two players
  RollbackConfig::default(),
  step,
);

// Each frame: your input is known, the remote's may not be.
session.queue_local_input(LOCAL, read_stick());
send_to_peer(session.current_frame(), read_stick());
session.advance_frame();
draw(session.state());

// When a remote input lands, for this frame or an earlier one.
session.confirm_remote_input(REMOTE, frame, stick);

// Both peers agree on a fully-confirmed frame.
debug_assert_eq!(session.state_at(old_frame), peer_reported_state);
```

## Choosing Your Pieces

### Which Predictor

The two bundles differ by how the **server** consumes input, not by how the client feels. Choosing wrong is silent: it shows up as a prediction that is always slightly behind.

| the server | use |
|---|---|
| consumes one input per simulation step | `PredictedPlayer` (replay unacknowledged inputs) |
| holds an input and integrates it every tick | `HeldInputPredictor` (dead reckon and ease) |

Replaying inputs against a server of the second kind double counts, and gets worse the more you economise on bandwidth, because one coalesced input can cover a long stretch of simulation.

### Drawing an Entity You Do Not Control

Four options. The choice is a property of **the entity**, not of the game, and different entities in one scene legitimately sit on different rows.

| you know | draw it by | piece |
|---|---|---|
| its rule, and the inputs the rule reads | running that rule locally, corrected by samples | `HeldInputPredictor` |
| nothing but its past positions | interpolating between two real samples, in the past | `RemoteView` with `interpolate` |
| its positions, and that its motion is constrained | dead reckoning along the last velocity, briefly | `RemoteView` with `extrapolate` |
| none of the above | holding the newest sample | `RemoteView`, both off |

Take the highest row you have the data for. Dead reckoning a **player** is guessing at a human's intention, which nothing on the wire carries, so it overshoots every direction change; it is for entities with inertia and a turning limit.

## Your Own Entity

### Creating the Predictor

*   **`PredictedPlayer`**, for a server consuming one input per step:

    ```rust,ignore
    let config = PlayerConfig {
      input_buffer: 256,      // inputs retained for replay
      smoothing_secs: 0.1,    // ease a correction over this long; 0.0 snaps
      easing: plaza_client_utils::smoothing::ease_out_cubic,
      ..Default::default()
    };
    let mut me = PredictedPlayer::new(start, config, apply_move, lerp_pos);
    ```

*   **`HeldInputPredictor`**, for a server integrating a held input:

    ```rust,ignore
    let mut me = HeldInputPredictor::new(
      start,
      HeldInputConfig { blend: 0.25 },   // fraction of the gap closed per packet
      advance_move,                       // fn(&mut State, &Input, dt_secs, &Ctx)
      lerp_pos,
    )
    .with_teleport(|a, b| a.distance_to(b), 400.0);  // beyond 400 units, snap
    ```

### Sending an Input

```rust,ignore
let seq = me.input(mv);
send(SequencedClientInput { sequence_number: seq, input_data: mv });
```

`input` predicts locally and buffers for replay in one call. Under `HeldInputPredictor` there is nothing to replay, so you hold instead:

```rust,ignore
me.hold(mv);
send(mv);
```

### Reconciling a Packet

```rust,ignore
let correction = me.reconcile(packet.authoritative_player_state, packet.last_processed_input_seq);
```

Under `HeldInputPredictor` the second argument is the sample's **age**, not a sequence, because the server's state is one one-way delay old:

```rust,ignore
let age_secs = (timeline.server_time_ms(now_ms) - packet.server_time) as f32 / 1000.0;
let correction = me.reconcile(packet.state, age_secs);
```

### Advancing and Drawing

```rust,ignore
me.advance(dt_secs);          // integrate, then progress the ease
draw(&me.render());           // eased: what the eye should see
let exact = me.logical();     // exact: what the next prediction builds on
```

Never feed `render()` back into a rule. It is presentation.

### Pausing, Freezing and Teleporting

*   **The server is holding your entity still** (a respawn delay, a stun, a cutscene). Stop integrating, or you invent a correction stream of your own making:

    ```rust,ignore
    me.set_active(false);
    // ... later
    me.set_active(true);
    ```

*   **A discontinuity** (a spawn, a respawn, a warp). Snap, never ease:

    ```rust,ignore
    me.teleport(spawn_point);
    ```

*   **The rule needs the world to run** (gravity, wind, a moving platform):

    ```rust,ignore
    me.set_context(WorldCtx { gravity, platforms });
    ```

### Sending Inputs Only When They Change

Pairs with `HeldInputPredictor`, never with `PredictedPlayer`.

```rust,ignore
let mut coalescer = InputCoalescer::new(200);  // keepalive every 200ms
if coalescer.should_send(&mv, now_ms) {
  send(mv);
}
```

The keepalive is the content, not a fallback: a *dropped* direction change is not a missing update but a wrong state that persists, because the server keeps applying the last direction it received.

## Everyone Else

### Pushing Snapshots and Rendering

```rust,ignore
let mut view = RemoteView::new(12, 500);  // 12 snapshots, coast at most 500ms

view.push(packet.server_time, state, velocity);

if let Some(state) = view.render(clock.target(), RenderOpts::default()) {
  draw(&state);
}
```

`RenderOpts { interpolate, extrapolate }` both default on. Turn them off to compare on screen:

```rust,ignore
let raw = RenderOpts { interpolate: false, extrapolate: false };
```

Watch `view.over_extrapolations()`: climbing steadily means the render target is computed *ahead* of the newest sample rather than trailing it, so the entity is dead reckoned every frame and never interpolated. Fix the clock, not the cap.

### Low Send Rates

Below roughly 20 snapshots a second a straight line between samples visibly corners. `HermiteView` leaves each sample along its recorded velocity:

```rust,ignore
let mut view = HermiteView::new(8);
view.push(packet.server_time, state, velocity);
if let Some(state) = view.render(target_ms) { draw(&state); }
```

Only for motion that is smooth between samples. A straight line cannot leave the segment its two samples bracket; a spline can, whenever the recorded velocity mispredicts the path.

### Running an Entity's Own Rule

`HeldInputPredictor` is not only for the entity you control. Hold an entity's *intent* and it becomes a locally simulated remote, which is the top row of the table above:

```rust,ignore
let mut enemy = HeldInputPredictor::new(start, HeldInputConfig::default(), chase, lerp_pos);
enemy.set_context(WorldCtx { player_positions });
enemy.hold(Intent { target: player_id });

// Each frame, and only a sample now and then from the server.
enemy.advance(dt_secs);
enemy.reconcile(sample.state, sample_age_secs);
```

## The Render Clock

### Driving the Target

```rust,ignore
let mut clock = InterpolationClock::new(100);  // ms behind estimated server time

clock.observe(packet.server_time);  // first call starts it; later calls ignored
clock.advance(dt_ms);               // once a frame

let target = clock.target();        // None before the first observe
```

Every entity in a frame is rendered at that one target. Two entities drawn at two instants is a seam.

### Keeping It Aligned

Free-running drifts as latency changes. Pick **one** of these, never both:

*   **Position steering**, a small nudge per packet:

    ```rust,ignore
    clock.resync(newest_server_time_ms, 0.1);   // 1.0 snaps, 0.1 is smooth
    ```

*   **Rate steering**, gliding into alignment by running slightly fast or slow:

    ```rust,ignore
    clock.observe_rate(newest_server_time_ms, 0.1);  // at most +/-10% off real time
    clock.advance_scaled(dt_ms);                     // instead of advance
    let dilation = clock.playback_rate();            // for a readout
    ```

Size the delay from measurement rather than a guess. See [What Render Delay This Stream Needs](#what-render-delay-this-stream-needs).

### Which Clock Drives What

Every piece here takes a `dt` or a timestamp and is deliberately clock-agnostic, which is how a local pause works.

*   **Wall-clock time** drives anything about the network: `InterpolationClock`, `RttEstimator`, the `ErrorSmoother` ease. Network delay does not stop when your menu opens.
*   **Game time** drives the simulation: `apply`, prediction. To pause locally, feed `dt = 0` to the game step and real `dt` to everything above.

In an authoritative game the shared world keeps ticking, so a pause is a local overlay over a world that moves on.

## Corrections

### Smoothing What You Draw

`PredictedPlayer` holds one internally. Reach for `ErrorSmoother` directly for any other entity that jumps:

```rust,ignore
let mut smoother = ErrorSmoother::new(0.1).with_easing(smoothing::ease_out_cubic);

let drawn_before = smoother.sample(&logical, lerp_pos);
logical = authoritative;              // the jump
smoother.begin_from(drawn_before);    // ease from where the eye was

// each frame
smoother.advance(dt_secs);
draw(&smoother.sample(&logical, lerp_pos));
```

Keep the duration **shorter than your send interval**, or corrections arrive faster than the ease finishes and the smoother itself becomes the dominant error. Past that point, shed a fraction per frame instead:

```rust,ignore
let mut smoother = ErrorSmoother::at_rate(0.85);
```

Snap rather than ease on a discontinuity. The distinction is by **cause**, not magnitude: ease continuous error, snap a spawn or a warp.

```rust,ignore
if correction_distance > DESYNC {
  smoother.reset();
}
```

### Decaying Big Errors Faster

A fixed duration makes a large error and a small one take the same time, which is backwards: a small offset can afford to linger, a large one is already visible.

```rust,ignore
let decay = AdaptiveDecay::default();  // keep 0.95/frame under 0.25 units, 0.85 over 1.0
offset = offset * decay.retain(offset.length(), dt_secs);
draw(&(logical + offset));
```

It is the rate, not the state: keep your own offset. Framerate-independent, so a 30fps client and a 144fps one shed the same error over the same wall time.

### Knowing Whether a Correction Was Abnormal

There is no fixed normal. Thirty pixels is unremarkable at one send rate and alarming at another.

```rust,ignore
let mut monitor = CorrectionMonitor::new().with_warmup(64);

let correction = me.reconcile(state, acked);
let distance = correction.seen.distance_to(&correction.settled);
if monitor.record(distance) {
  warn!(distance, threshold = monitor.threshold(), "abnormal correction");
}
```

`with_warmup` matters: a baseline initialised to zero calls every early correction enormous, so an unwarmed monitor alarms loudest at startup when it knows least.

## Fixed Steps and Periods

Both sides stepping the same rule at different step sizes are not running the same simulation, and the drift reads as network jitter.

```rust,ignore
let mut ticker = FixedTimestep::from_step_ms(16).with_max_frame_ms(250);

for step_ms in ticker.advance(elapsed_ms) {
  world.step(step_ms as f32 / 1000.0);   // the yielded duration, never the frame delta
}
let blend = ticker.alpha();               // for interpolating a render between two states
```

`from_hz` is integer division, so a rate that does not divide 1000 truncates: 60 Hz is a 16 ms step running 62.5 times a second, which does **not** match a server on `plaza::TickDriver::from_hz` at an exact 16.667 ms. Pick a rate that divides 1000 when both sides matter.

Watch `ticker.dropped_ms()`: real time the simulation never ran because the cap refused it.

For work that is idempotent and only needs doing now and then, `Periodic` asks "is it time yet" instead:

```rust,ignore
let mut heartbeat = Periodic::new(1000);
if heartbeat.due(elapsed_ms) {
  send_keepalive();
}
```

## Streamed Entity Sets

### Applying a Delta Packet

```rust,ignore
let mut mirror: DeltaMirror<Enemy> = DeltaMirror::new();

mirror.begin(packet.seq, packet.full_baseline);
for e in packet.entered {
  mirror.insert(e.key, Enemy::new(e.state));
}
for key in packet.left {
  mirror.remove(key);
}
let agreement = mirror.settle(packet.digest);
if !agreement.agreed() {
  warn!("mirror diverged");
}
```

**Apply every packet, whatever baseline it names.** These deltas carry absolute values, so applying them is idempotent and applying a superset is harmless, while discarding what you cannot rebase starves the mirror.

### Allocating Keys

The server hands out `SlotKey`s; a client that allocates its own uses the same type so both ends key alike.

```rust,ignore
let mut slots = SlotAllocator::with_capacity(1024).with_policy(ReusePolicy::Fifo);

let key = slots.alloc();
storage[key.index() as usize] = entity;   // storage is yours, sized by index_space()
slots.free(key);                          // the generation bumps here, not on alloc
```

### Diagnosing a Divergence

A digest detects and cannot diagnose, so ship the ground truth beside it in a debug build:

```rust,ignore
let d = mirror.divergence_from(&packet.all_keys);
error!(missing = ?d.missing, extra = ?d.extra, "mirror divergence");
```

`missing` means something was lost or never sent. `extra` means a removal never landed.

Read the three counters separately. `frames_lost()` is the wire, `stale_refs()` is a message naming an occupant you no longer hold, `divergences()` is the symptom neither predicts.

## Surviving a Resume

### The Playout Queue

A client that renders in the past queues packets and plays each out when the render clock reaches the instant it describes.

```rust,ignore
let mut playout: PlayoutBuffer<Packet> = PlayoutBuffer::new(256, 2000);

match playout.push(packet.server_time, packet.seq, packet, clock.target()) {
  Admission::Queued => {}
  Admission::TimelineLost => {
    mirror.clear();
    clock = InterpolationClock::new(delay_ms);
    timeline.on_resume();
  }
}

while let Some(packet) = playout.pop_due(render_at) {
  apply(packet);
}
```

`underruns()` says the render delay is too small for this link. `restarts()` counts stalls survived, one per discontinuity however large the backlog.

### The Resume Contract

A browser tab backgrounds, a laptop sleeps, a frame loop stalls. The socket keeps receiving, so a resumed client faces a *lump*: minutes of packets describing moments it can never play.

One invariant makes recovery work, and each half lives in a different crate: **a client may discard any stretch of the stream unread, provided it also drops the state derived from it, because an acknowledgement carrying the digest of nothing obligates the server to answer with a full baseline.** There is no resync request message anywhere; dropping the mirror *is* the request.

Three layers each own a verdict:

*   **Transport**: discards the backlog before parsing it (`plaza_ws::trim_backlog`).
*   **Playout queue**: treats the gap as a discontinuity and restarts once, keeping the newest packet.
*   **Server**: stops streaming to a subscriber that has provably stopped reading (`DeltaBaseline::with_flow`).

What is left to you is one thing: on `Admission::TimelineLost`, drop the mirror and re-anchor the render clock on what just arrived.

## Measuring the Link

### Round Trip and Server Time

```rust,ignore
let mut timeline = Timeline::new();

// Sending a probe.
let probe = timeline.begin(now);
send_ping(probe.sent_at);

// Its answer.
timeline.complete(probe, now, pong.responder);

// Any message the server stamped.
timeline.note_stamp(packet.server_time, now_ms);

let server_now = timeline.server_time_ms(now_ms);
let rtt = timeline.rtt.rtt();
let jitter = timeline.rtt.jitter();
```

Call `timeline.on_reconnect()` when the socket changes and `on_resume()` when wall time jumped. A probe answered in a later epoch is discarded rather than recorded, because a probe sent before a suspend and answered after it measures the suspend.

**Every estimator here is unit-agnostic.** Feed milliseconds and read milliseconds. `ClockSyncEstimator` compares your clock against someone else's, so both ends disagreeing about the unit produces a confident wrong answer.

### What Render Delay This Stream Needs

Nothing tells a real client the send rate or the delay its buffer must cover, so measure. Keep one monitor per interpolated stream.

```rust,ignore
let mut arrival = ArrivalMonitor::new(0.05);

arrival.observe(packet.server_time, timeline.server_time_ms(now_ms));

if arrival.warmed_up() && arrival.needed_delay_ms() > clock.delay() as f32 {
  warn!(needed = arrival.needed_delay_ms(), in_force = clock.delay(), "render delay is short");
}
```

Whether to *adapt* is yours: a delay that follows the link hides bad links instead of reporting them.

### Acknowledging What Arrived

```rust,ignore
let mut acks = AckWindow::new();
acks.observe(packet.seq);
if let Some((newest, mask)) = acks.encode() {
  send(Ack { newest, mask });    // twelve bytes, whatever the loss rate
}
```

**Which answer you want depends on what your protocol does with it, and getting it wrong is silent.** A protocol that **retransmits** wants the mask:

```rust,ignore
for seq in acks.missing_since(oldest_held) {
  resend(seq);
}
```

A protocol that **re-derives**, such as a delta stream diffing against a state the peer provably reached, wants the contiguous run:

```rust,ignore
if let Some(base) = acks.contiguous_base(first_seq) {
  diff_against(base);
}
```

Receiving packet N+1 after losing N does not put a peer in the state N+1 implies: whatever N announced and N+1 had no reason to repeat is gone.

## Deterministic Arithmetic

For a wire that carries causes rather than state, where nothing is ever corrected and `f32` cannot be relied on to match between a wasm build and a native one.

```rust,ignore
use plaza_client_utils::fixed::{Fx, P};

let speed = Fx::ratio(3, 2);              // 1.5
let pos = P::from_ints(10, 4);
let next = P { x: pos.x + speed, y: pos.y };

if next.dist_sq(&target) < RANGE_SQ {     // no square root on the path
  hit();
}

draw(next.x.to_f32(), next.y.to_f32());   // the only float, one way, for the renderer
```

Nothing in a simulation may call `to_f32`.

## Testing Without a Network

```rust,ignore
use plaza_client_utils::net_sim::{LatencyLink, Ordering, Rng};

let mut link = LatencyLink::new().with_ordering(Ordering::Ordered);
let mut rng = Rng::new(42);

link.send(now_ms, packet, 80, 15, 2.0, &mut rng);   // 80ms, 15ms jitter, 2% loss
for packet in link.drain_due(now_ms) {
  client.apply(packet);
}
```

`Ordering::Ordered` is the default because that is what TCP and WebSocket are. An unclamped queue reorders under jitter and manufactures a failure mode the real transport cannot produce.

## Four Principles

None is enforceable by a type. They *prevent* bugs, where everything else here only recovers from them.

**A shared rule must be shared code, not code written twice.** The `apply` you hand a predictor is meant to *be* the server's step function. Anything the server does that your copy leaves out arrives as a permanent correction: it looks like network jitter, it is largest exactly when it is most visible, and it is expensive to find later. If your rule needs the world to run, that is what `set_context` is for.

**Prediction is presentation; shared rules consume authoritative state.** Feeding a locally predicted position into a rule that *both* sides run creates a second, divergent world, and every packet then fights the local one. Prediction drives the camera and your own marker. The rules both sides run read `logical()` or the authoritative state, even though it is older.

**One instant per frame.** Pick a single `T` and evaluate everything at it: not only where entities are drawn, but everything a behaviour rule reads while producing the frame, aim targets and chase context included. An entity simulated to `T` while reading a target from the newest packet is two timelines in one scene.

**The timeline comes from declaration, not arrival.** Round trips and jitter and arrival times may size buffers and admit or refuse connections. They never decide which moment is on screen or when an input executes. A render clock steered by packet arrival hides bad links, lets every client pick a different "now", and quietly makes ping an input to the game.

## What the Measurements Settled

**A blend fraction beats an ease duration for a held-input predictor.** A fixed-duration ease has a correction rate above which it never finishes. In `horde_playground` that made locally simulated enemies get *worse* as the send rate rose, 10, 16 then 20 px at 4, 10 and 30 Hz. On `blend` the same entities sit at 9 to 10 px at every rate.

**Ease continuously rather than past a threshold.** Correcting only once the error crosses a threshold and then closing the whole gap produces a metronomic sawtooth: a small jump forward roughly every four hundred milliseconds, at every latency including zero, which a player feels as a rhythmic tug.

**The ease-versus-rate crossover.** Worst error 2.67 at one correction every 0.5 s, 15.00 at one every frame, against 11.33 for `at_rate(0.85)`. Below that crossover the duration wins.

**Running the rule beats interpolating.** Over 3000 enemies in `horde_playground`, by 43 px of mean error at 1 Hz, and it still leads at 30 Hz, because an interpolated entity is always a send interval in the past.

**A spline is worth 484x, or 13x worse.** On a 10-unit circle at 10 Hz, worst error 0.0003 against linear's 0.1231. Across 300 solver-driven cubes at 10 Hz *with impacts*, it left the bracketing segment on half of all frames by up to 2.48 units and came out 13x worse than the chord it replaced.

**Second-order dead reckoning pays only at low send rates.** The correction goes as the gap squared. On a circular path at 10 Hz coasted through a 100 ms gap it cuts the error 45%; at a normal server rate it changes nothing measurable. It begins to pay below about 10 Hz.

**Ack-driven resends against blind redundancy.** In `rollback_playground`, 28% cheaper on a clean link, 45% dearer at 50% loss, crossing over around 12%. Blind redundancy makes a fixed number of attempts; acks retry until acknowledged, and converged at 55% loss where blind did not.

**Discarding unrebaseable deltas starves the mirror.** An earlier `horde_playground` version discarded, and at 25% loss its mirror emptied out while every agreement check read perfect, because the checks only ran over what had been applied.

**Reuse order is a wire decision.** Under `ReusePolicy::Lifo` a burst of 233 despawns was 204 separate runs, mean run length 1.14, which is why run-length encoding lost decisively to delta-varint there.

**When a slot generation earns its keep.** Under ordered delivery with each death announced before the next diff, `horde_playground` recorded zero stale handle references across 413 kills with slots actively recycling. It became load-bearing again the moment loss recovery re-derived a retraction after a slot may have been recycled.
