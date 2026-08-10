# API Reference: `plaza_client_utils`

## 1. Introduction & Core Concepts

`plaza_client_utils` is the **client half** of real-time networking: making a server's authoritative updates feel immediate and smooth. The server half (input sequence tracking, delayed input buffers, lag-compensation rewind), lives in `plaza::game_common::reconciliation`.

**No workspace dependencies.** This crate depends on `thiserror` and `tracing` and nothing else, deliberately, so wasm builds and game-engine plugins do not drag in a server's async runtime. It is pure logic with no transport, no serialization, and no engine coupling; you feed it what you receive and read back what to render.

It addresses these problems, usable independently:

| Problem | Piece |
|---|---|
| Local input should feel instant, but the server decides | [`PredictedEntity`](#struct-predictedentitystatetype-op) + [`ClientInputBuffer`](#struct-clientinputbufferop-predictedstatesnapshot) |
| Other players' updates arrive discretely and jittery | [`SnapshotBuffer`](#struct-snapshotbuffertimestamp-statetype) |
| Updates stop arriving for a moment | [`ExtrapolationBase`](#struct-extrapolationbasestatetype-velocitytype-servertimestamp) |
| A variable frame has to drive a fixed-step simulation | [`FixedTimestep`](#7d-module-timestep) |
| Holding the client side of a streamed entity set, and proving it agrees | [`DeltaMirror`](#7e-module-mirror-the-client-half-of-a-delta-stream) |
| A resumed tab faces minutes of backlog it can never play | [`PlayoutBuffer`](#7i-module-playout-the-playout-queue-and-the-resume-verdict) |
| Nothing tells a real client the send rate or the delay its buffer must cover | [`ArrivalMonitor`](#7j-module-arrival-measuring-how-a-stream-actually-arrives) |
| Peers run one deterministic sim and cannot wait for each other's input | [`RollbackSession`](#3b-rollback-netcode-deterministic-lockstep) |

All but the last serve the *server-authoritative* model (an authority decides, a client predicts its own entity and is reconciled). The last serves the other family, *peer-to-peer deterministic lockstep* (rollback), covered in [section 3b](#3b-rollback-netcode-deterministic-lockstep).

### Four principles worth knowing before you predict or render anything

None is enforceable by a type, and between them they account for every netcode bug found while building the playground examples ([examples/LEARNINGS.md](../examples/LEARNINGS.md) is the evidence). They *prevent* bugs, where everything else in this crate only recovers from them. The first two are about simulation, the last two about rendering.

**A shared rule must be shared code, not code written twice.** The `apply` you hand a predictor is meant to *be* the server's step function, not a client approximation of it. Anything the server does that your copy leaves out arrives as a permanent correction: it looks like network jitter, it is largest exactly when it is most visible, and it is expensive to find later. If your client's rule needs the world to run (gravity, wind, a moving platform), that is what the context parameter is for; being unable to pass the world in is exactly what pushes people into writing the second, lesser rule, so it is a deficiency in the API rather than a reason to fork the rule.

**Prediction is presentation; shared rules consume authoritative state.** Feeding a locally predicted position into a rule that *both* sides run creates a second, divergent world, and every packet then fights the local one. Prediction drives the camera and the local player's own marker; the rules both sides run read the authoritative state, even though it is older. This is counterintuitive, because using the freshest local data looks like an improvement.

**One instant per frame.** A client that renders in the past picks a single instant T for the whole frame, and everything is evaluated at T: not only where entities are drawn, but everything a behaviour rule reads while producing the frame, aim targets and chase context included. An entity simulated to T while reading a target from the newest packet is two timelines in one scene, and the seam between them is a bug whether or not it is visible yet. [`InterpolationClock`](#struct-interpolationclockt) supplies T; the discipline of feeding every read from it is the application's.

**The timeline comes from declaration, not arrival.** Transport facts, round trips and jitter and arrival times, may size buffers and admit or refuse connections. They never decide which moment is on screen or when an input executes; those are declared numbers the server chooses and publishes. A render clock steered by packet arrival hides bad links instead of reporting them, lets every client pick a different "now", and quietly makes ping an input to the game.

### The resume contract

Every long-lived client eventually stops reading: a browser tab goes to the background, a laptop sleeps, a frame loop stalls. The socket keeps receiving the whole time, so what a resumed client faces is not a slow stream but a *lump*: minutes of packets, delivered at once, describing moments it can never play. The recovery that works is built from one invariant, stated here because each half lives in a different crate: **a client may discard any stretch of the stream unread, provided it also drops the state derived from it, because an acknowledgement carrying the digest of nothing obligates the server to answer with a full baseline.** That is the digest-and-rebuild machinery of `plaza_server_utils::DeltaBaseline` and [`DeltaMirror`](#7e-module-mirror-the-client-half-of-a-delta-stream), read as a permission, and it is why there is no "resync request" message anywhere: dropping the mirror *is* the request.

On top of it, resume is three verdicts at three layers, each owned by a block: the **transport** discards the backlog before parsing it (`plaza_ws::trim_backlog`), because none of it survives what follows; the **playout queue** treats the gap as a discontinuity and restarts once, keeping only the newest packet ([`PlayoutBuffer`](#7i-module-playout-the-playout-queue-and-the-resume-verdict)); and the **server** stops streaming to a subscriber that has provably stopped reading (`DeltaBaseline::with_flow`), so the lump never grows to megabytes in the first place. The application's remaining job is small and cannot be taken from it: on `Admission::TimelineLost`, drop the mirror and re-anchor the render clock on what just arrived.

### Which predictor

The two local-player bundles differ by how the **server** consumes input, not by how the client feels. Choosing wrong is silent, and shows up as a prediction that is always slightly behind.

| the server | use |
|---|---|
| consumes one input per simulation step | [`PredictedPlayer`](#struct-predictedplayerstate-input-ctx) (replay unacknowledged inputs) |
| holds an input and integrates it every tick | [`HeldInputPredictor`](#struct-heldinputpredictorstate-input-ctx) (dead reckon and ease) |

Replaying inputs against a server of the second kind double counts, and gets worse the more you economise on bandwidth, because one coalesced input can cover a long stretch of simulation.

**`HeldInputPredictor` is not only for the entity you control**, which is the least obvious thing in this crate. It runs a rule locally and corrects from sparse authoritative samples, and nothing in it cares whether the "held input" came from a keyboard. For an entity whose behaviour you know, hold its *intent* (an enemy's target, a vehicle's waypoint), put the world it reads in `Ctx`, and it becomes a locally simulated remote. That is the fourth option in the table below, and it is the one `RemoteView` cannot express.


### Drawing an entity you do not control

Four options, and the choice is a property of **the entity**, not of the game. Different entities in one scene legitimately sit on different rows.

| you know | draw it by | piece |
|---|---|---|
| its rule, and the inputs the rule reads | running that rule locally, corrected by samples | [`HeldInputPredictor`](#struct-heldinputpredictorstate-input-ctx) |
| nothing but its past positions | interpolating between two real samples, in the past | [`RemoteView`](#struct-remoteviewstate-velocity) with `interpolate` |
| its positions, and that its motion is constrained | dead reckoning along the last velocity, briefly | `RemoteView` with `extrapolate` |
| none of the above | holding the newest sample | `RemoteView`, both off |

Take the highest row you have the data for. Measured over 3000 enemies in `horde_playground`, running the rule beats interpolating by 43 px of mean error at 1 Hz and still leads at 30 Hz, because an interpolated entity is always a send interval in the past and at 1 Hz that is a second.

The rows are not interchangeable at the bottom either. Dead reckoning a **player** is guessing at a human's intention, which nothing on the wire carries, so it overshoots every direction change; it is for entities with inertia and a turning limit.

### The prediction loop

1.  The player acts. Apply it locally at once and record it with its sequence number, so the screen responds without waiting for a round trip.
2.  The server eventually replies with authoritative state plus the last sequence number it processed.
3.  Acknowledge everything up to that number, snap to the authoritative state, then **replay** the inputs it had not yet seen. The result is the server's truth plus your still-pending actions.

A misprediction corrects itself on the next reconciliation; that is the design, not a failure.

## 2. Error Handling

### Enum `ClientUtilError`

`Debug`, implements `std::error::Error` via `thiserror`. Variants cover `InputBufferFull`, `InputNotFoundInBuffer`, `ReconciliationInconsistency`, `InterpolationError`, `ExtrapolationError`, `InvalidArgument`.

Most operations here return values or `Option` rather than `Result`: a full buffer discards its oldest entry and logs, because dropping a stale input is preferable to failing a frame.

## 3. Core Types

### Type Aliases

*   **`SequenceNumber = u64`**: identifies one client input, incrementing per input.
*   **`ClientTimeMs = u64`**: client-local milliseconds since some fixed origin.

## 3a. Drop-in entities

The recommended starting point: three types that bundle the primitives into the whole client-side job. The primitives (sections 4 onward) remain public for finer control.

### Struct `PredictedPlayer<State, Input, Ctx>`

Your controlled entity when the server consumes **one input per simulation step**: `PredictedEntity` + `ClientInputBuffer` + `ErrorSmoother` + a sequence counter, wired. `apply` and `lerp` are plain `fn` pointers (the game rule and the smoothing blend), so no closure bounds are imposed.

*   **`new(initial, PlayerConfig, apply: fn(&mut State, &Input, &Ctx), lerp: fn(&State, &State, f32) -> State)`**
*   **`input(&mut self, input) -> SequenceNumber`**: predict locally, buffer for replay, return the sequence to send.
*   **`reconcile(&mut self, authoritative, acked_seq) -> Correction<State>`**: snap the logical state to authority, replay unacknowledged inputs, begin easing the visible correction, and hand back what it did (see [`Correction`](#7f-module-correction)).
*   **`advance(&mut self, dt_secs)`**, **`render() -> State`** (eased), **`logical() -> &State`** (exact), **`authoritative() -> &State`**, **`latest_seq()`**, **`acked_seq()`**, **`unacked_count()`**.

**Lifecycle**, shared with `HeldInputPredictor` so the two read as one family:

*   **`set_active(&mut self, bool)`** / **`is_active()`**: an inactive predictor stops integrating and stops correcting. The case it exists for is a server holding an entity still (a respawn delay, a stun, a cutscene). Without it a client keeps predicting motion into a frozen entity and invents a correction stream entirely of its own making, which is a real bug this cost a diagnostic cycle.
*   **`teleport(&mut self, State)`**: a discontinuity, applied without easing. Snap-versus-ease is chosen by **cause**, not magnitude: ease continuous error, snap a spawn, a respawn or a warp. Easing a two thousand pixel jump draws the player smoothly across the arena.
*   **`set_context(&mut self, Ctx)`** / **`context() -> &Ctx`**: the world the rule needs to run. `Ctx` defaults to `()`, so a rule that needs nothing pays nothing.

**`PlayerConfig`**: `input_buffer: usize` (retain the most inputs that can be in flight), `smoothing_secs: f32` (`0.0` disables smoothing), `easing: fn(f32) -> f32` (the correction's time curve, default `linear`). `Default` is 256 / 0.1s / linear.

Keep `smoothing_secs` **shorter than your send interval**. Measured in `horde_playground`: a 250 ms ease never completes when corrections arrive every 33 ms, so the smoother itself becomes the dominant error and accuracy gets *worse* as the send rate rises.

Snap-versus-ease on a large desync is otherwise the caller's call (check `render()` against the incoming authoritative before `reconcile`, or use `teleport`), because "how far is a desync" is application geometry.

### Struct `HeldInputPredictor<State, Input, Ctx>`

Your controlled entity when the server **holds an input and integrates it every tick**. Same shape as `PredictedPlayer` (predict locally, fold in authority, ease the visible correction) and a different correction model: there is nothing to replay, because the server is not consuming a queue, so it dead reckons under the held input and eases a fixed fraction of the error toward each authoritative sample.

*   **`new(initial, HeldInputConfig, advance: fn(&mut State, &Input, f32, &Ctx), lerp: fn(&State, &State, f32) -> State)`**: `advance` is the shared step, taking `dt` in seconds.
*   **`with_teleport(self, distance: fn(&State, &State) -> f32, beyond: f32)`**: past `beyond`, `reconcile` snaps instead of easing. The metric is an argument rather than a trait bound, so no geometry is imposed on `State`.
*   **`hold(&mut self, input)`** / **`held() -> &Input`**: the direction the server is holding.
*   **`advance(&mut self, dt_secs)`**: integrate locally, then progress the ease.
*   **`project(&self, authoritative: &State, age_secs: f32) -> State`**: where that sample would be *now* under the held input. Public so an application can measure and decide for itself.
*   **`reconcile(&mut self, authoritative, age_secs) -> Correction<State>`**: project the sample forward by its own age (the server's state is one one-way delay old), then ease toward it.
*   **`render()`**, **`logical()`**, **`set_active`**, **`is_active`**, **`teleport`**, **`set_context`**, **`context`**: as above.

**`HeldInputConfig`**: `blend` alone, the fraction of the remaining gap closed on each `reconcile`. `1.0` snaps, `0.0` is pure dead reckoning and drifts without bound. `Default` is `0.25`.

**A fraction rather than a duration, and that is the design.** A fixed-duration ease has a correction rate above which it never finishes, so corrections pile up and the smoother becomes the dominant error. Measured in `horde_playground`, that made locally simulated enemies get *worse* as the send rate rose (10, 16, then 20 px at 4, 10 and 30 Hz) and was mistaken for a limit of the technique; on `blend` the same entities sit at 9 to 10 px at every rate. Use `ErrorSmoother` when you need the logical state left exact and only the *drawing* eased, which is what `PredictedPlayer` requires because replay depends on it.

**The easing is the point, not a nicety.** Correcting only once the error passes a threshold and then closing the whole gap produces a metronomic sawtooth: holding one direction gave a small jump forward roughly every four hundred milliseconds, at every latency including zero, which a player feels as a rhythmic tug. Continuous easing absorbs the same drift invisibly, so this primitive makes it the default.

### Struct `RemoteView<State, Velocity>`

An entity you do not control: a `SnapshotBuffer` plus the interpolate / extrapolate / hold decision, with the buffer-starvation detection handled for you. Time is `u64` milliseconds or ticks; for a `Duration` timeline, compose `SnapshotBuffer` and `ExtrapolationBase` directly. Requires `State: Interpolatable<u64> + Extrapolatable<Velocity, f32>`.

*   **`new(buffer_size, max_extrapolation_ms)`**: `buffer_size` >= 2; dead-reckon at most `max_extrapolation_ms` past the newest before holding.
*   **`push(&mut self, time_ms, state, velocity)`**: record a snapshot and the velocity to dead-reckon along.
*   **`render(&self, target: Option<u64>, RenderOpts) -> Option<State>`**: `None` until the first push; otherwise the state to draw. Interpolated at `target`, dead-reckoned when the buffer has starved (if `opts.extrapolate`), or the raw newest (if `opts.interpolate` is false).
*   **`latest() -> Option<&State>`**.
*   **`over_extrapolations() -> u64`**: how many renders asked for a time further past the newest sample than `max_extrapolation_ms` and were served the capped coast instead. Holding at the cap is a legitimate outcome, so this is a rate to watch rather than an error count, and it accumulates what each render's `ExtrapolationBase` found, since that base is built per call and would otherwise take its own count with it. Climbing steadily is almost never a starved link: it means the render target is computed ahead of the newest sample rather than trailing it, so the entity is dead reckoned every frame and never interpolated.
*   **`oldest_timestamp() -> Option<u64>`**: the oldest instant the view can still interpolate at. A `render` target before this is **clamped to the oldest snapshot**, silently drawing the entity at a newer instant than asked for; a caller rendering a whole scene at one instant should compare its target against this and count the misses, and size `buffer_size` to cover its deepest render delay at its highest sample rate so they stay rare.

**`RenderOpts`**: `interpolate: bool`, `extrapolate: bool`. `Default` is both on; a real client fixes them, the booleans exist so a UI can toggle them.

### Struct `HermiteView<State, Velocity>`

`RemoteView` for low send rates. A straight line between two snapshots is right when they are close together and wrong when they are not: at 60 snapshots a second the error over a 16ms chord is invisible, at 10 the chord flattens 100ms of a curved path and the entity visibly corners, sliding to each sample and changing direction. A cubic Hermite spline passes through both samples *and* leaves along the velocity recorded at each, so the seams stop being corners.

Requires `State: HermiteInterpolatable<Velocity>`, implemented here for `f32`, `Vec2` and `Vec3`. An orientation wants `Quat::slerp` instead; a quaternion's components do not interpolate independently.

*   **`new(capacity)`**: panics below 2, since a spline needs two samples to sit between.
*   **`push(&mut self, time_ms, state, velocity)`**: out-of-order arrivals are inserted in time order rather than dropped, since a straggler still improves the segment it lands in.
*   **`render(&self, target_ms) -> Option<State>`**: `None` until the first sample; before the oldest or past the newest it **holds** that end rather than guessing. Coasting past the end is a separate decision with a different failure mode, which is what [`RenderOpts`](#struct-remoteviewstate-velocity) is for.
*   **`latest()`**, **`oldest_time()`**, **`latest_time()`**, **`len()`**, **`is_empty()`**, **`clear()`**.

Why it is a separate type rather than a flag on `RemoteView`: a spline needs the velocity at **both** ends, and `RemoteView` keeps one, for dead reckoning past the newest sample.

Measured on a 10-unit circle sampled at 10Hz and drawn at 60 (`cargo test -p plaza_client_utils hermite -- --nocapture`): worst error over a second is **0.0003 against linear's 0.1231**. That ratio is flattering because a cubic is near-exact on a smooth curve given true derivatives; expect less on a path that changes direction sharply. Worth it below roughly 20 snapshots a second, and not worth the second velocity on the wire much above that.

**Trait `HermiteInterpolatable<Velocity>`**: `hermite(&self, other, velocity_a, velocity_b, t, seconds) -> Self`, where `t` runs `0..=1` across the segment and `seconds` is its wall duration, which is what puts a per-second velocity into the same units as the positions. **`hermite_scalar(p0, v0, p1, v1, t, seconds) -> f32`** is the one-axis form.

## 3b. Rollback netcode (deterministic lockstep)

A different model from the rest of this crate. There is no server: peers run the **same deterministic simulation**, exchange only inputs, and stay identical frame for frame. Latency is handled by **predicting** a missing remote input (repeat its last one), simulating ahead, and **rolling back** to re-simulate when the real input arrives and disagrees. Determinism is what makes the re-simulation land on the state the other peer already has. Lives in module `rollback`.

Three pieces, smallest first; the primitives stay public for hand-wiring, `RollbackSession` is the ready-made loop (the rollback counterpart to [`PredictedPlayer`](#struct-predictedplayerstate-input-ctx)).

**`Frame = u64`**: a logical simulation frame. Rollback counts in fixed frames, never wall-clock time.

### Struct `StateHistory<State: Clone>`

A frame-indexed ring of whole-world snapshots: the save-states rollback restores. Pure save/restore by frame, no interpolation (unlike `SnapshotBuffer`, which blends between times).

*   **`new(capacity)`**: keeps the most recent `capacity` frames, the maximum rollback distance. **Panics if 0.**
*   **`save(&mut self, frame, state)`**: record a frame. Contiguous use (append `latest + 1`, or overwrite a frame in the window, as re-simulation does); a save that skips ahead resets the window rather than leaving a gap.
*   **`restore(&self, frame) -> Option<State>`**: the saved state, or `None` if evicted or never saved.
*   **`oldest_frame()`**, **`latest_frame()`**, **`len`**, **`is_empty`**, **`clear`**.
*   **`resets() -> u64`**: how many saves fell outside the window and reset it. Rollback assumes contiguous saves, so this should stay zero for the life of a session; non-zero means the window was thrown away and rebuilt from one frame, which silently shortens how far back the session can roll, and a correction arriving next frame finds nothing to restore.

### Struct `InputTimeline<Input: Clone + Debug>`

The inputs known for one source (one player), by frame, with unconfirmed frames left for the session to predict.

*   **`new(capacity)`**: retain inputs across `capacity` frames. **Panics if 0.**
*   **`confirm(&mut self, frame, input)`**: record the real input for a frame. Out-of-order arrivals are fine (a resent input fills a gap); a frame past the retained window is dropped.
*   **`confirmed_at(frame) -> Option<&Input>`**: the confirmed input, or `None` if that frame is still predicted.
*   **`last_confirmed_at_or_before(frame) -> Option<&Input>`**: the basis for predicting `frame`.
*   **`last_confirmed_frame() -> Option<Frame>`**.

**`fn repeat_last_input(last, frame) -> Input`**: the default predictor, repeat the last confirmed input. Right whenever a player holds an input, which dominates.

### Struct `RollbackSession<State, Input>`

The whole loop wired together: a `StateHistory`, an `InputTimeline` per player, the current frame, and the predict / detect / rollback / re-simulate cycle against a deterministic step you supply. Requires `State: Clone + Debug`, `Input: Clone + Debug + PartialEq`.

*   **`new(initial_state, neutral_inputs: Vec<Input>, RollbackConfig, advance: fn(&State, &[Input]) -> State)`**: `neutral_inputs.len()` is the player count; `neutral_inputs[p]` is what player `p` is assumed to hold before any of its inputs are known. `advance` is the deterministic step, the same on every peer.
*   **`with_predictor(self, fn(&Input, Frame) -> Input) -> Self`**: replace the default repeat-last predictor.
*   **`queue_local_input(&mut self, player, input)`**: the local player's input for the current frame (known before it runs, so never mispredicted).
*   **`confirm_remote_input(&mut self, player, frame, input)`**: a remote input for a past or current frame. If it contradicts the guess already used for a simulated frame, the session marks it for rollback.
*   **`advance_frame(&mut self)`**: roll back and re-simulate if a guess was disproved, then simulate the current frame, predicting any unknown input.
*   **`state() -> &State`**: the present, including predicted inputs. **`state_at(frame) -> Option<State>`**: the saved state at a frame; identical across peers for a fully-confirmed frame (the determinism guarantee, and how a demo checks two peers are in sync).
*   **`is_frame_confirmed(frame) -> bool`**: whether every player's input for a frame is known. A delay-based front end waits for this instead of predicting; that policy is the app's, so the session only reports it.
*   **`set_rollback_enabled(&mut self, bool)`**: turn rollback off to trust guesses forever (an A/B against what rollback buys, or a delay-based front end).
*   **`confirmed_frame(player)`**, **`prediction_horizon()`** (how far ahead of confirmation the present runs), **`current_frame()`**, **`num_players()`**, **`last_rollback_frames()`**, **`max_rollback_frames()`**, **`rollback_count()`**.

**`RollbackConfig`**: `max_rollback_frames: usize` (the rollback window, and the history retained; must exceed the worst latency in frames). `Default` is 240 (four seconds at 60 fps).

The `rollback_playground` example wires two full peers over a simulated wire and renders their agreement.

## 4. Prediction and Reconciliation

### Struct `PredictedEntity<StateType, Op>`

The client's own entity: a predicted state that runs ahead of the server, and the last authoritative state received.

*   **`new(initial_state: StateType) -> Self`**
*   **`apply_local_input_and_predict(&mut self, op: &Op, sequence_number: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_op_fn: &impl Fn(&mut StateType, &Op))`** Applies an input immediately and records it as unacknowledged.
*   **`reconcile_with_server_state(&mut self, new_authoritative_state: StateType, server_ack_input_seq: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_op_fn: &impl Fn(&mut StateType, &Op))`** Snaps to the server's state, drops acknowledged inputs, and replays the rest.
*   **Public fields**: `current_predicted_state: StateType`, `last_authoritative_state: StateType`, `last_server_acknowledged_input_seq: SequenceNumber`, readable (and writable) for rendering and diagnostics. `Clone` whenever `StateType: Clone`, with no bound on `Op`.

`apply_op_fn` is the shared simulation step, the same rule the server applies. Both sides must agree, or prediction fights the server every frame.

### Struct `ClientInputBuffer<Op, PredictedStateSnapshot>`

Unacknowledged inputs, kept for replay.

*   **`new(max_size: usize) -> Self`**: **panics if `max_size` is 0.**
*   **`record_input(&mut self, sequence_number, op, state_before_op_predicted)`**: stores an input and the state it was applied to. At capacity the oldest is discarded (with a `warn`).
*   **`acknowledge_inputs_up_to(&mut self, ack_sequence_number)`**: drops everything the server has processed.
*   **`get_unacknowledged_inputs(&self, last_acknowledged_sequence_number: SequenceNumber) -> impl Iterator`**: every buffered input with a sequence number greater than the argument, in order. What to replay.
*   **`get_predicted_state_before_input(&self, sequence_number) -> Option<&PredictedStateSnapshot>`** Useful for diagnosing how far a prediction diverged.
*   **`len`**, **`is_empty`**, **`clear`**
*   **`overflowed() -> u64`**: how many inputs were discarded because the buffer was full. Non-zero means a reconciliation can no longer replay everything the server has not acknowledged, so the prediction is wrong by whatever those inputs did. The `warn` beside it fires once per event into a log; this is the number to put on a HUD or an alert, since what matters is whether it is climbing. Size the buffer at input rate times worst round trip and it stays zero.

### Struct `BufferedInput<Op, PredictedStateSnapshot>`

One recorded input: its sequence number, the op, and the state before it applied.

## 5. Interpolation

Remote entities arrive as discrete snapshots at the server's tick rate. Rendering them directly looks stepped, so render *slightly in the past* and interpolate between the two snapshots bracketing that time.

### Struct `SnapshotBuffer<Timestamp, StateType>`

*   **`new(max_buffer_size: usize) -> Self`**: **panics if less than 2**; interpolation needs two points.
*   **`add_snapshot(&mut self, server_timestamp: Timestamp, state: StateType)`** Keeps chronological order, inserting late arrivals in position. Over capacity, the oldest is dropped.
*   **`get_interpolated_state(&self, target_render_time: Timestamp) -> Option<StateType>`** `None` only when the buffer is empty. A target before the oldest or after the newest snapshot clamps to that end rather than extrapolating.
*   **`oldest_timestamp`**, **`latest_timestamp`**, **`len`**, **`is_empty`**, **`clear`**

Choose the render delay to exceed your typical inter-snapshot gap: roughly one to two server ticks. Too small and the buffer runs dry; too large and remote entities visibly lag.

### Struct `InterpolationClock<T>`

Supplies the `target_render_time` that `get_interpolated_state` needs, so you do not hand-track it. `T` is whatever timeline the snapshots use (`u64` ms or ticks, or a `Duration`).

*   **`new(delay: T) -> Self`**: render this far behind the estimated server clock.
*   **`observe(&mut self, server_time: T)`**: the first call starts the clock; later calls are ignored, so the estimate free-runs on `advance` instead of snapping on every packet.
*   **`advance(&mut self, dt: T)`**: move the estimate forward by a frame's worth of time.
*   **`target(&self) -> Option<T>`**: the point to interpolate at (`now - delay`), or `None` before the first `observe`. Clamped so it never precedes the timeline's zero.
*   **`started(&self) -> bool`**

This fits the "estimate server time from packets, then subtract the delay" model. A client that instead derives its clock from a local wall-clock anchored to a join time (as `csp_net_example` does) is a different strategy and does not need this.

*   **`resync(&mut self, newest_server_time_ms: u64, strength: f32)`** (on `InterpolationClock<u64>`): steers the estimate toward the newest server time seen, by `strength` in `[0, 1]`. Call it on each packet in place of `observe` for a clock that self-corrects as latency drifts, rather than free-running and eventually starving the interpolation buffer. `strength` near `0.1` is smooth; `1.0` snaps.
*   **`observe_rate(&mut self, newest_server_time_ms: u64, max_rate_adjust: f32)`** + **`advance_scaled(&mut self, dt_ms: u64)`** + **`playback_rate() -> f32`** (on `InterpolationClock<u64>`): the rate-based alternative to `resync`. Instead of nudging the estimate's *position* toward the stream (a small snap each packet), `observe_rate` adjusts its *speed*, faster when the estimate is behind the newest time, slower when ahead (buffer starving), so it glides into alignment (time dilation). Drift is normalized by the render delay; `max_rate_adjust` (e.g. `0.1` for +/-10%) bounds how far from real time it goes. Pair `observe_rate` (per packet) with `advance_scaled` (per frame) in place of `observe`/`advance`; `playback_rate` reads the current dilation for a readout. Pick one of `resync` or `observe_rate`, not both.
*   **`delay()`** / **`set_delay(delay)`**: read and change the render-behind delay. Change it at runtime to size the interpolation buffer dynamically, larger under jitter, smaller on a stable connection.

### Struct `Timeline` and struct `Probe`

The bookkeeping around a probe, and where most clients should start rather than driving the two estimators by hand.

```rust
let probe = timeline.begin(now);          // stamp your Kind::Ping with probe.sent_at
timeline.complete(probe, now, pong.responder);   // feeds both estimators
```

*   **`begin(now) -> Probe`**, **`complete(probe, now, responder: Option<u64>) -> bool`**: `false` when the probe was discarded. With a `responder` the clock fit gets an exchange too; without one, only the round trip is recorded.
*   **`on_reconnect()`**, **`on_resume()`**, **`epoch()`**.
*   **`note_stamp(stamp_ms, now_ms)`**, **`newest_stamp_ms()`**: records a timestamp the server wrote into a message. A stamp needs no synchronisation to trust: the server wrote it, so server time is provably past it.
*   **`server_time_ms(now_ms) -> u64`**: the best estimate of server time now. The fitted clock (falling back to `now_ms` until two exchanges are in), **floored by the newest stamp carried forward at wall rate**. The floor holds even while the fit is empty or trailing, which it does for hundreds of milliseconds after a resume while its window refills, and it only ever lifts the estimate, never past the truth: the stamp trails real server time by the one-way delay it took to arrive.
*   **`rtt`** and **`clock`** are public: read the estimates straight off them.

**A probe carries the epoch it started in, and one that outlives its epoch is discarded rather than recorded.** A probe sent before a suspend and answered after it measures the suspend, not the network, and a smoothed estimator carries one such sample for minutes. A **reconnect** invalidates measurements in flight but keeps what has been learned, because the socket changed and the link probably did not. A **resume** invalidates both, because arbitrary wall time passed and a least-squares fit across a ten-minute gap produces a meaningless skew. The newest stamp survives both: it is a fact about the stream, not a measurement of the link.

### Struct `RttEstimator`

Smooths round-trip samples (from a `plaza_wire` `Kind::Ping` exchange) into a stable latency estimate. Both a client measuring the server and a server measuring a client use it.

**No unit is named or assumed.** Samples go in as whatever you stamped a probe with and every number comes back in that same unit: feed milliseconds and read milliseconds, feed microseconds and read microseconds. Mixing two units across one estimator is the only way to get a wrong answer, and no signature can stop you, so pick one where you stamp and keep it.

*   **`new(alpha)`** / **`Default`** (alpha 0.1): `alpha` is each sample's moving-average weight.
*   **`rtt()`**, **`one_way()`** (half the RTT), **`min_rtt()`** (the smallest seen, the best latency estimate since jitter only adds delay), **`jitter()`** (the smoothed mean deviation, size a dynamic interpolation buffer from it). Each `None` before the first sample.
*   **`observe(sample)`**, **`observe_pong(origin, now)`** (the round trip is `now - origin`), **`clear()`**.

`RttEstimator` is the zero-config default. The next two are heavier building blocks for when it is not enough, offered as options, not replacements.

### Struct `ClockSyncEstimator`

Fits the client-to-server clock **offset and its skew** (drift rate) by least squares over a sliding window, where a moving average models offset alone. `f64` times (millisecond timestamps over a long session exceed `f32`'s integer precision).

*   **`new(window: usize)`**: fit over the last `window` measurements (16 to 64 typical). **Panics if less than 2.**
*   **`observe(local, offset)`**: record a measured `offset = server - local` at local time `local`.
*   **`observe_exchange(local_send, remote_recv, local_recv)`**: derive the offset from a round trip under the symmetric-delay assumption. `remote_recv` is what the other end stamped into its reply, which for a probe is `Pong.responder`.

**Both ends must mean the same unit.** Unlike `RttEstimator`, which only ever subtracts two of your own readings, this compares your clock against someone else's, so a disagreement about the unit produces a confident wrong answer rather than a visible one.
*   **`offset_at(local) -> Option<f64>`** (fitted, so it interpolates and extrapolates along the line), **`server_time_at(local) -> Option<f64>`**, **`skew() -> f64`** (drift per unit time; `x 1e6` for ppm), **`is_ready()`**, **`sample_count()`**, **`clear()`**.

One honest limit: a round trip cannot recover the *asymmetric* one-way offset (upload slower than download) without an external time source; regression buys the drift rate cleanly, not the asymmetric constant. Size the interpolation buffer to absorb the residual. Contrasted against a moving average in the [`estimator_lab`](examples/estimator_lab.rs) example.

### Struct `ScalarKalman`

A one-dimensional Kalman filter: an optimal smoother for one noisy scalar (jitter, latency, a bandwidth estimate). Unlike a moving average it tracks its own confidence, settling fast then rejecting jitter once settled. Two knobs, `f32`.

*   **`new(process_noise: f32, measurement_noise: f32)`**: `process_noise` (`Q`) is how much the true value wanders between samples (higher trusts measurements more); `measurement_noise` (`R`) is how noisy each reading is (higher smooths harder). The first `observe` seeds the estimate.
*   **`with_initial(estimate, variance)`**: seed explicitly instead.
*   **`observe(measurement) -> f32`** (returns the updated estimate), **`estimate()`**, **`variance()`**, **`last_gain()`** (near 1 while settling, near 0 once settled), **`is_initialized()`**, **`set_process_noise(q)`** / **`set_measurement_noise(r)`** (retune live), **`reset()`**.

### Trait `Interpolatable<Timestamp>`

```rust,ignore
pub trait Interpolatable<Timestamp>: Sized + Clone
where Timestamp: Copy + Debug + PartialOrd {
  fn interpolate(&self, other: &Self, t: f32, time_a: Timestamp, time_b: Timestamp) -> Self;
}
```

Implement for whatever you render. `t` is `0.0` at `self`, `1.0` at `other`. The timestamps are supplied for non-linear schemes.

### Trait `ToF32`

Converts a timestamp (or the difference between two), into `f32` so the interpolation factor can be computed. Implemented for `u64`, `i64`, `f32`, `f64`, and `Duration`.

`Interpolatable` and `ToF32` are shared with `plaza_server_utils`: its `HistoricalStateBuffer` uses the same traits, so one state type feeds both a client's `SnapshotBuffer` and the server's rewind with a single impl.

### Struct `ServerSnapshot<Timestamp, StateType>`

One buffered entry: `server_timestamp`, `state`.

## 6. Extrapolation

When snapshots stop arriving, continue an entity along its last known velocity rather than freezing it.

### Struct `ExtrapolationBase<StateType, VelocityType, ServerTimestamp>`

*   **`new(state, velocity, server_timestamp, client_receipt_time_ms: ClientTimeMs) -> Self`**: the last authoritative state, plus when the client processed it (which is what the extrapolation duration is measured from).
*   **Public fields**: `state`, `velocity`, `server_timestamp`, `client_receipt_time_ms`.
*   **`over_extrapolations() -> u64`**: how many projections ran past the cap and were held at it. Behind a `Cell`, because the method that counts is a read and making it `&mut self` would force every caller holding a base across frames to hold it mutably just to draw.
*   **`get_extrapolated_state<TimeDelta>(&self, target_client_render_time_ms: ClientTimeMs, max_extrapolation_duration_ms: u64, convert_ms_to_time_delta: impl Fn(u64) -> TimeDelta) -> Option<StateType>`** Projects forward, capping the **duration** by `max_extrapolation_duration_ms` so an entity coasts to the limit and stops there rather than running off into the distance. A target before `client_receipt_time_ms` returns the un-extrapolated base state (extrapolation is for the future; use interpolation for the past). `convert_ms_to_time_delta` turns the capped millisecond duration into whatever `TimeDelta` your `Extrapolatable` impl takes (`|ms| ms as f32 / 1000.0` for seconds, `Duration::from_millis` for a `Duration`).

Capping the duration rather than discarding the result is the fix to a real bug worth knowing about, because "clamp" reads naturally as the other thing. Returning the *un-extrapolated* state past the cap is a discontinuity: at the limit an entity has coasted `velocity * max_ms` forward, and one millisecond later it was drawn back at the raw sample, a jump of the entire window in the wrong direction, flickering whenever a jittery target crossed the boundary. Two tests had asserted the old behaviour, so they were pinning the bug rather than the requirement.

The cap logs at `warn`, and a *steady* occurrence usually means a render target sitting permanently ahead of the newest snapshot rather than a starved link, so the view never interpolates at all. That warning is doing its job; it was briefly downgraded for being repetitive and was the only thing announcing a real defect.

### Trait `Extrapolatable<VelocityType, TimeDelta>`

```rust,ignore
pub trait Extrapolatable<VelocityType, TimeDelta>
where Self: Sized + Clone, VelocityType: Debug, TimeDelta: Copy + Debug {
  fn extrapolate_with_velocity(&self, velocity: &VelocityType, delta_time: TimeDelta) -> Self;
}
```

**Extrapolation is the starvation fallback, not a general technique.** When the render target runs past the newest snapshot there is nothing to interpolate between, and the only choices are freeze or coast. Whether coasting helps is a property of the **entity**, not of the game: it works when the next state follows from the current one, which is true of vehicles, projectiles and anything with inertia and a turning limit, and is where the term comes from. It fails for anything steered instantaneously by a person or an AI, because there the velocity is not a constraint on the future, it is a record of the past. Dead reckoning a *player* overshoots every direction change and snaps back when the truth lands.

That gives a hierarchy, and this crate has all four rungs. Run the entity's own rule if you know it and know its inputs (best, and what makes a 1 Hz enemy stream playable). Interpolate between two real snapshots if you do not (safest, and what peers should do). Extrapolate from one snapshot and a velocity only if the dynamics are predictable. Hold if they are not. Reach down the list only when the rung above has no data. Different entities in one game legitimately sit on different rungs.

## 7. Smoothing

Reconciliation snaps the predicted state to the server's truth and replays. When the prediction was right this is invisible; when it was wrong the entity teleports to the corrected spot in one frame. `ErrorSmoother` turns that teleport into a short glide.

### Struct `ErrorSmoother<State>`

Smooths only what you *draw*, never the logical predicted state, which must stay exact for the next prediction. It holds the visual position where the eye currently is and eases it toward the live logical state. It is a standalone primitive, not a method on `PredictedEntity`, because any jumping entity wants it (a remote box that pops on a late snapshot, for instance), and because prediction needs only "apply an op" while smoothing needs "blend two states". The blend is a closure you pass, so no trait bound is imposed on `State`.

*   **`new(duration_secs: f32) -> Self`**: ease each correction over this long; `0.0` disables smoothing (every correction snaps).
*   **`with_easing(self, easing: fn(f32) -> f32) -> Self`**: sets the time curve the correction eases along (default `linear`). The blend *across states* stays your `sample` `lerp`; the easing only remaps how far along the ease is this frame. It is a plain `fn` pointer, not a closed enum, so any curve works and it stays a zero-cost indirect call. `smoothstep`, `ease_out_cubic`, and `ease_in_out_quad` ship as conveniences; `PlayerConfig` carries an `easing` field so the bundle path gets it too.
*   **`begin_from(&mut self, rendered_before_correction: State)`**: start easing from where the entity was last drawn. Call it right after a reconciliation. The caller decides whether to: a large jump means a real desync and is better snapped, so gate this on a distance threshold.
*   **`advance(&mut self, dt_secs: f32)`**: progress the ease by a frame.
*   **`sample(&self, logical: &State, lerp: impl Fn(&State, &State, f32) -> State) -> State`**: where to draw this frame. While easing, blends from the captured position toward the live `logical`; otherwise returns `logical` unchanged.
*   **`is_easing(&self) -> bool`**
*   **`reset(&mut self)`**: abandon the ease in progress and draw the logical state directly from this frame. For a genuine discontinuity (a spawn, a respawn, a teleport), where finishing the ease would draw the entity smoothly across the whole jump.

The snap-versus-ease threshold is deliberately not a parameter: check the correction distance yourself and skip `begin_from` for large jumps. The distinction that matters is by **cause**, not magnitude: ease continuous error, snap discontinuities.

**Keep the ease shorter than the send interval.** Measured in `horde_playground`: a 250 ms ease never completes when corrections arrive every 33 ms, so the smoother becomes the dominant error and accuracy gets worse as the send rate rises.

## 7b. Module `ack`

Sliding-window acknowledgement: pure sequence arithmetic, no allocation, no socket.

### Struct `AckWindow`

A record of which recent sequence numbers arrived: the newest, plus a bitmask of the `WINDOW` (64) before it. Bit `i` of the mask stands for `newest - 1 - i`.

*   **`new()`**, **`reset()`**.
*   **`observe(seq: u64) -> bool`**: records an arrival, returning whether it was new. Handles reordering: a straggler arriving after a newer packet lands in its own slot rather than being taken for the new newest.
*   **`encode() -> Option<(u64, u64)>`** / **`from_encoded(newest, mask)`**: the wire form, twelve bytes, and the rebuild on the far side.
*   **`contains(seq) -> bool`**, **`newest() -> Option<u64>`**, **`mask() -> u64`**, **`received_in_window() -> u32`**.
*   **`missing_since(oldest) -> impl Iterator<Item = u64>`**: the gaps, ascending, clamped to the window. What a sender resends. Past the window the data is beyond recovery and the caller should be resynchronising rather than backfilling, so the ask stays bounded no matter how far back it points.
*   **`contiguous_base(first) -> Option<u64>`**: the newest sequence such that everything from `first` up to it arrived. `None` if `first` itself is missing, so a run that cannot be established at all reports nothing rather than a stale previous answer, and the caller resynchronises instead of backfilling.

Fixed size is the whole point: a link losing half its packets reports in the same twelve bytes as a perfect one, and heavy loss is precisely when there is no room for an explicit list.

**Which of the two you want depends on what your protocol does with the answer**, and getting this wrong is silent. A protocol that **retransmits** wants the mask, which names the holes to refill. A protocol that **re-derives** (a delta stream diffing against a state the peer provably reached) wants `contiguous_base`, because receiving packet N+1 after losing N does not put a peer in the state N+1 implies: whatever N announced and N+1 had no reason to repeat is simply gone. Taking the newest set bit hands the diff a state that never existed. Measured in `horde_playground`, that mistake made loss recovery statistically indistinguishable from no recovery at every loss rate.

The argument is the first sequence to *check*, not the newest already known to have arrived. The latter is the natural signature and it is unrepresentable at the start of a protocol numbering from zero, where a caller has to invent "one before zero" and passing `0` reads as "zero already arrived", silently skipping the first packet.

**On choosing it over blind redundancy.** Measured in `rollback_playground`: ack-driven resends cost 28% less than a fixed six-frame tail on a clean link, 45% more at 50% loss, crossing over around 12%. But blind redundancy makes a *fixed number of attempts*; this retries until acknowledged, so it converged at 55% loss where blind did not. Its own bound is how long the sender keeps the payload, not the attempt count.

## 7c. Module `trajectory`

### Struct `TrajectoryPredictor`

Second-order dead reckoning for one scalar: keeps the last three samples, takes velocity from the newest pair and acceleration from the change between pairs, and projects a damped quadratic. Run one per axis.

*   **`new(damping: f32, max_horizon_ms: u64)`**: `damping` scales the acceleration term, `0.0` being plain constant velocity and `1.0` the full quadratic. `max_horizon_ms` clamps how far past the newest sample a prediction may reach; beyond it the projection is evaluated *at* the horizon and held. There is no safe unbounded setting, which is why it is a constructor argument rather than an option.
*   **`observe(time_ms, value)`**: samples at or before the newest are ignored, because a straggler would invert the fitted derivatives and send the prediction backwards.
*   **`predict(time_ms) -> Option<f32>`**: degrades by sample count. `None` before any sample, holds with one, first order with two, the damped curve with three. Interpolates as readily as it extrapolates.
*   **`velocity()`**, **`acceleration()`** (undamped), **`newest_time()`**, **`samples()`**, **`reset()`**.

**When it is worth switching on**, which is narrower than it sounds. The correction goes as the gap squared, so over a short gap it is worth thousandths of a pixel. On a circular path sampled at 10 Hz and coasted through a 100 ms gap it cuts the error 45%; in `netcode_playground` at a normal server rate, where an adaptive buffer keeps the render target within a few milliseconds of the newest snapshot, it changes nothing measurable. It begins to pay below about 10 Hz. Long snapshot intervals are the trigger, not a general preference for the better algorithm.

Scalar rather than generic over a state type on purpose: a curve fit needs arithmetic on the value, and a vector-space bound would fall on every consumer of `RemoteView` for two lines an app can write itself. `netcode_playground` keeps a pair beside each `RemoteView` and overrides the position only when the view was going to dead-reckon anyway.

## 7d. Module `timestep`

Turning however long the last frame took into whole fixed steps, or into "is it time yet". Both sides of a connection stepping the same rule at different step sizes are not running the same simulation, and the drift reads as network jitter, so taking the step from here is what keeps them equal.

### Struct `FixedTimestep`

*   **`from_step_ms(step_ms)`** / **`from_hz(hz)`**: the step size. **Panics on zero.**
*   **`with_max_frame_ms(ms)`**: cap how much elapsed time one `advance` may pay for. Default `DEFAULT_MAX_FRAME_MS` (250 ms, or fifteen steps at 60 Hz): enough that an ordinary hitch catches up smoothly, small enough that a resumed tab skips ahead instead of grinding through the minutes it was asleep.
*   **`advance(&mut self, elapsed_ms) -> Steps`**: an `ExactSizeIterator` yielding the step duration in milliseconds, once per step this frame paid for. The duration is *yielded* rather than assumed so a caller cannot accidentally integrate by the frame delta instead.
*   **`step_ms()`**, **`step_secs()`**, **`set_step_ms(ms)`**, **`pending_ms()`**, **`alpha()`** (the fraction of a step accumulated, for interpolating a render between two simulated states), **`dropped_ms()`**, **`reset()`**.

`dropped_ms` is real time the simulation never ran, because the cap refused it. It is counted rather than discarded on purpose: a world quietly behind wall time explains a whole class of "it desynced and I do not know when".

**If you also keep a clock, advance it by *simulated* time, not wall time.** A packet's timestamp says when its state is from, and clients subtract it to project samples forward, so a clock running ahead of the state it describes has every client integrating into a future the server never simulated. Identical in the normal case, correct in the stalled one.

### Struct `Periodic`

The same accumulator with a different consumption rule, and separate because the two answer different questions. A fixed step asks "how much simulation does this frame pay for", where every step must run or the world falls behind. A period asks "is it time yet", where the work is usually idempotent and running it twice in one frame is waste rather than correctness.

*   **`new(interval_ms)`** / **`from_hz(hz)`**, **`set_interval_ms`**, **`interval_ms()`**, **`remaining_ms()`**, **`reset()`**.
*   **`due(&mut self, elapsed_ms) -> bool`**: fires at most once per advance.
*   **`advance(&mut self, elapsed_ms) -> u32`**: every occurrence, for work that genuinely needs each one.

Subtract the interval rather than zeroing the accumulator, which is what `Periodic` does and what a hand-rolled copy usually forgets: zeroing drops the remainder and makes every period slightly too long.

## 7e. Module `mirror`: the client half of a delta stream

The counterpart to `plaza_server_utils::DeltaBaseline`. A server streaming interest-managed entities sends *entered* and *left* and lets each client keep a mirror; this is the mirror, and the two halves are keyed by the same [`SlotKey`](#7g-modules-slot-and-digest) and checked by the same `SetDigest`.

### Struct `DeltaMirror<Entity>`

Generic over the entity, because what an application keeps per entity (a smoother, interpolation history, a render kind) is its business. The mirror owns only the keying, the agreement and the counters.

*   **`new()`**, **`with_generations(bool)`** / **`set_generations(bool)`**: whether a key names an occupant or only a slot.
*   **`begin(&mut self, seq, full_baseline)`**: start applying a packet. `full_baseline` clears first, for a rebuild.
*   **`insert(key, entity)`**, **`remove(key) -> Option<Entity>`**, **`get`**, **`get_mut`**, **`contains`**.
*   **`settle(&mut self, expected: u64) -> Agreement`**: fold the digest and compare it against the server's. `Agreement::agreed()` for the boolean.
*   **`divergence_from(server_keys) -> Divergence`**: which way it diverged, given the server's own key set. A digest **detects** and cannot **diagnose**, so anything shipping a digest wants a debug mode that ships the ground truth beside it: `missing` means something was lost or never sent, `extra` means a removal never landed.
*   **`digest()`**, **`acks() -> &AckWindow`**, **`applied_seq()`**, **`keys`**, **`iter`**, **`iter_mut`**, **`values`**, **`values_mut`**, **`len`**, **`is_empty`**, **`clear`**.
*   **`frames_lost()`**, **`stale_refs()`**, **`divergences()`**.

**Apply every packet, whatever baseline it names.** The instinct is the opposite, and that instinct is correct for a *relative* delta protocol: if you cannot reach the baseline, discard. These deltas carry absolute values, so applying them is idempotent and applying a superset is harmless, while discarding what you cannot rebase starves the mirror. Measured: an earlier `horde_playground` version discarded, and at 25% loss its mirror emptied out while every agreement check read perfect, because the checks only ran over what had been applied. The `begin`/`insert`/`remove`/`settle` shape only makes sense that way, which is why this is a type and not a doc comment.

**The three counters stay separate on purpose.** Sequence gaps mean the wire lost something. Stale references mean a message named an occupant this mirror no longer holds. Digest divergences are the symptom no counter predicts. "Forty mismatches and zero frames lost" and "forty mismatches and forty frames lost" are different bugs, and collapsing them into one health number is how an earlier investigation nearly went wrong.

## 7f. Module `correction`

### Struct `Correction<State>`

What a reconciliation actually did: the state as it was `seen` before the correction, and the `settled` state after. Two states rather than a distance, deliberately: a distance needs a metric on `State`, which would tax every user for the benefit of the ones wanting telemetry. The caller knows its own units, so the subtraction is its business.

### Struct `CorrectionMonitor`

A running picture of prediction error, and an adaptive test for what counts as abnormal.

*   **`new()`**, **`with_warmup(samples)`**, **`with_smoothing(alpha)`**, **`with_sigma(sigma)`**, **`with_floor(floor)`**.
*   **`record(magnitude) -> bool`**: fold in a sample, returning whether it is abnormal.
*   **`is_abnormal(magnitude)`** (without recording), **`threshold()`**, **`band()`**, **`norm()`**, **`peak()`**, **`counts() -> (u64, u64)`**, **`is_warming_up()`**, **`reset()`**.

**There is no fixed normal, which is the whole reason this exists.** A thirty pixel correction is unremarkable at one send rate and alarming at another, and the same is true across latency settings and across how much contact the simulation is currently in. A constant threshold reports whatever it happened to be tuned against: it goes quiet exactly when conditions change, and noisy for reasons that have nothing to do with a bug. Tracking the mean and variance of the corrections it is fed keeps its meaning as conditions move underneath it.

**Cold start is a decision, not an accident**, which is what `with_warmup` is for. A baseline initialised to zero says every early correction is enormous, so the monitor alarms loudest at startup, when it knows least.

## 7g. Modules `slot` and `digest`

The vocabulary both sides of a streamed entity set have to agree about. They live in this crate, and `plaza_server_utils` re-exports them, because a browser client needs both and must not inherit a server to get them. Two implementations that agree today are a disagreement waiting to happen, and the failure would present as a divergence about the *world* rather than about the arithmetic.

### Struct `SlotKey`

A storage slot and the generation of its current occupant. **`new(index: u32, generation: u16)`**, **`encode() -> u64`** (`(index << 16) | generation`, the key space `SetDigest`, `DeltaBaseline` and `DeltaMirror` all work in), **`decode(u64)`**, **`ungenerational()`**, **`same_occupant(other)`**, plus `From` both ways.

### Struct `SlotAllocator`

Hands out `SlotKey`s over a dense index space, recycling freed slots and bumping a generation so stale handles stay detectable. **`alloc`**, **`free`**, **`is_live`**, and `ReusePolicy`.

*   **It does not store your entities.** Keep them in a `Vec<T>` indexed by `SlotKey::index`, which is what the rest of these crates expect anyway: `VisibilitySet` takes dense `u32` indices, and the delta types key on `SlotKey::encode`. An allocator that owned the payload would force an application to restructure around it and would still compose with neither.
*   **The generation bumps on free, not on allocate.** A handle should stop naming anything the moment its subject dies, not whenever something happens to want the index. The gap between those two moments is exactly the window a delta stream re-derives retractions in.
*   **`ReusePolicy::{Lifo, Fifo}` is public contract, not an implementation detail**, because it decides how *clustered* recycled indices are, and that decides which wire encoding is cheapest for a despawn set. Measured: under `Lifo` a burst of 233 despawns was 204 separate runs (mean run length **1.14**), which is why run-length encoding lost decisively to delta-varint there. Neither policy is more correct; if something downstream cares about clustering, measure rather than assume.
*   **The ceiling, stated rather than assumed.** A `u16` generation wraps after 65,536 reuses of one slot and nothing can detect the wrap. The width is the mitigation, with `Fifo` available to spread reuse across the index space instead of hammering the same slots.

**When a generation earns its keep** is narrower than it sounds and was measured both ways. Under ordered delivery with each death announced explicitly before the next diff, `horde_playground` recorded **zero** stale handle references across 413 kills with slots actively recycling. It became load-bearing again the moment loss recovery re-derived a retraction *after* a slot may have been recycled. The rule reconciling the two: a generation insures against a slot being reused between the moment you name an entity and the moment the other side reads the name, so anything that widens that window (unordered delivery, loss, re-derivation, a client applying late) brings it back.

### Struct `SetDigest`

An order-independent digest of a set of `u64` keys, maintainable incrementally. **`new`**, **`from_keys`**, **`insert`**, **`remove`**, **`clear`**, **`len`**, **`is_empty`**, **`digest() -> u64`**.

A delta-relevance stream has a silent failure mode: the client applies `entered`/`left`, and if one delta is lost, malformed or misapplied, the mirror is wrong **for good**, with no symptom. Bandwidth looks normal, positions look normal, and the only evidence is on the screen. Both sides summarise their set and compare.

Order independence is the requirement that shapes it: two peers holding the same set may iterate it in different orders. Summation gives that, and unlike XOR it does not silently cancel duplicates (a double-insert is itself a mistake worth catching). Because the combine is addition, a key can be added or removed in O(1), so a client maintains its digest as entities enter and leave rather than rehashing every tick. The key is a `u64` you choose, which is the important flexibility: hash a bare index to check *membership*, or pack index with generation to check that both sides agree on the *occupant*.

`VisibilitySet::digest()` computes the same value over a bitset's membership.

## 7h. Module `coalesce`

### Struct `InputCoalescer<Input>`

Send an input on change, plus a keepalive. **`new(keepalive_ms)`**, **`should_send(&input, now_ms) -> bool`**, **`set_enabled`**, **`is_enabled`**, **`last_sent`**, **`reset`**.

**The keepalive is the content.** Sending purely on change means a *dropped* direction change is not a missing update but a **wrong state that persists**, because the server holds the last direction it received: the player keeps gliding until they press something else. It is intermittent and it reads as the controls sticking rather than as packet loss.

Pairs with [`HeldInputPredictor`](#struct-heldinputpredictorstate-input-ctx) and explicitly **not** with `PredictedPlayer`, whose server consumes one input per step and therefore needs every one of them.

## 7i. Module `playout`: the playout queue and the resume verdict

A client that renders in the past does not apply packets on arrival: it queues them and plays each one out when the render clock reaches the instant it describes. That queue is fed by a remote peer and drained by a local clock, and the two disagree in exactly three ways, each of which this module answers: a packet a little late (an underrun, counted and still delivered, because late is better than starved), a packet absurdly ahead or a queue overflowing (a discontinuity: the local clock stopped while the world kept going, and the only honest move is to snap), and the transport's own verdict that the timeline is lost (a discarded resume backlog).

Two counting rules live here because each was learned from a misleading panel. An underrun is jitter-scale lateness only; a packet late by more than the discontinuity threshold belongs to a lost timeline, which `restarts` accounts for, and charging it as an underrun made one stall read as a thousand link faults. And a restart is counted once per discontinuity, wherever it was detected, so the panel's number is stalls survived rather than a function of how large each backlog happened to be.

### Enum `Admission`

What `push` concluded. **`Queued`** (nothing to do until the clock reaches it) or **`TimelineLost`**. `#[must_use]`: `TimelineLost` obliges the caller to restart its own timeline, re-anchor its render clock on what just arrived, and drop derived state (its entity mirror above all), so the stream's own recovery rebuilds it; see the resume contract in section 1.

### Struct `PlayoutBuffer<T>`

*   **`new(max_queued: usize, lost_ahead: u64) -> Self`**: `max_queued` bounds the queue absolutely; size it several times past what an honest buffer holds at the deepest render delay and fastest send rate, so reaching it means something is wrong rather than merely slow. `lost_ahead` is the discontinuity threshold, how far past the render instant an arrival may reach before the client is lost rather than buffering; match it with the server's stalled-subscriber threshold so both sides agree on when a gap stops being jitter.
*   **`push(&mut self, stamp: u64, order: u64, item: T, render_at: Option<u64>) -> Admission`**: takes delivery. `stamp` is the instant the packet describes, `order` its sequence number (play-out is ordered by sequence, so deltas compose in the order the server built them even when arrivals interleave), `render_at` the instant currently being drawn, `None` before the timeline has started (a join burst must be admitted whole: nothing can be late and nothing can be a discontinuity).
*   **`pop_due(&mut self, render_at: u64) -> Option<T>`**: the oldest packet whose instant the clock has reached, in sequence order. Call in a loop each tick until `None`.
*   **`timeline_lost(&mut self)`**: the transport's verdict arriving from outside (a resume backlog discarded unread, a reconnect). Drops everything but the newest packet, which is what the caller's restarted clock anchors on.
*   **`iter(&self) -> impl Iterator<Item = &T>`**: the packets held but not yet due, in sequence order, for readouts that look into the future the buffer holds (a ghost overlay, a queue panel).
*   **`underruns() -> u64`** (packets that arrived after their instant had been drawn, by a margin jitter produces: the number that says the render delay is too small for this link), **`restarts() -> u64`** (discontinuities survived, one per stall however large the backlog), **`len()`**, **`is_empty()`**.

## 7j. Module `arrival`: measuring how a stream actually arrives

The render-delay budget has three terms: the link's one-way delay, the spread in it, and one send interval (so two samples always bracket the interpolation target). A host that simulates its own link can compute that from its sliders; a real client cannot, because nothing tells it the send rate or the delay. It can only measure, and measuring is better anyway: the server is then free to change its rate live, and a client that trusts a configured rate is wrong exactly when the rate is being changed, which is when it matters.

Two measurement decisions are load bearing. **The buffer covers irregularity, not delay**: a steady 200 ms link needs no more buffer than a steady 20 ms one, because a constant delay just shifts the whole timeline, so the jitter term is the smoothed mean deviation of lateness (what RFC 6298 uses for the same job), not the lateness itself. **The interval is measured between declared stamps, not arrivals**: two packets can arrive in one poll and still describe moments an interval apart, and gaps between their declared times are stable where gaps between their arrivals are noise. Keep one monitor per interpolated stream: it is the streams peers are interpolated between that size the buffer, not the ones simulated forward from single samples.

### Struct `ArrivalMonitor`

*   **`new(smoothing: f32) -> Self`**: the EWMA weight for new observations, `0..=1`; around `0.05` follows a link's drift without chasing individual packets.
*   **`observe(&mut self, stamp: u64, recv: u64)`**: notes one arrival. `stamp` is the declared server time the packet describes; `recv` is the client's synced estimate of server time at arrival (the same clock its render delay is subtracted from, which is what makes the readings commensurable with the delay). Call for every packet of the stream, reordered or not: a stamp older than the newest seen still updates lateness (it is late, that is data) but never the interval, which is measured forward only.
*   **`interval_ms() -> f32`**: the smoothed gap between declared stamps, the send interval as it actually is.
*   **`lateness_ms() -> f32`**: the smoothed mean lateness; with an honest clock sync, the link's one-way delay plus whatever error the sync carries, which is exactly what the budget must absorb anyway.
*   **`jitter_ms() -> f32`**: the smoothed mean deviation of lateness, the irregularity the buffer exists to cover.
*   **`needed_delay_ms() -> f32`**: the render delay this stream needs, measured: lateness plus jitter plus one interval. Compare against the delay in force and warn when short; whether to *adapt* to it is the application's decision, because a delay that follows the link hides bad links instead of reporting them (the timeline should come from declaration, not arrival).
*   **`warmed_up() -> bool`**: whether at least two forward stamps have been seen, so an interval exists and the readings mean anything.

## 8. Module `math`

Small vector and quaternion types, so this crate is usable without pulling in a math library. Implement `Interpolatable` and `Extrapolatable` for your own types instead if you already have `glam` or `nalgebra`.

*   **`Vec2`**: `new`, `ZERO`, `ONE`, `length`, `length_squared`, `dot`, `normalize`, plus `Add`/`Sub`/`Mul` operators.
*   **`Vec3`**: the same surface in three dimensions.
*   **`Quat`**: `new`, `IDENTITY`, `normalize`, `dot`, `multiply` (Hamilton product, composing rotations), `slerp` (shortest-path spherical interpolation).

These implement `Interpolatable` and `Extrapolatable`, so they work with the buffers above out of the box.

> This overlaps `plaza::common::math` intentionally. Keeping this crate free of
> workspace dependencies matters more than sharing sixty lines of plain data.

## 8b. Module `net_sim` (feature `net-sim`)

A deterministic latency / jitter / loss queue, so prediction and reconciliation are testable at all. Opt-in, because it is a test and demo aid rather than core client API.

### Struct `LatencyLink<T>`

*   **`new()`**, **`with_ordering(Ordering)`**, **`send(now_ms, packet, latency_ms, jitter_ms, loss_pct, &mut Rng)`**, **`drain_due(now_ms) -> Vec<T>`**, **`in_flight()`**.
*   **`Ordering::Ordered`** (the default) clamps each delivery time to at least the previous one, so jitter delays a packet past its predecessor but never ahead of it. **`Ordering::Unordered`** is the datagram case.

**Impairment tooling must be faithful to the transport it stands in for**, and the default is ordered because that is what TCP and WebSocket are. An unclamped queue reorders under jitter, which manufactures a failure mode the real transport cannot produce: a full diagnostic cycle here went into a reordering hypothesis that WebSocket makes impossible. Worse than the wasted time, it hides the fact that the real system has stronger guarantees than the tests assume. At one example's shipped defaults (15 ms of jitter against a ~16 ms send interval) the unclamped version could hand a client an older frame after a newer one, which an order-sensitive spawn/despawn stream has no tolerance for.

`LatencyLink` is `Clone`, which is load-bearing rather than incidental: a plaza state must be `Clone`, and a primitive that cannot sit inside application state gets reimplemented, which is how this fix once lived in an example instead of the library. **Derives are part of the API contract.**

### Struct `Rng`

A seeded, reproducible generator: **`new(seed)`**, **`unit() -> f32`**, **`up_to(n) -> u64`**. It is a test and demo aid; it is deliberately not a "deterministic shared stream" block, because identical seeds fed divergent inputs still diverge.

## 8c. Module `fixed` (feature `fixed`)

Fixed-point arithmetic, for a wire that carries causes instead of state. A wire that sends positions forgives arithmetic (a client wrong by one part in a million is corrected on the next frame); a wire that sends nothing but a seed and the inputs is never corrected, and `f32` cannot be relied on to give the same answer in a wasm build and a native one: compilers may contract a multiply-add, widen an intermediate, or reassociate a sum, and one last bit fed back into a position every tick is a visible gap in twenty seconds.

### Struct `Fx` and struct `P`

**`Fx(pub i32)`**: signed 32-bit fixed point, 24 integer bits and 8 fractional (`FRAC_BITS`, `ONE`). Serialized as the raw `i32` (`serde(transparent)`), so a snapshot carries exactly what the simulation holds.

*   **`ZERO`**, **`ONE`**, **`from_int(n)`**, **`ratio(num, den)`**, **`to_int()`**.
*   **`to_f32()`**: the only float in the vocabulary, one way, for the renderer. Nothing in a simulation may call it.
*   **`mul`**, **`div`** (carried in `i64` so the intermediate cannot overflow; truncating, because rounding has a tie case two implementations can disagree about), **`abs`**, **`min`**, **`max`**, and `Add`/`Sub`/`Neg`/`AddAssign` (wrapping).
*   **`sqrt()`**: defined as the largest `r` with `r*r <= n`, a property of the input alone. Newton to get close, then a correction to the definition, because "iterate until it stops changing" does not terminate for integer Newton (it can settle into a two-value cycle) and "iterate N times" makes the answer a function of N.

**`P { x: Fx, y: Fx }`**: a point. **`new`**, **`from_ints`**, **`dist_sq`** (what a range check wants: comparing squares removes the square root, and with it an implementation, from the path), **`dist`**.

## 9. Putting It Together

```rust,ignore
// Local input: apply now, record for replay.
predicted.apply_local_input_and_predict(&op, seq, &mut inputs, apply_move);
send_to_server(SequencedClientInput { sequence_number: seq, input_data: op });
seq += 1;

// Server reply: snap to truth, replay what it had not seen.
predicted.reconcile_with_server_state(
  update.authoritative_player_state,
  update.last_processed_input_seq,
  &mut inputs,
  apply_move,
);

// Remote entities: render slightly in the past.
remote.add_snapshot(snapshot.server_time, snapshot.into_state());
let render_time = estimated_server_time.saturating_sub(INTERPOLATION_DELAY);
if let Some(state) = remote.get_interpolated_state(render_time) {
  draw(state);
}
```

`examples/csp_net_example` in the workspace runs both halves (a predicting client against an authoritative server), over a simulated network.
