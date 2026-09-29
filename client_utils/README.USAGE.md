# Usage Guide: plaza_client_utils

How to build the client half of a networked game with `plaza_client_utils`: predicting your own entity, drawing everyone else, keeping a render clock, holding a streamed entity set and measuring the link.

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
    *   [Firing a One-Shot Effect Once](#firing-a-one-shot-effect-once)
    *   [Reconciling a Packet](#reconciling-a-packet)
    *   [Advancing and Drawing](#advancing-and-drawing)
    *   [Pausing, Freezing and Teleporting](#pausing-freezing-and-teleporting)
    *   [Sending Inputs Only When They Change](#sending-inputs-only-when-they-change)
*   [Predicting a Walk by Shared Rule](#predicting-a-walk-by-shared-rule)
    *   [Seating the Body](#seating-the-body)
    *   [Setting Out on a Route](#setting-out-on-a-route)
    *   [Walking and Drawing Each Frame](#walking-and-drawing-each-frame)
    *   [Checking the Server Against the Route](#checking-the-server-against-the-route)
*   [Everyone Else](#everyone-else)
    *   [Pushing Snapshots and Rendering](#pushing-snapshots-and-rendering)
    *   [Low Send Rates](#low-send-rates)
    *   [Running an Entity's Own Rule](#running-an-entitys-own-rule)
    *   [Forgetting What the Server Stopped Mentioning](#forgetting-what-the-server-stopped-mentioning)
*   [The Render Clock](#the-render-clock)
    *   [Driving the Target](#driving-the-target)
    *   [Keeping It Aligned](#keeping-it-aligned)
    *   [Which Clock Drives What](#which-clock-drives-what)
*   [Corrections](#corrections)
    *   [Smoothing What You Draw](#smoothing-what-you-draw)
    *   [Decaying Big Errors Faster](#decaying-big-errors-faster)
    *   [Knowing Whether a Correction Was Abnormal](#knowing-whether-a-correction-was-abnormal)
*   [Fixed Steps and Periods](#fixed-steps-and-periods)
    *   [Stepping a Simulation](#stepping-a-simulation)
    *   [Capping Catch-Up in Steps](#capping-catch-up-in-steps)
    *   [Running Something Now and Then](#running-something-now-and-then)
*   [Streamed Entity Sets](#streamed-entity-sets)
    *   [Applying a Delta Packet](#applying-a-delta-packet)
    *   [Allocating Keys](#allocating-keys)
    *   [Digesting a Set Yourself](#digesting-a-set-yourself)
    *   [Diagnosing a Divergence](#diagnosing-a-divergence)
*   [Surviving a Resume](#surviving-a-resume)
    *   [The Playout Queue](#the-playout-queue)
    *   [The Resume Contract](#the-resume-contract)
*   [Measuring the Link](#measuring-the-link)
    *   [Round Trip and Server Time](#round-trip-and-server-time)
    *   [What Render Delay This Stream Needs](#what-render-delay-this-stream-needs)
    *   [Acknowledging What Arrived](#acknowledging-what-arrived)
    *   [Measuring a Rate](#measuring-a-rate)
*   [Deterministic Arithmetic](#deterministic-arithmetic)
*   [Agreeing on Randomness and State](#agreeing-on-randomness-and-state)
    *   [Drawing Numbers Both Ends Agree On](#drawing-numbers-both-ends-agree-on)
    *   [Generating Terrain From a Seed](#generating-terrain-from-a-seed)
    *   [Checking Two Worlds Match](#checking-two-worlds-match)
*   [Testing Without a Network](#testing-without-a-network)
*   [Four Principles](#four-principles)
*   [What the Measurements Settled](#what-the-measurements-settled)

## Core Concepts

*   **Authoritative state**: what the server said and the only thing a rule both sides run may read. It is older than what you draw, but it is correct.
*   **Predicted state**: your own entity simulated ahead of the server so input feels instant. Presentation only.
*   **Reconciliation**: folding an authoritative packet into the predicted state. `PredictedPlayer` replays the inputs the server had not seen; `HeldInputPredictor` eases toward the sample instead.
*   **Sequence number**: `SequenceNumber`, one per input you send. The server echoes the last one it processed, which is what says how much to replay.
*   **Render target**: the single instant `T` a frame is drawn at, produced by `InterpolationClock::target`. Everything a frame reads is evaluated at `T`.
*   **Render delay**: how far behind estimated server time `T` sits. It must be large enough that two snapshots bracket it and small enough not to feel laggy.
*   **Snapshot**: one timestamped state for an entity you do not control, pushed into a `RemoteView` or a `SnapshotBuffer`.
*   **Correction**: what a reconciliation did, returned as `Correction { seen, settled }` for `CorrectionMonitor` to judge.
*   **Mirror**: the client's copy of a server-streamed entity set, held by `DeltaMirror` and checked against the server's `SetDigest`.
*   **`SlotKey`**: an index plus a generation, the key both ends of that stream name entities by.
*   **Epoch**: a `Timeline` generation. A probe answered in a later epoch is discarded rather than recorded.
*   **Shared rule**: a deterministic function both ends run over the same state, such as a pathfinder. `RoutePredictor` predicts by running it instead of replaying inputs.
*   **One-shot**: something an input causes once (a shot, a footstep), run from `on_first` because `apply` runs again on every replay.
*   **Grace**: how long an entity may go unmentioned before `Silence` treats it as gone, in whatever unit your stamps use.
*   **State digest**: one `u64` summarising a whole simulation state in a fixed field order, compared between two ends to catch a quiet divergence.

## Quick Start

### A Predicting Client

The whole client-side job, against a server that consumes one input per step.

```rust,ignore
use plaza_client_utils::{PredictedPlayer, PlayerConfig, RemoteView, RenderOpts, InterpolationClock};
use std::collections::HashMap;

// The rule the server runs: share this function with the server rather than copying it.
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

Pick between the two bundles by how the **server** consumes input. A wrong choice raises no error; it shows up as a prediction that is always slightly behind.

| the server | use |
|---|---|
| consumes one input per simulation step | `PredictedPlayer` (replay unacknowledged inputs) |
| holds an input and integrates it every tick | `HeldInputPredictor` (dead reckon and ease) |
| answers an op with a deterministic rule the client can run too, such as a pathfinder | `RoutePredictor` (walk the same route locally) |

Replaying inputs against a server of the second kind double counts and gets worse the more you economise on bandwidth, because one coalesced input can cover a long stretch of simulation. The third kind sends one op for a whole journey, so there is nothing per tick to replay or ease; see [Predicting a Walk by Shared Rule](#predicting-a-walk-by-shared-rule).

### Drawing an Entity You Do Not Control

Four options. Choose per **entity** rather than per game: different entities in one scene can sit on different rows.

| you know | draw it by | piece |
|---|---|---|
| its rule and the inputs the rule reads | running that rule locally, corrected by samples | `HeldInputPredictor` |
| nothing but its past positions | interpolating between two real samples, in the past | `RemoteView` with `interpolate` |
| its positions and that its motion is constrained | dead reckoning along the last velocity, briefly | `RemoteView` with `extrapolate` |
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

### Firing a One-Shot Effect Once

`apply` runs on the press and again on every reconcile while the input is unacknowledged. A sound or a muzzle flash inside `apply` repeats once per replay: about ten times per trigger pull at 150 ms round trip and 60 packets a second (`client_utils/examples/replay_refires.rs`). Keep `apply` to what the input does to state and move the effect to `on_first`, which runs only from `input`:

```rust,ignore
fn apply_shot(state: &mut Pos, input: &Shot, _ctx: &()) {
  state.x += input.dx;              // state only: this is replayed
}
fn fire_once(input: &Shot, _ctx: &()) {
  if input.fire {
    play_sound(Sound::Shot);        // runs once per press, never from a replay
  }
}

let mut me = PredictedPlayer::new(start, PlayerConfig::default(), apply_shot, lerp_pos)
  .on_first(fire_once);
```

A predictor you build yourself on `ClientInputBuffer` gets the same split from `unacknowledged_with_pass`, which pairs each input with whether this is the first time it has been handed out:

```rust,ignore
for (buffered, first) in inputs.unacknowledged_with_pass(acked_seq) {
  apply_shot(&mut state, &buffered.op, &ctx);
  if first {
    fire_once(&buffered.op, &ctx);
  }
}
```

The full surface is in [`PredictedPlayer`](API_REFERENCE.md#struct-predictedplayerstate-input-ctx) and [`ClientInputBuffer`](API_REFERENCE.md#struct-clientinputbufferop-predictedstatesnapshot).

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
me.advance(dt_secs);          // PredictedPlayer: progress the ease. HeldInputPredictor: dead reckon one step
draw(&me.render());           // eased: what the eye should see
let exact = me.logical();     // exact: what the next prediction builds on
```

Never feed `render()` back into a rule; it is only for drawing.

### Pausing, Freezing and Teleporting

*   **The server is holding your entity still** (a respawn delay, a stun, a cutscene). Stop integrating; otherwise the client produces corrections of its own:

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

The keepalive is required. The server keeps applying the last direction it received, so a *dropped* direction change leaves a wrong state in place until the keepalive resends the input.

## Predicting a Walk by Shared Rule

`RoutePredictor` is for a game where a click sends one op and the server answers it with a deterministic rule, such as a pathfinder over a map both ends derive. The client runs the same rule, so it knows the whole journey the moment the click happens. Prediction only has to walk the body across its squares on the local clock without jumping. The rule must be shared code over shared state (see [Agreeing on Randomness and State](#agreeing-on-randomness-and-state)). Otherwise every journey diverges.

### Seating the Body

`P` is a square: a tile, a node, whatever the rule routes over. `point` maps it to the plane you draw on.

```rust,ignore
use plaza_client_utils::{Heard, RoutePredictor};

fn tile_point(tile: &Tile) -> [f32; 2] {
  [tile.x as f32, tile.y as f32]
}

let mut body = RoutePredictor::new(Tile::default(), tile_point, TICK_MS);

// The seat assignment, a respawn or a teleport. The only move that jumps.
body.jump_to(you.tile, now_ms);

// Each frame packet: the step length is live, so chase the server's tick.
body.set_step_ms(frame.tick_ms);
```

Nothing walks until `jump_to` has run once; `is_seeded()` says whether it has.

### Setting Out on a Route

Draw the route locally first and send the op second, so the body is already moving before the op leaves the machine:

```rust,ignore
let route = finder.route(body.predicted, Goal::On(tile));   // the server's own pathfinder
body.set_out(route, true, now_ms);
send(Op::WalkTo { tile });
```

The `checkable` flag says whether the server expands the same route. Pass `true` for a walk to a fixed square. Pass `false` when the rule answers differently on each end, such as chasing something that moves:

```rust,ignore
body.set_out(finder.route(body.predicted, Goal::Beside(target_tile)), false, now_ms);
```

A `true` is only honoured when the body is at rest with nothing owed, because a click mid-walk makes each end expand from a different square. Cancelling is an empty route; the crossing in progress finishes and nothing else happens:

```rust,ignore
body.set_out(std::iter::empty(), false, now_ms);
```

### Walking and Drawing Each Frame

```rust,ignore
let steps = if running { 2 } else { 1 };   // squares per tick, as the server's rule defines a run
body.advance(now_ms, steps);
body.settle(now_ms);                       // takes the server's square once the body has stopped

let [x, y] = body.drawn(now_ms);
if let Some([dx, dy]) = body.heading(now_ms) {
  face_towards(dx, dy);
}
let animate_walk = body.walking(now_ms);
```

`settle` is the only reconciliation. There is no per-tick correction, because both ends are walking to the same place and arrive together, so it usually does nothing.

### Checking the Server Against the Route

On each packet, check the server's square against the route this client drew, not against the client's current square. The two ends are a tick out of phase by design.

```rust,ignore
if you.refused == Some(Refusal::NoRoute) {
  body.abandon(you.tile, now_ms);          // keeps the drawn position, drops the plan
}

match body.confirm(you.tile, 2) {          // slack: the server's steps per tick
  Heard::OnRoute | Heard::Unchecked => {}
  Heard::Diverged => notice("the world walked a different way"),
}
```

A divergence means the two ends are no longer running the same rule, which is a bug rather than a network condition. `body.diverged` should read zero; show it beside `body.confirmations` in a debug panel. The full surface is in [`RoutePredictor`](API_REFERENCE.md#struct-routepredictorp-module-route).

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

Watch `view.over_extrapolations()`: climbing steadily means the render target is computed *ahead* of the newest sample rather than trailing it, so the entity is dead reckoned every frame and never interpolated. Fix the clock rather than raising the cap.

### Low Send Rates

Below roughly 20 snapshots a second a straight line between samples visibly corners. `HermiteView` leaves each sample along its recorded velocity:

```rust,ignore
let mut view = HermiteView::new(8);
view.push(packet.server_time, state, velocity);
if let Some(state) = view.render(target_ms) { draw(&state); }
```

Use it only for motion that is smooth between samples. A spline leaves the segment its two samples bracket whenever the recorded velocity mispredicts the path, which a straight line never does.

### Running an Entity's Own Rule

`HeldInputPredictor` also works for an entity you do not control. Hold the entity's *intent* and it is simulated locally, which is the top row of the table above:

```rust,ignore
let mut enemy = HeldInputPredictor::new(start, HeldInputConfig::default(), chase, lerp_pos);
enemy.set_context(WorldCtx { player_positions });
enemy.hold(Intent { target: player_id });

// Each frame and only a sample now and then from the server.
enemy.advance(dt_secs);
enemy.reconcile(sample.state, sample_age_secs);
```

### Forgetting What the Server Stopped Mentioning

A server that filters by relevance sends no despawn. It stops mentioning the entity and no other message will say it is gone. Record the frame each entity was last mentioned on and sweep with a `Silence`:

```rust,ignore
use plaza_client_utils::Silence;

const GRACE: Silence = Silence::new(8);    // frames; panics at zero

for ship in frame.ships {
  ships.insert(ship.seat, Known { state: ship.state, seen: frame.number });
}

let forgotten = GRACE.sweep(&mut ships, frame.number, |seat, known| {
  (Some(*seat) != my_seat).then_some(known.seen)   // None: never forget my own ship
});
```

The closure returns `None` for anything silence must never remove: your own entity or one sent once by design and never mentioned again, such as a spawn-only projectile. `GRACE.keeps(seen, now)` answers the same question for one entity.

A grace of one frame makes entities at the edge of the view radius flicker as both ends drift across it. An entity streamed every frame it exists can use a short grace; one that is sometimes skipped for bandwidth needs a longer one. See [`Silence`](API_REFERENCE.md#struct-silence).

## The Render Clock

### Driving the Target

```rust,ignore
let mut clock = InterpolationClock::new(100);  // ms behind estimated server time

clock.observe(packet.server_time);  // first call starts it; later calls ignored
clock.advance(dt_ms);               // once a frame

let target = clock.target();        // None before the first observe
```

Every entity in a frame is rendered at that one target. Drawing two entities at two different instants puts a seam between them.

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

In an authoritative game the shared world keeps ticking while one client is paused, so a pause only affects that client.

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

Keep the duration **shorter than your send interval**. Otherwise corrections arrive faster than the ease finishes and the smoother itself becomes the dominant error. Past that point, shed a fraction per frame instead:

```rust,ignore
let mut smoother = ErrorSmoother::at_rate(0.85);
```

Snap rather than ease on a discontinuity. Decide by **cause** rather than magnitude: ease continuous error and snap a spawn or a warp.

```rust,ignore
if correction_distance > DESYNC {
  smoother.reset();
}
```

### Decaying Big Errors Faster

A fixed duration makes a large error and a small one take the same time. A small offset can linger unnoticed, but a large one is already visible and should clear sooner.

```rust,ignore
let decay = AdaptiveDecay::default();  // keep 0.95/frame under 0.25 units, 0.85 over 1.0
offset = offset * decay.retain(offset.length(), dt_secs);
draw(&(logical + offset));
```

`AdaptiveDecay` only supplies the rate, so keep your own offset. It is framerate-independent: a 30fps client and a 144fps one shed the same error over the same wall time.

### Knowing Whether a Correction Was Abnormal

What counts as a normal correction changes with conditions: thirty pixels is unremarkable at one send rate and alarming at another.

```rust,ignore
let mut monitor = CorrectionMonitor::new().with_warmup(64);

let correction = me.reconcile(state, acked);
let distance = correction.seen.distance_to(&correction.settled);
if monitor.record(distance) {
  warn!(distance, threshold = monitor.threshold(), "abnormal correction");
}
```

The warmup matters: a baseline initialised to zero calls every early correction enormous, so a monitor without one raises the most alarms at startup, before it has seen enough samples to judge. `CorrectionMonitor::new()` learns from 32 samples before flagging anything; `with_warmup` changes that count.

## Fixed Steps and Periods

Both sides stepping the same rule at different step sizes are not running the same simulation and the drift reads as network jitter.

### Stepping a Simulation

```rust,ignore
let mut ticker = FixedTimestep::from_step_ms(16).with_max_frame_ms(250);

for step in ticker.advance(elapsed_ms) {
  world.step(step.as_secs_f32());   // the yielded Duration, never the frame delta
}
let blend = ticker.alpha();               // for interpolating a render between two states
```

Build it from a `Duration` or a rate when you have one:

```rust,ignore
let ticker = FixedTimestep::from_step(Duration::from_millis(50));
let ticker = FixedTimestep::from_hz(60);
```

`from_hz` is exact to the nanosecond and uses the same expression as `plaza::TickDriver::from_hz`, so 60 Hz is a 16.666667 ms step on both sides. `ticker.step()` reads the step back as a `Duration` and `set_step` changes it live.

Watch `ticker.dropped_ms()`: real time the simulation never ran because the cap refused it.

### Capping Catch-Up in Steps

`with_max_frame_ms` caps how much elapsed time one `advance` is handed. For a slow tick over a fast driver, such as a 600 ms game tick fed by 50 ms wakes, cap in whole steps instead. That cap follows the step length when the step length changes:

```rust,ignore
let mut ticker = FixedTimestep::from_step_ms(TICK_MS)
  .with_max_steps(CATCH_UP)
  .with_max_frame_ms(3_600_000);   // raise the time cap so the steps cap is the policy

ticker.set_step(Duration::from_millis(frame.tick_ms));   // the server changed its tick
```

Time owed past the steps cap goes into `dropped_ms`.

### Running Something Now and Then

For work that is idempotent and only needs doing now and then, `Periodic` asks "is it time yet" instead:

```rust,ignore
let mut heartbeat = Periodic::new(1000);          // or Periodic::from_interval(Duration::from_secs(1))
if heartbeat.due(elapsed_ms) {
  send_keepalive();
}

let owed = heartbeat.advance(elapsed_ms);          // every occurrence, when each one counts
let every = heartbeat.interval();                  // a Duration
```

See [`FixedTimestep` and `Periodic`](API_REFERENCE.md#10-module-timestep) for the full surface.

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

**Apply every packet, whatever baseline it names.** These deltas carry absolute values, so applying them is idempotent and applying a superset is harmless, while discarding what you cannot rebase empties the mirror.

### Allocating Keys

The server hands out `SlotKey`s; a client that allocates its own uses the same type so both ends key alike.

```rust,ignore
let mut slots = SlotAllocator::with_capacity(1024).with_policy(ReusePolicy::Fifo);

let key = slots.alloc();
storage[key.index as usize] = entity;     // storage is yours, sized by index_space()
slots.free(key);                          // the generation bumps here, not on alloc
```

### Digesting a Set Yourself

`DeltaMirror` keeps its own digest. Reach for `SetDigest` directly when you are the side producing one or when you compare a set that is not a mirror. Order does not matter and a key can be added or removed in O(1):

```rust,ignore
use plaza_client_utils::SetDigest;

let mut digest = SetDigest::new();
for enemy in &field.enemies {
  digest.insert(enemy.key.encode());      // index and generation: checks the occupant
}
digest.remove(dead.key.encode());
send(Frame { digest: digest.digest(), .. });

let rebuilt = SetDigest::from_keys(field.enemies.iter().map(|e| e.key.encode()));
```

Hash a bare index to check membership only. Pack index with generation, as `SlotKey::encode` does, to check that both ends hold the same occupant. When the order of the fields matters, as in a whole world, use a [state digest](#checking-two-worlds-match) instead.

### Diagnosing a Divergence

A digest tells you the sets differ but not which keys differ, so in a debug build ship the server's key list beside it:

```rust,ignore
let d = mirror.divergence_from(packet.all_keys.iter().copied());
error!(missing = ?d.missing, extra = ?d.extra, "mirror divergence");
```

`missing` means something was lost or never sent. `extra` means a removal never landed.

Read the three counters separately. `frames_lost()` counts packets the wire lost, `stale_refs()` counts messages naming an occupant you no longer hold and `divergences()` counts digest mismatches, which neither of the other two predicts.

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

When a browser tab backgrounds, a laptop sleeps or a frame loop stalls, the socket keeps receiving. A resumed client then gets a *lump*: minutes of packets describing moments it can no longer play.

Recovery depends on one invariant, with each half in a different crate: **a client may discard any stretch of the stream unread, provided it also drops the state derived from it, because an acknowledgement carrying the digest of nothing obligates the server to answer with a full baseline.** There is no resync request message; dropping the mirror acts as the request.

Three layers each handle one part:

*   **Transport**: discards the backlog before parsing it (`plaza_ws::trim_backlog`).
*   **Playout queue**: treats the gap as a discontinuity and restarts once, keeping the newest packet.
*   **Server**: stops streaming to a subscriber that has provably stopped reading (`DeltaBaseline::with_flow`).

Your code handles one case: on `Admission::TimelineLost`, drop the mirror and re-anchor the render clock on what just arrived.

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
  send(Ack { newest, mask });    // sixteen bytes, whatever the loss rate
}
```

**Which answer you want depends on what your protocol does with it and a wrong choice raises no error.** A protocol that **retransmits** wants the mask:

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

### Measuring a Rate

`RateMeter` is a running total, a sample count and a clock you supply. Feed it amounts as they happen and hand it your clock each tick:

```rust,ignore
use plaza_client_utils::RateMeter;

let mut traffic = RateMeter::new();
let mut packets = RateMeter::new();

// Each poll: the delta of a cumulative counter.
let rx = pump.bytes_received();
traffic.add(rx - seen_rx_bytes);
seen_rx_bytes = rx;
packets.add(1);

// Each tick, with the simulation clock rather than wall time.
traffic.elapsed(now_ms);
packets.elapsed(now_ms);
```

Read `per_sec` for a number somebody watches and `lifetime_per_sec` for a summary over a fixed run:

```rust,ignore
let live = traffic.per_sec();              // the last eight seconds
let whole = traffic.lifetime_per_sec();    // since this meter started
let per_packet = traffic.mean();           // per sample, same window as per_sec
```

A lifetime average on a live readout keeps creeping toward a new steady state for minutes, which looks like a slow leak. The rolling window follows a setting you just changed within seconds and decays to zero when traffic stops.

Before optimising how a stream is encoded, measure its share of the whole:

```rust,ignore
positions.add(encoded_positions.len() as u64);
bytes.add(packet.len() as u64);
let share = positions.share_of(&bytes);    // 0.0..=1.0
```

In `horde_playground` despawn ids were 1.2% of the traffic and position samples 86.1%. Call `reset()` when the world is rebuilt, so the rates cover only the current world. `add_empty()` counts a sample that carried nothing toward `mean`. See [`RateMeter`](API_REFERENCE.md#struct-ratemeter).

## Deterministic Arithmetic

For a wire that carries inputs rather than state, where nothing is ever corrected and `f32` cannot be relied on to match between a wasm build and a native one. Module `fixed` needs the `fixed` feature, which also pulls in `serde`:

```toml
plaza_client_utils = { version = "0.6", features = ["fixed"] }
```

```rust,ignore
use plaza_client_utils::fixed::{Fx, P};

let speed = Fx::ratio(3, 2);              // 1.5
let pos = P::from_ints(10, 4);
let next = P { x: pos.x + speed, y: pos.y };

if next.dist_sq(target) < RANGE_SQ {      // no square root on the path
  hit();
}

draw(next.x.to_f32(), next.y.to_f32());   // the only float, one way, for the renderer
```

Nothing in a simulation may call `to_f32`.

## Agreeing on Randomness and State

A shared rule that draws a random number, reads terrain or walks a map must get the same answer on both ends and in every build. `mix64`, `XorShift::next`, `XorShift::below` and `StateDigest` are integer arithmetic. `XorShift::unit`, `ValueNoise::corner` and `ValueNoise::octave` return `f32` built only from integer conversion, `floor` and the basic IEEE operations, which round the same way on wasm and native. None of it has dependencies.

### Drawing Numbers Both Ends Agree On

`XorShift` is a stream: seed it once and replaying a tick reproduces its draws exactly. `mix64` folds values into a seed:

```rust,ignore
use plaza_client_utils::determinism::{mix64, XorShift};

fn deal_seed(name: &str, tick: u64, deals: u64) -> u64 {
  let name = name.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |acc, b| mix64(acc ^ u64::from(b)));
  mix64(name ^ mix64(tick) ^ mix64(deals.rotate_left(32)))
}

let mut rng = XorShift::new(deal_seed(&table.name, table.tick, table.deals));
for i in (1..deck.len()).rev() {
  deck.swap(i, rng.below(i as u32 + 1) as usize);   // Fisher-Yates
}
let jitter = rng.unit();                            // 0.0..1.0
```

A stateless draw needs no generator at all. Key it on what it belongs to and no order can differ between the ends:

```rust,ignore
let roll = mix64(seed ^ mix64(entity_id) ^ mix64(tick)) % 100;
```

Walking a `HashMap` while drawing from one shared `XorShift` hands each entity a different number on each run. Sort the keys first or key each draw with `mix64`.

### Generating Terrain From a Seed

`ValueNoise` samples one octave of smoothed value noise. Octave weights and scales are yours:

```rust,ignore
use plaza_client_utils::determinism::ValueNoise;

const NOISE: ValueNoise = ValueNoise::new(SEED);
const LATTICE: f32 = 19.0;   // squares between lattice points; larger is smoother

pub fn height_at(x: f32, z: f32) -> f32 {
  let broad = NOISE.octave(x, z, LATTICE, 0);
  let hills = NOISE.octave(x, z, LATTICE / 2.6, 1) * 0.45;
  let detail = NOISE.octave(x, z, LATTICE / 6.1, 2) * 0.15;
  broad + hills + detail
}
```

Derive both the renderer's surface and the pathfinder's walkable squares from the one function, so the picture and the rules agree about where a cliff is. `NOISE.corner(xi, zi, octave)` reads a raw lattice value.

### Checking Two Worlds Match

`StateDigest` folds a simulation state into one `u64`. Write the fields in the same canonical order on both ends each frame and compare:

```rust,ignore
use plaza_client_utils::StateDigest;

pub fn digest(world: &World) -> u64 {
  let mut digest = StateDigest::new();
  for p in &world.paddles {
    digest.write_i32(p.x.0);
    digest.write_i32(p.y.0);
  }
  digest.write_i32(world.puck.x.0);
  digest.write_i32(world.puck.y.0);
  digest.write_i32(world.scores[0] as i32);
  digest.write_i32(world.scores[1] as i32);
  digest.finish()
}

// Server: ship it with the frame. Client: compare against its own world for that frame.
if digest(&local_world) != frame.digest {
  error!(frame = frame.number, "worlds diverged");
}
```

`write_f32` hashes the bit pattern, so `-0.0` and `0.0` disagree and a difference in the lowest bit shows. `SetDigest` answers "do we hold the same set"; `StateDigest` answers "is this the same world". See [`determinism`](API_REFERENCE.md#19-module-determinism) and [`StateDigest`](API_REFERENCE.md#struct-statedigest).

## Testing Without a Network

Module `net_sim` needs the `net-sim` feature:

```toml
plaza_client_utils = { version = "0.6", features = ["net-sim"] }
```

```rust,ignore
use plaza_client_utils::net_sim::{LatencyLink, Ordering, Rng};

let mut link = LatencyLink::new().with_ordering(Ordering::Ordered);
let mut rng = Rng::new(42);

link.send(now_ms, packet, 80, 15, 2.0, &mut rng);   // 80ms, 15ms jitter, 2% loss
for packet in link.drain_due(now_ms) {
  client.apply(packet);
}
```

`Ordering::Ordered` is the default because that is what TCP and WebSocket are. An unclamped queue reorders under jitter and produces a failure the real transport cannot.

## Four Principles

No type can enforce these. Breaking them causes bugs that the rest of this crate can only recover from after they happen.

**A shared rule must be shared code, not code written twice.** The `apply` you hand a predictor should be the server's own step function. Anything the server does that your copy leaves out shows up as a constant correction: it looks like network jitter, it is largest when it is most visible and it is hard to track down later. If your rule needs the world to run, pass it with `set_context`.

**Prediction is presentation; shared rules consume authoritative state.** Feeding a locally predicted position into a rule that both sides run makes the client compute a different world from the server's and every packet then pulls it back. Prediction drives the camera and your own marker. The rules both sides run read `logical()` or the authoritative state, even though it is older.

**One instant per frame.** Pick a single `T` and evaluate everything at it. That covers where entities are drawn and also everything a behaviour rule reads while producing the frame, such as aim targets and chase context. An entity simulated to `T` that reads its target from the newest packet mixes two timelines in one scene.

**The timeline comes from declaration, not arrival.** Round trips and jitter and arrival times may size buffers and admit or refuse connections. They never decide which moment is on screen or when an input executes. A render clock steered by packet arrival hides bad links and lets every client pick a different "now". It also lets each player's ping change what happens in the game.

## What the Measurements Settled

**A blend fraction beats an ease duration for a held-input predictor.** A fixed-duration ease has a correction rate above which it never finishes. In `horde_playground` that made locally simulated enemies get *worse* as the send rate rose, 10, 16 then 20 px at 4, 10 and 30 Hz. On `blend` the same entities sit at 9 to 10 px at every rate.

**Ease continuously rather than past a threshold.** Correcting only once the error crosses a threshold and then closing the whole gap produces a metronomic sawtooth: a small jump forward roughly every four hundred milliseconds, at every latency including zero, which a player feels as a rhythmic tug.

**The ease-versus-rate crossover.** Worst error 2.67 at one correction every 0.5 s, 15.00 at one every frame, against 11.33 for `at_rate(0.85)`. Below that crossover the duration wins.

**Running the rule beats interpolating.** Over 3000 enemies in `horde_playground`, by 43 px of mean error at 1 Hz and it still leads at 30 Hz, because an interpolated entity is always a send interval in the past.

**A spline is 484x better on a smooth path and 13x worse with impacts.** On a 10-unit circle at 10 Hz, worst error 0.0003 against linear's 0.1231. Across 300 solver-driven cubes at 10 Hz *with impacts*, it left the bracketing segment on half of all frames by up to 2.48 units and came out 13x worse than the chord it replaced.

**Second-order dead reckoning helps only at low send rates.** The correction goes as the gap squared. On a circular path at 10 Hz coasted through a 100 ms gap it cuts the error 45%; at a normal server rate it changes nothing measurable. It starts to help below about 10 Hz.

**Ack-driven resends against blind redundancy.** In `rollback_playground`, 28% cheaper on a clean link, 45% dearer at 50% loss, crossing over around 12%. Blind redundancy makes a fixed number of attempts; acks retry until acknowledged and converged at 55% loss where blind did not.

**Discarding unrebaseable deltas empties the mirror.** An earlier `horde_playground` version discarded and at 25% loss its mirror emptied out while every agreement check read perfect, because the checks only ran over what had been applied.

**Reuse order decides which despawn encoding wins.** Under `ReusePolicy::Lifo` a burst of 233 despawns was 204 separate runs, mean run length 1.14, which is why run-length encoding lost decisively to delta-varint there.

**When a slot generation matters.** Under ordered delivery with each death announced before the next diff, `horde_playground` recorded zero stale handle references across 413 kills with slots actively recycling. It became necessary again once loss recovery re-derived a retraction after a slot may have been recycled.
