# API Reference: `plaza_client_utils`

## 1. Introduction & Core Concepts

`plaza_client_utils` is the **client half** of real-time networking: making a server's authoritative updates feel immediate and smooth. The server half (input sequence tracking, delayed input buffers, lag-compensation rewind), lives in `plaza::game_common::reconciliation`.

**No workspace dependencies.** This crate depends on `thiserror` and `tracing` and nothing else, deliberately, so wasm builds and game-engine plugins do not drag in a server's async runtime. It is pure logic with no transport, no serialization, and no engine coupling; you feed it what you receive and read back what to render.

It addresses these problems, usable independently:

| Problem | Piece |
|---|---|
| Local input should feel instant, but the server decides | [`PredictedEntity`](#struct-predictedentity) + [`ClientInputBuffer`](#struct-clientinputbuffer) |
| Other players' updates arrive discretely and jittery | [`SnapshotBuffer`](#struct-snapshotbuffer) |
| Updates stop arriving for a moment | [`ExtrapolationBase`](#struct-extrapolationbase) |
| Peers run one deterministic sim and cannot wait for each other's input | [`RollbackSession`](#3b-rollback-netcode-deterministic-lockstep) |

The first three serve the *server-authoritative* model (an authority decides, a client predicts its own entity and is reconciled). The last serves the other family, *peer-to-peer deterministic lockstep* (rollback), covered in [section 3b](#3b-rollback-netcode-deterministic-lockstep).

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

The recommended starting point: two types that bundle the primitives into the whole client-side job. The primitives (sections 4 onward) remain public for finer control.

### Struct `PredictedPlayer<State, Input>`

Your controlled entity: `PredictedEntity` + `ClientInputBuffer` + `ErrorSmoother` + a sequence counter, wired. `apply` and `lerp` are plain `fn` pointers (the game rule and the smoothing blend), so no closure bounds are imposed.

*   **`new(initial, PlayerConfig, apply: fn(&mut State, &Input), lerp: fn(&State, &State, f32) -> State)`**
*   **`input(&mut self, input) -> SequenceNumber`**: predict locally, buffer for replay, return the sequence to send.
*   **`reconcile(&mut self, authoritative, acked_seq)`**: snap the logical state to authority, replay unacknowledged inputs, begin easing the visible correction.
*   **`advance(&mut self, dt_secs)`**, **`render() -> State`** (eased), **`logical() -> &State`** (exact), **`authoritative() -> &State`**, **`latest_seq()`**, **`acked_seq()`**, **`unacked_count()`**.

**`PlayerConfig`**: `input_buffer: usize` (retain the most inputs that can be in flight), `smoothing_secs: f32` (`0.0` disables smoothing), `easing: fn(f32) -> f32` (the correction's time curve, default `linear`). `Default` is 256 / 0.1s / linear.

Snap-versus-ease on a large desync is the caller's call (check `render()` against the incoming authoritative before `reconcile`, or set `smoothing_secs = 0`), because "how far is a desync" is application geometry.

### Struct `RemoteView<State, Velocity>`

An entity you do not control: a `SnapshotBuffer` plus the interpolate / extrapolate / hold decision, with the buffer-starvation detection handled for you. Time is `u64` milliseconds or ticks; for a `Duration` timeline, compose `SnapshotBuffer` and `ExtrapolationBase` directly. Requires `State: Interpolatable<u64> + Extrapolatable<Velocity, f32>`.

*   **`new(buffer_size, max_extrapolation_ms)`**: `buffer_size` >= 2; dead-reckon at most `max_extrapolation_ms` past the newest before holding.
*   **`push(&mut self, time_ms, state, velocity)`**: record a snapshot and the velocity to dead-reckon along.
*   **`render(&self, target: Option<u64>, RenderOpts) -> Option<State>`**: `None` until the first push; otherwise the state to draw. Interpolated at `target`, dead-reckoned when the buffer has starved (if `opts.extrapolate`), or the raw newest (if `opts.interpolate` is false).
*   **`latest() -> Option<&State>`**.

**`RenderOpts`**: `interpolate: bool`, `extrapolate: bool`. `Default` is both on; a real client fixes them, the booleans exist so a UI can toggle them.

## 3b. Rollback netcode (deterministic lockstep)

A different model from the rest of this crate. There is no server: peers run the **same deterministic simulation**, exchange only inputs, and stay identical frame for frame. Latency is handled by **predicting** a missing remote input (repeat its last one), simulating ahead, and **rolling back** to re-simulate when the real input arrives and disagrees. Determinism is what makes the re-simulation land on the state the other peer already has. Lives in module `rollback`.

Three pieces, smallest first; the primitives stay public for hand-wiring, `RollbackSession` is the ready-made loop (the rollback counterpart to [`PredictedPlayer`](#struct-predictedplayerstate-input)).

**`Frame = u64`**: a logical simulation frame. Rollback counts in fixed frames, never wall-clock time.

### Struct `StateHistory<State: Clone>`

A frame-indexed ring of whole-world snapshots: the save-states rollback restores. Pure save/restore by frame, no interpolation (unlike `SnapshotBuffer`, which blends between times).

*   **`new(capacity)`**: keeps the most recent `capacity` frames, the maximum rollback distance. **Panics if 0.**
*   **`save(&mut self, frame, state)`**: record a frame. Contiguous use (append `latest + 1`, or overwrite a frame in the window, as re-simulation does); a save that skips ahead resets the window rather than leaving a gap.
*   **`restore(&self, frame) -> Option<State>`**: the saved state, or `None` if evicted or never saved.
*   **`oldest_frame()`**, **`latest_frame()`**, **`len`**, **`is_empty`**, **`clear`**.

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
*   **`apply_local_input_and_predict(&mut self, op: &Op, sequence_number: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_fn: impl Fn(&mut StateType, &Op))`** Applies an input immediately and records it as unacknowledged.
*   **`reconcile_with_server_state(&mut self, authoritative_state: StateType, last_processed_sequence: SequenceNumber, input_buffer: &mut ClientInputBuffer<Op, StateType>, apply_fn: impl Fn(&mut StateType, &Op))`** Snaps to the server's state, drops acknowledged inputs, and replays the rest.
*   **Fields**: the current predicted state and the last acknowledged sequence number are readable for rendering and diagnostics.

`apply_fn` is the shared simulation step, the same rule the server applies. Both sides must agree, or prediction fights the server every frame.

### Struct `ClientInputBuffer<Op, PredictedStateSnapshot>`

Unacknowledged inputs, kept for replay.

*   **`new(max_size: usize) -> Self`**
*   **`record_input(&mut self, sequence_number, op, state_before)`**: stores an input and the state it was applied to. At capacity the oldest is discarded.
*   **`acknowledge_inputs_up_to(&mut self, ack_sequence_number)`**: drops everything the server has processed.
*   **`get_unacknowledged_inputs(&self) -> impl Iterator`**: what to replay, in order.
*   **`get_predicted_state_before_input(&self, sequence_number) -> Option<&PredictedStateSnapshot>`** Useful for diagnosing how far a prediction diverged.
*   **`len`**, **`is_empty`**, **`clear`**

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

### Struct `RttEstimator`

Smooths round-trip samples (from a `plaza_wire` `Ping`/`Pong` exchange) into a stable latency estimate. Both a client measuring the server and a server measuring a client use it.

*   **`new(alpha)`** / **`Default`** (alpha 0.1): `alpha` is each sample's moving-average weight.
*   **`observe(&mut self, rtt_sample_ms: u64)`**, **`observe_pong(&mut self, origin_time_ms, now_ms)`**: record a sample; the second computes `now - origin`.
*   **`rtt_ms()`**, **`one_way_ms()`** (half the RTT), **`min_rtt_ms()`** (the smallest seen, the best latency estimate since jitter only adds delay), **`jitter_ms()`** (the smoothed mean deviation, size a dynamic interpolation buffer from it). Each `None` before the first sample.

`RttEstimator` is the zero-config default. The next two are heavier building blocks for when it is not enough, offered as options, not replacements.

### Struct `ClockSyncEstimator`

Fits the client-to-server clock **offset and its skew** (drift rate) by least squares over a sliding window, where a moving average models offset alone. `f64` times (millisecond timestamps over a long session exceed `f32`'s integer precision).

*   **`new(window: usize)`**: fit over the last `window` measurements (16 to 64 typical). **Panics if less than 2.**
*   **`observe(local_ms, offset_ms)`**: record a measured `offset = server - local` at local time `local_ms`.
*   **`observe_exchange(local_send, server_recv, local_recv)`**: derive the offset from a round trip under the symmetric-delay assumption.
*   **`offset_at(local_ms) -> Option<f64>`** (fitted, so it interpolates and extrapolates along the line), **`server_time_at(local_ms) -> Option<f64>`**, **`skew() -> f64`** (drift per unit time; `x 1e6` for ppm), **`is_ready()`**, **`sample_count()`**, **`clear()`**.

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

*   **`new(state, velocity, timestamp) -> Self`**: the last authoritative state.
*   **`get_extrapolated_state(&self, target_time, max_extrapolation) -> StateType`** Projects forward, clamped by `max_extrapolation` so a long gap does not send an entity off into the distance.

### Trait `Extrapolatable<VelocityType, TimeDelta>`

```rust,ignore
pub trait Extrapolatable<VelocityType, TimeDelta> {
  fn extrapolate(&self, velocity: &VelocityType, delta_time: TimeDelta) -> Self;
}
```

Prefer interpolation when snapshots are available; extrapolation is a stopgap, and every extrapolated frame is a guess to be corrected.

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

The snap-versus-ease threshold is deliberately not a parameter: check the correction distance yourself and skip `begin_from` for large jumps.

## 7b. Module `ack`

Sliding-window acknowledgement: pure sequence arithmetic, no allocation, no socket.

### Struct `AckWindow`

A record of which recent sequence numbers arrived: the newest, plus a bitmask of the `WINDOW` (64) before it. Bit `i` of the mask stands for `newest - 1 - i`.

*   **`new()`**, **`reset()`**.
*   **`observe(seq: u64) -> bool`**: records an arrival, returning whether it was new. Handles reordering: a straggler arriving after a newer packet lands in its own slot rather than being taken for the new newest.
*   **`encode() -> Option<(u64, u64)>`** / **`from_encoded(newest, mask)`**: the wire form, twelve bytes, and the rebuild on the far side.
*   **`contains(seq) -> bool`**, **`newest() -> Option<u64>`**, **`mask() -> u64`**, **`received_in_window() -> u32`**.
*   **`missing_since(oldest) -> impl Iterator<Item = u64>`**: the gaps, ascending, clamped to the window. What a sender resends. Past the window the data is beyond recovery and the caller should be resynchronising rather than backfilling, so the ask stays bounded no matter how far back it points.

Fixed size is the whole point: a link losing half its packets reports in the same twelve bytes as a perfect one, and heavy loss is precisely when there is no room for an explicit list.

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

## 8. Module `math`

Small vector and quaternion types, so this crate is usable without pulling in a math library. Implement `Interpolatable` and `Extrapolatable` for your own types instead if you already have `glam` or `nalgebra`.

*   **`Vec2`**: `new`, `ZERO`, `ONE`, `length`, `length_squared`, `dot`, `normalize`, plus `Add`/`Sub`/`Mul` operators.
*   **`Vec3`**: the same surface in three dimensions.
*   **`Quat`**: `new`, `IDENTITY`, `normalize`, `dot`, `multiply` (Hamilton product, composing rotations), `slerp` (shortest-path spherical interpolation).

These implement `Interpolatable` and `Extrapolatable`, so they work with the buffers above out of the box.

> This overlaps `plaza::common::math` intentionally. Keeping this crate free of
> workspace dependencies matters more than sharing sixty lines of plain data.

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
