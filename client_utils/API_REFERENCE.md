# API Reference: `plaza_client_utils`

`plaza_client_utils` is the **client half** of real-time networking. The server half (input sequence tracking, delayed input buffers, lag-compensation rewind) lives in `plaza::game_common::reconciliation`

## Contents

- [1. Core Types](#1-core-types)
  - [Type Aliases](#type-aliases)
- [2. Drop-in entities](#2-drop-in-entities)
  - [Struct `PredictedPlayer<State, Input, Ctx>`](#struct-predictedplayerstate-input-ctx)
  - [Struct `HeldInputPredictor<State, Input, Ctx>`](#struct-heldinputpredictorstate-input-ctx)
  - [Struct `RemoteView<State, Velocity>`](#struct-remoteviewstate-velocity)
  - [Struct `HermiteView<State, Velocity>`](#struct-hermiteviewstate-velocity)
  - [Struct `AdaptiveDecay`](#struct-adaptivedecay)
- [3. Rollback netcode (deterministic lockstep)](#3-rollback-netcode-deterministic-lockstep)
  - [Struct `StateHistory<State: Clone>`](#struct-statehistorystate-clone)
  - [Struct `InputTimeline<Input: Clone + Debug>`](#struct-inputtimelineinput-clone-debug)
  - [Struct `RollbackSession<State, Input>`](#struct-rollbacksessionstate-input)
- [4. Prediction and Reconciliation](#4-prediction-and-reconciliation)
  - [Struct `PredictedEntity<StateType, Op>`](#struct-predictedentitystatetype-op)
  - [Struct `ClientInputBuffer<Op, PredictedStateSnapshot>`](#struct-clientinputbufferop-predictedstatesnapshot)
  - [Struct `BufferedInput<Op, PredictedStateSnapshot>`](#struct-bufferedinputop-predictedstatesnapshot)
- [5. Interpolation](#5-interpolation)
  - [Struct `SnapshotBuffer<Timestamp, StateType>`](#struct-snapshotbuffertimestamp-statetype)
  - [Struct `InterpolationClock<T>`](#struct-interpolationclockt)
  - [Struct `Timeline` and struct `Probe`](#struct-timeline-and-struct-probe)
  - [Struct `RttEstimator`](#struct-rttestimator)
  - [Struct `ClockSyncEstimator`](#struct-clocksyncestimator)
  - [Struct `ScalarKalman`](#struct-scalarkalman)
  - [Trait `Interpolatable<Timestamp>`](#trait-interpolatabletimestamp)
  - [Trait `ToF32`](#trait-tof32)
  - [Struct `ServerSnapshot<Timestamp, StateType>`](#struct-serversnapshottimestamp-statetype)
- [6. Extrapolation](#6-extrapolation)
  - [Struct `ExtrapolationBase<StateType, VelocityType, ServerTimestamp>`](#struct-extrapolationbasestatetype-velocitytype-servertimestamp)
  - [Trait `Extrapolatable<VelocityType, TimeDelta>`](#trait-extrapolatablevelocitytype-timedelta)
- [7. Smoothing](#7-smoothing)
  - [Struct `ErrorSmoother<State>`](#struct-errorsmootherstate)
- [8. Module `ack`](#8-module-ack)
  - [Struct `AckWindow`](#struct-ackwindow)
- [9. Module `trajectory`](#9-module-trajectory)
  - [Struct `TrajectoryPredictor`](#struct-trajectorypredictor)
- [10. Module `timestep`](#10-module-timestep)
  - [Struct `FixedTimestep`](#struct-fixedtimestep)
  - [Struct `Periodic`](#struct-periodic)
- [11. Module `mirror`: the client half of a delta stream](#11-module-mirror-the-client-half-of-a-delta-stream)
  - [Struct `DeltaMirror<Entity>`](#struct-deltamirrorentity)
- [12. Module `correction`](#12-module-correction)
  - [Struct `Correction<State>`](#struct-correctionstate)
  - [Struct `CorrectionMonitor`](#struct-correctionmonitor)
- [13. Modules `slot` and `digest`](#13-modules-slot-and-digest)
  - [Struct `SlotKey`](#struct-slotkey)
  - [Struct `SlotAllocator`](#struct-slotallocator)
  - [Struct `SetDigest`](#struct-setdigest)
- [14. Module `coalesce`](#14-module-coalesce)
  - [Struct `InputCoalescer<Input>`](#struct-inputcoalescerinput)
- [15. Module `playout`: the playout queue and the resume verdict](#15-module-playout-the-playout-queue-and-the-resume-verdict)
  - [Enum `Admission`](#enum-admission)
  - [Struct `PlayoutBuffer<T>`](#struct-playoutbuffert)
- [16. Module `arrival`: measuring how a stream actually arrives](#16-module-arrival-measuring-how-a-stream-actually-arrives)
  - [Struct `ArrivalMonitor`](#struct-arrivalmonitor)
- [17. Module `math`](#17-module-math)
- [18. Module `net_sim` (feature `net-sim`)](#18-module-netsim-feature-net-sim)
  - [Struct `LatencyLink<T>`](#struct-latencylinkt)
  - [Struct `Rng`](#struct-rng)
- [19. Module `fixed` (feature `fixed`)](#19-module-fixed-feature-fixed)
  - [Struct `Fx` and struct `P`](#struct-fx-and-struct-p)
- [20. Error Handling](#20-error-handling)
  - [Enum `ClientUtilError`](#enum-clientutilerror)

## 1. Core Types

### Type Aliases

*   **`SequenceNumber = u64`**: identifies one client input, incrementing per input.
*   **`ClientTimeMs = u64`**: client-local milliseconds since some fixed origin.

## 2. Drop-in entities

Three types bundling the primitives, which stay public.

### Struct `PredictedPlayer<State, Input, Ctx>`

For a server that consumes **one input per simulation step**: `PredictedEntity` + `ClientInputBuffer` + `ErrorSmoother` + a sequence counter. `apply` and `lerp` are `fn` pointers, so no closure bounds are imposed.

*   **`new(initial, PlayerConfig, apply: fn(&mut State, &Input, &Ctx), lerp: fn(&State, &State, f32) -> State)`**
*   **`input(&mut self, input) -> SequenceNumber`**: predict locally, buffer for replay, return the sequence to send.
*   **`reconcile(&mut self, authoritative, acked_seq) -> Correction<State>`**: snap the logical state to authority, replay unacknowledged inputs, begin easing the visible correction. See [`Correction`](#12-module-correction).
*   **`advance(&mut self, dt_secs)`**, **`render() -> State`** (eased), **`logical() -> &State`** (exact), **`authoritative() -> &State`**, **`latest_seq()`**, **`acked_seq()`**, **`unacked_count()`**.
*   **`set_active(&mut self, bool)`** / **`is_active()`**: an inactive predictor stops integrating and stops correcting.
*   **`teleport(&mut self, State)`**: a discontinuity, applied without easing.
*   **`set_context(&mut self, Ctx)`** / **`context() -> &Ctx`**: the world the rule reads. `Ctx` defaults to `()`.

**`PlayerConfig`**: `input_buffer: usize`, `smoothing_secs: f32` (`0.0` disables smoothing), `easing: fn(f32) -> f32` (default `linear`). `Default` is 256 / 0.1s / linear.

Keep `smoothing_secs` shorter than the send interval.

### Struct `HeldInputPredictor<State, Input, Ctx>`

For a server that **holds an input and integrates it every tick**. Nothing is replayed: it dead reckons under the held input and eases a fixed fraction of the error toward each authoritative sample.

*   **`new(initial, HeldInputConfig, advance: fn(&mut State, &Input, f32, &Ctx), lerp: fn(&State, &State, f32) -> State)`**: `advance` takes `dt` in seconds.
*   **`with_teleport(self, distance: fn(&State, &State) -> f32, beyond: f32)`**: past `beyond`, `reconcile` snaps instead of easing. The metric is an argument rather than a trait bound, so no geometry is imposed on `State`.
*   **`hold(&mut self, input)`** / **`held() -> &Input`**.
*   **`advance(&mut self, dt_secs)`**: integrate locally, then progress the ease.
*   **`project(&self, authoritative: &State, age_secs: f32) -> State`**: where that sample would be now under the held input.
*   **`reconcile(&mut self, authoritative, age_secs) -> Correction<State>`**: project the sample forward by its own age, then ease toward it.
*   **`render()`**, **`logical()`**, **`set_active`**, **`is_active`**, **`teleport`**, **`set_context`**, **`context`**: as above.

**`HeldInputConfig`**: `blend` alone, the fraction of the remaining gap closed per `reconcile`. `1.0` snaps, `0.0` is pure dead reckoning and drifts without bound. `Default` is `0.25`.

### Struct `RemoteView<State, Velocity>`

A `SnapshotBuffer` plus the interpolate / extrapolate / hold decision, with buffer-starvation detection. Time is `u64` milliseconds or ticks; for a `Duration` timeline compose `SnapshotBuffer` and `ExtrapolationBase` directly. Requires `State: Interpolatable<u64> + Extrapolatable<Velocity, f32>`.

*   **`new(buffer_size, max_extrapolation_ms)`**: `buffer_size` >= 2.
*   **`push(&mut self, time_ms, state, velocity)`**.
*   **`render(&self, target: Option<u64>, RenderOpts) -> Option<State>`**: `None` until the first push. Interpolated at `target`, dead-reckoned when the buffer has starved (if `opts.extrapolate`), or the raw newest (if `opts.interpolate` is false).
*   **`latest() -> Option<&State>`**.
*   **`over_extrapolations() -> u64`**: renders that asked past `max_extrapolation_ms` and were served the capped coast. Accumulates what each render's `ExtrapolationBase` found, since that base is built per call.
*   **`oldest_timestamp() -> Option<u64>`**: the oldest instant still interpolatable. A `render` target before this is **clamped to the oldest snapshot** silently.

**`RenderOpts`**: `interpolate: bool`, `extrapolate: bool`. `Default` is both on.

### Struct `HermiteView<State, Velocity>`

A cubic Hermite spline through both samples, leaving along the velocity recorded at each. Requires `State: HermiteInterpolatable<Velocity>`, implemented here for `f32`, `Vec2` and `Vec3`. An orientation wants `Quat::slerp`; a quaternion's components do not interpolate independently.

*   **`new(capacity)`**: **panics below 2.**
*   **`push(&mut self, time_ms, state, velocity)`**: out-of-order arrivals are inserted in time order rather than dropped.
*   **`render(&self, target_ms) -> Option<State>`**: `None` until the first sample; before the oldest or past the newest it **holds** that end rather than guessing.
*   **`latest()`**, **`oldest_time()`**, **`latest_time()`**, **`len()`**, **`is_empty()`**, **`clear()`**.

**Trait `HermiteInterpolatable<Velocity>`**: `hermite(&self, other, velocity_a, velocity_b, t, seconds) -> Self`, where `t` runs `0..=1` across the segment and `seconds` is its wall duration. **`hermite_scalar(p0, v0, p1, v1, t, seconds) -> f32`** is the one-axis form.

### Struct `AdaptiveDecay`

Per-frame correction decay whose rate depends on the size of the error. This is the rate, not the state: keep your own offset, multiply it by `retain` each frame, add it to what you draw.

*   **`new(small, large, small_at, large_at)`**. **`Default`**: keep 0.95 of the error per frame at or below 0.25 units, 0.85 at or above 1.0, blended between.
*   **`retain(&self, magnitude, dt_secs) -> f32`**: the fraction of an error that size to keep after `dt_secs`. Framerate-independent.

## 3. Rollback netcode (deterministic lockstep)

Module `rollback`. **`Frame = u64`**: a logical simulation frame. Rollback counts in fixed frames, never wall-clock time.

### Struct `StateHistory<State: Clone>`

A frame-indexed ring of whole-world snapshots. Pure save/restore by frame, no interpolation.

*   **`new(capacity)`**: keeps the most recent `capacity` frames, the maximum rollback distance. **Panics if 0.**
*   **`save(&mut self, frame, state)`**: contiguous use only (append `latest + 1`, or overwrite a frame in the window). A save that skips ahead resets the window rather than leaving a gap.
*   **`restore(&self, frame) -> Option<State>`**: `None` if evicted or never saved.
*   **`oldest_frame()`**, **`latest_frame()`**, **`len`**, **`is_empty`**, **`clear`**.
*   **`resets() -> u64`**: saves that fell outside the window and reset it. Non-zero means the window was rebuilt from one frame, which shortens how far back the session can roll.

### Struct `InputTimeline<Input: Clone + Debug>`

The inputs known for one source, by frame, with unconfirmed frames left for the session to predict.

*   **`new(capacity)`**: retain inputs across `capacity` frames. **Panics if 0.**
*   **`confirm(&mut self, frame, input)`**: out-of-order arrivals are fine; a frame past the retained window is dropped.
*   **`confirmed_at(frame) -> Option<&Input>`**: `None` if that frame is still predicted.
*   **`last_confirmed_at_or_before(frame) -> Option<&Input>`**, **`last_confirmed_frame() -> Option<Frame>`**.

**`fn repeat_last_input(last, frame) -> Input`**: the default predictor.

### Struct `RollbackSession<State, Input>`

Requires `State: Clone + Debug`, `Input: Clone + Debug + PartialEq`.

*   **`new(initial_state, neutral_inputs: Vec<Input>, RollbackConfig, advance: fn(&State, &[Input]) -> State)`**: `neutral_inputs.len()` is the player count; `neutral_inputs[p]` is what player `p` is assumed to hold before any of its inputs are known. `advance` is the deterministic step, the same on every peer.
*   **`with_predictor(self, fn(&Input, Frame) -> Input) -> Self`**.
*   **`queue_local_input(&mut self, player, input)`**: the local player's input for the current frame.
*   **`confirm_remote_input(&mut self, player, frame, input)`**: marks for rollback if it contradicts a guess already used.
*   **`advance_frame(&mut self)`**: roll back and re-simulate if a guess was disproved, then simulate the current frame.
*   **`state() -> &State`**: the present, including predicted inputs. **`state_at(frame) -> Option<State>`**: identical across peers for a fully-confirmed frame.
*   **`is_frame_confirmed(frame) -> bool`**, **`set_rollback_enabled(&mut self, bool)`**.
*   **`confirmed_frame(player)`**, **`prediction_horizon()`**, **`current_frame()`**, **`num_players()`**, **`last_rollback_frames()`**, **`max_rollback_frames()`**, **`rollback_count()`**.

**`RollbackConfig`**: `max_rollback_frames: usize`, which must exceed the worst latency in frames. `Default` is 240.

## 4. Prediction and Reconciliation

### Struct `PredictedEntity<StateType, Op>`

*   **`new(initial_state: StateType) -> Self`**
*   **`apply_local_input_and_predict(&mut self, op: &Op, sequence_number: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_op_fn: &impl Fn(&mut StateType, &Op))`**
*   **`reconcile_with_server_state(&mut self, new_authoritative_state: StateType, server_ack_input_seq: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_op_fn: &impl Fn(&mut StateType, &Op))`**
*   **Public fields**: `current_predicted_state`, `last_authoritative_state`, `last_server_acknowledged_input_seq`. `Clone` whenever `StateType: Clone`, with no bound on `Op`.

`apply_op_fn` must be the same rule the server applies.

### Struct `ClientInputBuffer<Op, PredictedStateSnapshot>`

*   **`new(max_size: usize) -> Self`**: **panics if 0.**
*   **`record_input(&mut self, sequence_number, op, state_before_op_predicted)`**: at capacity the oldest is discarded, with a `warn`.
*   **`acknowledge_inputs_up_to(&mut self, ack_sequence_number)`**.
*   **`get_unacknowledged_inputs(&self, last_acknowledged_sequence_number) -> impl Iterator`**: every buffered input above the argument, in order.
*   **`get_predicted_state_before_input(&self, sequence_number) -> Option<&PredictedStateSnapshot>`**.
*   **`len`**, **`is_empty`**, **`clear`**.
*   **`overflowed() -> u64`**: inputs discarded because the buffer was full. Non-zero means a reconciliation can no longer replay everything unacknowledged. Size the buffer at input rate times worst round trip.

### Struct `BufferedInput<Op, PredictedStateSnapshot>`

One recorded input: its sequence number, the op, and the state before it applied.

## 5. Interpolation

### Struct `SnapshotBuffer<Timestamp, StateType>`

*   **`new(max_buffer_size: usize) -> Self`**: **panics below 2.**
*   **`add_snapshot(&mut self, server_timestamp, state)`**: keeps chronological order, inserting late arrivals in position. Over capacity the oldest is dropped.
*   **`get_interpolated_state(&self, target_render_time) -> Option<StateType>`**: `None` only when empty. A target outside the buffer clamps to that end rather than extrapolating.
*   **`oldest_timestamp`**, **`latest_timestamp`**, **`len`**, **`is_empty`**, **`clear`**.

### Struct `InterpolationClock<T>`

Supplies the `target_render_time` `get_interpolated_state` needs. `T` is whatever timeline the snapshots use.

*   **`new(delay: T) -> Self`**: render this far behind the estimated server clock.
*   **`observe(&mut self, server_time: T)`**: the first call starts the clock; later calls are ignored.
*   **`advance(&mut self, dt: T)`**, **`started(&self) -> bool`**.
*   **`target(&self) -> Option<T>`**: `now - delay`, or `None` before the first `observe`. Clamped so it never precedes the timeline's zero.
*   **`resync(&mut self, newest_server_time_ms: u64, strength: f32)`** (on `InterpolationClock<u64>`): steers the estimate toward the newest server time by `strength` in `[0, 1]`. `0.1` is smooth, `1.0` snaps.
*   **`observe_rate(&mut self, newest_server_time_ms: u64, max_rate_adjust: f32)`** + **`advance_scaled(&mut self, dt_ms: u64)`** + **`playback_rate() -> f32`** (on `InterpolationClock<u64>`): adjusts the estimate's *speed* rather than its position. Drift is normalized by the render delay; `max_rate_adjust` bounds how far from real time it goes. Pair `observe_rate` (per packet) with `advance_scaled` (per frame). **Pick one of `resync` or `observe_rate`, not both.**
*   **`delay()`** / **`set_delay(delay)`**.

### Struct `Timeline` and struct `Probe`

```rust
let probe = timeline.begin(now);          // stamp your Kind::Ping with probe.sent_at
timeline.complete(probe, now, pong.responder);   // feeds both estimators
```

*   **`begin(now) -> Probe`**, **`complete(probe, now, responder: Option<u64>) -> bool`**: `false` when the probe was discarded. With a `responder` the clock fit gets an exchange too; without one, only the round trip is recorded.
*   **`on_reconnect()`**: invalidates measurements in flight, keeps what has been learned. **`on_resume()`**: invalidates both. **`epoch()`**.
*   **`note_stamp(stamp_ms, now_ms)`**, **`newest_stamp_ms()`**: a timestamp the server wrote into a message. Survives both `on_reconnect` and `on_resume`.
*   **`server_time_ms(now_ms) -> u64`**: the fitted clock, falling back to `now_ms` until two exchanges are in, **floored by the newest stamp carried forward at wall rate**. The floor only ever lifts the estimate, never past the truth.
*   **`with_estimators(RttEstimator, ClockSyncEstimator)`**: `new()` is this with a default `RttEstimator` and a `ClockSyncEstimator` over `CLOCK_WINDOW`.
*   **`rtt`** and **`clock`** are public.

A probe carries the epoch it started in, and one that outlives its epoch is discarded rather than recorded.

### Struct `RttEstimator`

**No unit is named or assumed.** Samples go in as whatever you stamped a probe with and every number comes back in that same unit. Mixing two units across one estimator is the only way to get a wrong answer, and no signature can stop you.

*   **`new(alpha)`** / **`Default`** (alpha 0.1): each sample's moving-average weight.
*   **`rtt()`**, **`one_way()`** (half the RTT), **`min_rtt()`**, **`jitter()`** (smoothed mean deviation). Each `None` before the first sample.
*   **`observe(sample)`**, **`observe_pong(origin, now)`** (the round trip is `now - origin`), **`clear()`**.

### Struct `ClockSyncEstimator`

Fits the client-to-server clock **offset and its skew** by least squares over a sliding window. `f64` times.

*   **`new(window: usize)`**: 16 to 64 typical. **Panics below 2.**
*   **`observe(local, offset)`**: a measured `offset = server - local` at local time `local`.
*   **`observe_exchange(local_send, remote_recv, local_recv)`**: derives the offset under the symmetric-delay assumption. `remote_recv` is `Pong.responder`.
*   **`offset_at(local) -> Option<f64>`**, **`server_time_at(local) -> Option<f64>`**, **`skew() -> f64`** (drift per unit time; `x 1e6` for ppm), **`is_ready()`**, **`sample_count()`**, **`clear()`**.

**Both ends must mean the same unit.** Unlike `RttEstimator`, this compares your clock against someone else's, so a disagreement about the unit produces a confident wrong answer rather than a visible one.

A round trip cannot recover the *asymmetric* one-way offset without an external time source. The regression buys the drift rate cleanly, not the asymmetric constant.

### Struct `ScalarKalman`

A one-dimensional Kalman filter. Two knobs, `f32`.

*   **`new(process_noise: f32, measurement_noise: f32)`**: `process_noise` (`Q`) is how much the true value wanders between samples (higher trusts measurements more); `measurement_noise` (`R`) is how noisy each reading is (higher smooths harder). The first `observe` seeds the estimate.
*   **`with_initial(estimate, variance)`**.
*   **`observe(measurement) -> f32`**, **`estimate()`**, **`variance()`**, **`last_gain()`** (near 1 while settling, near 0 once settled), **`is_initialized()`**, **`set_process_noise(q)`** / **`set_measurement_noise(r)`**, **`reset()`**.

### Trait `Interpolatable<Timestamp>`

```rust,ignore
pub trait Interpolatable<Timestamp>: Sized + Clone
where Timestamp: Copy + Debug + PartialOrd {
  fn interpolate(&self, other: &Self, t: f32, time_a: Timestamp, time_b: Timestamp) -> Self;
}
```

`t` is `0.0` at `self`, `1.0` at `other`. The timestamps are supplied for non-linear schemes.

### Trait `ToF32`

Converts a timestamp, or the difference between two, into `f32`. Implemented for `u64`, `i64`, `f32`, `f64`, and `Duration`.

`Interpolatable` and `ToF32` are shared with `plaza_server_utils`, so one state type feeds both a client's `SnapshotBuffer` and the server's `HistoricalStateBuffer` with a single impl.

### Struct `ServerSnapshot<Timestamp, StateType>`

One buffered entry: `server_timestamp`, `state`.

## 6. Extrapolation

### Struct `ExtrapolationBase<StateType, VelocityType, ServerTimestamp>`

*   **`new(state, velocity, server_timestamp, client_receipt_time_ms: ClientTimeMs) -> Self`**: `client_receipt_time_ms` is what the extrapolation duration is measured from.
*   **Public fields**: `state`, `velocity`, `server_timestamp`, `client_receipt_time_ms`.
*   **`over_extrapolations() -> u64`**: projections that ran past the cap and were held at it. Behind a `Cell`, so counting does not require `&mut self`.
*   **`get_extrapolated_state<TimeDelta>(&self, target_client_render_time_ms, max_extrapolation_duration_ms: u64, convert_ms_to_time_delta: impl Fn(u64) -> TimeDelta) -> Option<StateType>`**: the **duration** is capped, so an entity coasts to the limit and stops there. A target before `client_receipt_time_ms` returns the un-extrapolated base state. `convert_ms_to_time_delta` turns the capped millisecond duration into whatever `TimeDelta` your impl takes (`|ms| ms as f32 / 1000.0` for seconds, `Duration::from_millis` for a `Duration`).

The cap logs at `warn`. A *steady* occurrence means a render target sitting permanently ahead of the newest snapshot, so the view never interpolates at all.

### Trait `Extrapolatable<VelocityType, TimeDelta>`

```rust,ignore
pub trait Extrapolatable<VelocityType, TimeDelta>
where Self: Sized + Clone, VelocityType: Debug, TimeDelta: Copy + Debug {
  fn extrapolate_with_velocity(&self, velocity: &VelocityType, delta_time: TimeDelta) -> Self;
}
```

## 7. Smoothing

### Struct `ErrorSmoother<State>`

Smooths only what you *draw*, never the logical predicted state, which must stay exact for the next prediction. The blend is a closure you pass, so no trait bound is imposed on `State`.

*   **`new(duration_secs: f32) -> Self`**: `0.0` disables smoothing.
*   **`with_easing(self, easing: fn(f32) -> f32) -> Self`**: default `linear`. The blend *across states* stays your `sample` `lerp`; the easing only remaps how far along the ease is this frame. `smoothstep`, `ease_out_cubic`, `ease_in_out_quad`, `ease_in_cubic` and `ease_in_quad` ship as conveniences. Cubic covers 12.5% of the distance in the first half of the time, quadratic 25%.
*   **`at_rate(retain_per_frame)`**: sheds a fixed fraction of the remaining gap per 60Hz frame instead of finishing within a duration.
*   **`begin_from(&mut self, rendered_before_correction: State)`**: start easing from where the entity was last drawn.
*   **`advance(&mut self, dt_secs: f32)`**, **`is_easing(&self) -> bool`**.
*   **`sample(&self, logical: &State, lerp: impl Fn(&State, &State, f32) -> State) -> State`**: while easing, blends from the captured position toward the live `logical`; otherwise returns `logical` unchanged.
*   **`reset(&mut self)`**: abandon the ease in progress and draw the logical state directly.

The snap-versus-ease threshold is deliberately not a parameter: check the correction distance yourself and skip `begin_from` for large jumps.

## 8. Module `ack`

Pure sequence arithmetic, no allocation, no socket.

### Struct `AckWindow`

The newest sequence number, plus a bitmask of the `WINDOW` (64) before it. Bit `i` stands for `newest - 1 - i`.

*   **`new()`**, **`reset()`**.
*   **`observe(seq: u64) -> bool`**: returns whether it was new. A straggler arriving after a newer packet lands in its own slot rather than being taken for the new newest.
*   **`encode() -> Option<(u64, u64)>`** / **`from_encoded(newest, mask)`**: the wire form, twelve bytes.
*   **`contains(seq) -> bool`**, **`newest() -> Option<u64>`**, **`mask() -> u64`**, **`received_in_window() -> u32`**.
*   **`missing_since(oldest) -> impl Iterator<Item = u64>`**: the gaps, ascending, clamped to the window. What a sender resends.
*   **`contiguous_base(first) -> Option<u64>`**: the newest sequence such that everything from `first` up to it arrived. `None` if `first` itself is missing. **The argument is the first sequence to *check***, not the newest already known to have arrived: passing `0` at the start of a protocol numbering from zero would otherwise read as "zero already arrived".

## 9. Module `trajectory`

### Struct `TrajectoryPredictor`

Second-order dead reckoning for one scalar: keeps the last three samples, takes velocity from the newest pair and acceleration from the change between pairs, and projects a damped quadratic. Run one per axis.

*   **`new(damping: f32, max_horizon_ms: u64)`**: `damping` scales the acceleration term, `0.0` being plain constant velocity and `1.0` the full quadratic. `max_horizon_ms` clamps how far past the newest sample a prediction may reach; beyond it the projection is evaluated *at* the horizon and held. There is no unbounded setting.
*   **`observe(time_ms, value)`**: samples at or before the newest are ignored.
*   **`predict(time_ms) -> Option<f32>`**: `None` before any sample, holds with one, first order with two, the damped curve with three. Interpolates as readily as it extrapolates.
*   **`velocity()`**, **`acceleration()`** (undamped), **`newest_time()`**, **`samples()`**, **`reset()`**.

## 10. Module `timestep`

### Struct `FixedTimestep`

*   **`from_step_ms(step_ms)`** / **`from_hz(hz)`**: **panics on zero.** `from_hz` is integer division, so a rate that does not divide 1000 truncates: 60 Hz is a 16 ms step running 62.5 times a second. That does **not** match `plaza::TickDriver::from_hz`, which is exact at 16.667 ms, so a client predicting through this against such a server runs 4.2% fast. Pick a rate that divides 1000 when both sides matter, and take the delta from `step_secs()` rather than from the rate.
*   **`with_max_frame_ms(ms)`**: cap how much elapsed time one `advance` may pay for. Default `DEFAULT_MAX_FRAME_MS`, 250 ms.
*   **`advance(&mut self, elapsed_ms) -> Steps`**: an `ExactSizeIterator` yielding the step duration in milliseconds, once per step this frame paid for. The duration is *yielded* so a caller cannot integrate by the frame delta instead.
*   **`step_ms()`**, **`step_secs()`**, **`set_step_ms(ms)`**, **`pending_ms()`**, **`alpha()`** (the fraction of a step accumulated), **`dropped_ms()`**, **`reset()`**.

`dropped_ms` is real time the simulation never ran because the cap refused it. If you also keep a clock, advance it by *simulated* time, not wall time.

### Struct `Periodic`

The same accumulator, consumed as "is it time yet" rather than "how many steps". Subtracts the interval rather than zeroing the accumulator.

*   **`new(interval_ms)`** / **`from_hz(hz)`**, **`set_interval_ms`**, **`interval_ms()`**, **`remaining_ms()`**, **`reset()`**.
*   **`due(&mut self, elapsed_ms) -> bool`**: fires at most once per advance.
*   **`advance(&mut self, elapsed_ms) -> u32`**: every occurrence.

## 11. Module `mirror`: the client half of a delta stream

The counterpart to `plaza_server_utils::DeltaBaseline`, keyed by the same [`SlotKey`](#13-modules-slot-and-digest) and checked by the same `SetDigest`.

### Struct `DeltaMirror<Entity>`

Generic over the entity. The mirror owns only the keying, the agreement and the counters.

*   **`new()`**, **`with_generations(bool)`** / **`set_generations(bool)`**: whether a key names an occupant or only a slot.
*   **`begin(&mut self, seq, full_baseline)`**: start applying a packet. `full_baseline` clears first.
*   **`insert(key, entity)`**, **`remove(key) -> Option<Entity>`**, **`get`**, **`get_mut`**, **`contains`**.
*   **`settle(&mut self, expected: u64) -> Agreement`**: fold the digest and compare against the server's. `Agreement::agreed()` for the boolean.
*   **`divergence_from(server_keys) -> Divergence`**: `missing` means something was lost or never sent, `extra` means a removal never landed. A digest detects and cannot diagnose.
*   **`digest()`**, **`acks() -> &AckWindow`**, **`applied_seq()`**, **`keys`**, **`iter`**, **`iter_mut`**, **`values`**, **`values_mut`**, **`len`**, **`is_empty`**, **`clear`**.
*   **`frames_lost()`**, **`stale_refs()`**, **`divergences()`**.

**Apply every packet, whatever baseline it names.** These deltas carry absolute values, so applying them is idempotent and applying a superset is harmless.

The three counters are separate: sequence gaps mean the wire lost something, stale references mean a message named an occupant this mirror no longer holds, and digest divergences are the symptom no counter predicts.

## 12. Module `correction`

### Struct `Correction<State>`

The state as it was `seen` before the correction, and the `settled` state after. Two states rather than a distance, so no metric is imposed on `State`.

### Struct `CorrectionMonitor`

A running picture of prediction error, and an adaptive test for what counts as abnormal. There is no fixed threshold: it tracks the mean and variance of the corrections it is fed.

*   **`new()`**, **`with_warmup(samples)`**, **`with_smoothing(alpha)`**, **`with_sigma(sigma)`**, **`with_floor(floor)`**.
*   **`record(magnitude) -> bool`**: fold in a sample, returning whether it is abnormal.
*   **`is_abnormal(magnitude)`** (without recording), **`threshold()`**, **`band()`**, **`norm()`**, **`peak()`**, **`counts() -> (u64, u64)`**, **`is_warming_up()`**, **`reset()`**.

## 13. Modules `slot` and `digest`

The vocabulary both sides of a streamed entity set agree about. They live here and `plaza_server_utils` re-exports them, so a browser client does not inherit a server to get them.

### Struct `SlotKey`

A storage slot and the generation of its current occupant. **`new(index: u32, generation: u16)`**, **`encode() -> u64`** (`(index << 16) | generation`), **`decode(u64)`**, **`ungenerational()`**, **`same_occupant(other)`**, plus `From` both ways.

### Struct `SlotAllocator`

Hands out `SlotKey`s over a dense index space, recycling freed slots and bumping a generation. **`alloc`**, **`free`**, **`is_live`**.

*   **`with_capacity(slots)`**, **`with_policy(ReusePolicy)`**.
*   **`index_space() -> usize`**: how many indices exist, live or free. The width to size a `Vec<T>` or a `VisibilitySet` by; **`len()`** is the live count and is smaller.
*   **`is_occupied(index) -> bool`**: membership without building a key, and without the borrow `iter` holds.

It does not store your entities: keep them in a `Vec<T>` indexed by `SlotKey::index`.

**The generation bumps on free, not on allocate**, so a handle stops naming anything the moment its subject dies.

**`ReusePolicy::{Lifo, Fifo}`** decides how clustered recycled indices are, and is public contract for that reason.

**The ceiling.** A `u16` generation wraps after 65,536 reuses of one slot and nothing can detect the wrap. `Fifo` spreads reuse across the index space instead of hammering the same slots.

### Struct `SetDigest`

An order-independent digest of a set of `u64` keys, maintainable incrementally. **`new`**, **`from_keys`**, **`insert`**, **`remove`**, **`clear`**, **`len`**, **`is_empty`**, **`digest() -> u64`**.

The combine is addition, so a key can be added or removed in O(1) and duplicates do not cancel the way XOR would. The key is a `u64` you choose: hash a bare index to check *membership*, or pack index with generation to check the *occupant*.

`VisibilitySet::digest()` computes the same value over a bitset's membership.

## 14. Module `coalesce`

### Struct `InputCoalescer<Input>`

Send an input on change, plus a keepalive. **`new(keepalive_ms)`**, **`should_send(&input, now_ms) -> bool`**, **`set_enabled`**, **`is_enabled`**, **`last_sent`**, **`reset`**.

Pairs with [`HeldInputPredictor`](#struct-heldinputpredictorstate-input-ctx), never with `PredictedPlayer`.

## 15. Module `playout`: the playout queue and the resume verdict

### Enum `Admission`

What `push` concluded. **`Queued`** or **`TimelineLost`**. `#[must_use]`: `TimelineLost` obliges the caller to restart its own timeline, re-anchor its render clock on what just arrived, and drop derived state.

### Struct `PlayoutBuffer<T>`

*   **`new(max_queued: usize, lost_ahead: u64) -> Self`**: `max_queued` bounds the queue absolutely. `lost_ahead` is the discontinuity threshold, how far past the render instant an arrival may reach before the client is lost rather than buffering; match it with the server's stalled-subscriber threshold.
*   **`push(&mut self, stamp: u64, order: u64, item: T, render_at: Option<u64>) -> Admission`**: `stamp` is the instant the packet describes, `order` its sequence number (play-out is ordered by sequence), `render_at` the instant currently being drawn, `None` before the timeline has started.
*   **`pop_due(&mut self, render_at: u64) -> Option<T>`**: the oldest packet whose instant the clock has reached, in sequence order. Call in a loop until `None`.
*   **`timeline_lost(&mut self)`**: drops everything but the newest packet.
*   **`iter(&self) -> impl Iterator<Item = &T>`**: the packets held but not yet due, in sequence order.
*   **`underruns() -> u64`**: packets that arrived after their instant had been drawn, by a jitter-scale margin only. **`restarts() -> u64`**: discontinuities survived, one per stall however large the backlog. **`len()`**, **`is_empty()`**.

## 16. Module `arrival`: measuring how a stream actually arrives

### Struct `ArrivalMonitor`

Keep one per interpolated stream.

*   **`new(smoothing: f32) -> Self`**: the EWMA weight for new observations, `0..=1`; around `0.05` follows a link's drift without chasing individual packets.
*   **`observe(&mut self, stamp: u64, recv: u64)`**: `stamp` is the declared server time the packet describes; `recv` is the client's synced estimate of server time at arrival. Call for every packet, reordered or not: a stamp older than the newest seen updates lateness but never the interval, which is measured forward only.
*   **`interval_ms() -> f32`**: the smoothed gap between declared stamps.
*   **`lateness_ms() -> f32`**: the smoothed mean lateness.
*   **`jitter_ms() -> f32`**: the smoothed mean deviation of lateness.
*   **`needed_delay_ms() -> f32`**: lateness plus jitter plus one interval.
*   **`warmed_up() -> bool`**: whether two forward stamps have been seen, so an interval exists.

## 17. Module `math`

Small vector and quaternion types, so this crate is usable without a math library. They implement `Interpolatable` and `Extrapolatable`.

*   **`Vec2`**: `new`, `ZERO`, `ONE`, `length`, `length_squared`, `dot`, `normalize`, plus `Add`/`Sub`/`Mul`.
*   **`Vec3`**: the same surface in three dimensions.
*   **`Quat`**: `new`, `IDENTITY`, `normalize`, `dot`, `multiply` (Hamilton product), `slerp`.

## 18. Module `net_sim` (feature `net-sim`)

A deterministic latency / jitter / loss queue. Opt-in.

### Struct `LatencyLink<T>`

*   **`new()`**, **`with_ordering(Ordering)`**, **`send(now_ms, packet, latency_ms, jitter_ms, loss_pct, &mut Rng)`**, **`drain_due(now_ms) -> Vec<T>`**, **`in_flight()`**.
*   **`Ordering::Ordered`** (the default) clamps each delivery time to at least the previous one, so jitter delays a packet past its predecessor but never ahead of it. **`Ordering::Unordered`** is the datagram case.

`LatencyLink` is `Clone`, so it can sit inside a plaza state, which must be `Clone`.

### Struct `Rng`

A seeded, reproducible generator: **`new(seed)`**, **`unit() -> f32`**, **`up_to(n) -> u64`**. Deliberately not a "deterministic shared stream" block: identical seeds fed divergent inputs still diverge.

## 19. Module `fixed` (feature `fixed`)

Fixed-point arithmetic, for a wire that carries causes instead of state.

### Struct `Fx` and struct `P`

**`Fx(pub i32)`**: signed 32-bit fixed point, 24 integer bits and 8 fractional (`FRAC_BITS`, `ONE`). Serialized as the raw `i32` (`serde(transparent)`).

*   **`ZERO`**, **`ONE`**, **`from_int(n)`**, **`ratio(num, den)`**, **`to_int()`**.
*   **`to_f32()`**: the only float in the vocabulary, one way, for the renderer. Nothing in a simulation may call it.
*   **`mul`**, **`div`**: carried in `i64` so the intermediate cannot overflow, and truncating, because rounding has a tie case two implementations can disagree about.
*   **`abs`**, **`min`**, **`max`**, and `Add`/`Sub`/`Neg`/`AddAssign`, all wrapping.
*   **`sqrt()`**: the largest `r` with `r*r <= n`, a property of the input alone.

**`P { x: Fx, y: Fx }`**: **`new`**, **`from_ints`**, **`dist_sq`**, **`dist`**.

## 20. Error Handling

### Enum `ClientUtilError`

`Debug`, implements `std::error::Error` via `thiserror`. Variants: `InputBufferFull`, `InputNotFoundInBuffer`, `ReconciliationInconsistency`, `InterpolationError`, `ExtrapolationError`, `InvalidArgument`.

Most operations return values or `Option` rather than `Result`. A full buffer discards its oldest entry and logs.
