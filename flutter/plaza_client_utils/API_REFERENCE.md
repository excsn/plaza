# API Reference: `plaza_client_utils` (Dart)

## 1. Introduction & Core Concepts

The whole of the Rust `plaza_client_utils` crate, ported. Nothing here knows about a transport, a codec or a session; these are the pieces a real-time client assembles between the socket and the screen.

The Rust crate stays authoritative. Where Dart forced a decision the Rust source did not have to make, the entry below says so.

```dart
import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:plaza_client_utils/net_sim.dart';   // separate, see section 12
```

For the guidance that decides *which* of these to use ([the four principles](README.md#four-principles-worth-knowing-before-you-predict-or-render-anything), [which predictor](README.md#which-predictor), [drawing an entity you do not control](README.md#drawing-an-entity-you-do-not-control), [the resume contract](README.md#the-resume-contract)), see the [README](README.md). This file is the surface.

### Two rules that apply throughout

**Rules are functions, not traits.** Where the Rust crate constrains a state type with `Interpolatable` or `Extrapolatable`, the Dart port takes a `lerp` or `extrapolateBy` function. Same information, no trait system to lean on.

**`Frame` is not exported from the barrel.** The Rust crate re-exports `rollback::Frame`, an `int` alias for a frame index. This does not, because [`plaza_wire`](../plaza_wire/API_REFERENCE.md#class-frame) exports a `Frame` class and an app importing both could name neither. The alias is in `src/rollback.dart` for direct import.

## 2. Error Handling

### Sealed class `ClientUtilError`

```dart
sealed class ClientUtilError implements Exception { const ClientUtilError(); }
```

Ported from Rust's `ClientUtilError`. Dart has no `thiserror`, so each variant is a class and the message lives in `toString`.

| Class | Fields |
|---|---|
| `InputBufferFull` | `maxSize`, `sequenceNumberTried` |
| `InputNotFoundInBuffer` | `sequenceNumber` |
| `ReconciliationInconsistency` | `serverAckSequence`, `clientLastKnownSequence` |
| `InvalidArgument` | `details` |

**Nothing in this package currently throws one.** The type is ported so an application modelling the same conditions has the vocabulary, and so the Dart and Rust surfaces stay comparable.

What does throw is `ArgumentError`, from constructors given a value that cannot work: [`ClockSyncEstimator`](#class-clocksyncestimator) below 2, [`SnapshotBuffer`](#class-snapshotbuffer) below 2, [`ClientInputBuffer`](#class-clientinputbuffer) at zero, [`FixedTimestep`](#class-fixedtimestep) and [`Periodic`](#class-periodic) at zero, [`TickNamer`](#class-ticknamer) at a non-positive step, and [`StateHistory`](#class-statehistory) and [`InputTimeline`](#class-inputtimeline) at a non-positive capacity.

## 3. Core types

```dart
typedef SequenceNumber = int;   // monotonic per stream
typedef ClientTimeMs = int;     // ms since a client-defined epoch
```

`SequenceNumber` is what orders inputs during replay and what identifies which inputs the server has acknowledged. `ClientTimeMs`'s exact epoch and resolution are the application's.

## 4. Drop-in entities

The three ready-made bundles. Each wires the lower-level pieces into the loop for one entity.

### Class `PredictedPlayer`

```dart
class PredictedPlayer<S, I, C> {
  PredictedPlayer({
    required S initial,
    required S Function(S state, I input, C ctx) apply,
    required S Function(S a, S b, double t) lerp,
    required C context,
    PlayerConfig config = const PlayerConfig(),
  });
}
```

The local player's entity: predicts on input, reconciles against the server, and eases the correction.

**For a server that consumes one input per simulation step.** For one that holds an input and integrates it every tick, use [`HeldInputPredictor`](#class-heldinputpredictor).

`apply` takes the world as `C` so a *forced* entity, one the server moves by more than its own input, can run the same rule the server runs.

| Member | Notes |
|---|---|
| `int input(I input)` | Applies locally, records for replay, returns the sequence number to send. While frozen the input is still numbered, so sequences stay in step, but nothing is predicted. |
| `Correction<S> reconcile(S authoritative, int ackedSeq)` | Snaps, replays what the server had not seen, begins the ease. Returns what was drawn before and what was settled on, so you can measure your own prediction error. |
| `void advance(double dtSecs)` | Progresses the correction ease by one frame. |
| `S render()` | Where to draw: the prediction, eased through recent corrections. |
| `S get logical` | The exact predicted state, for game logic. **Never smoothed.** |
| `S get authoritative` | The last state the server confirmed, for a ghost overlay or an error readout. |
| `void teleport(S state)` | Moves without easing and drops pending inputs. A teleport is not a disagreement, and easing one draws the entity across the level through everything in between. |
| `C context` (get/set) | The world the prediction runs against. |
| `bool active` (get/set) | False freezes prediction, for an entity the server is holding still. |
| `int get latestSeq`, `int get ackedSeq` | |
| `int get unackedCount` | How many sent inputs still await acknowledgement, which is what a reconciliation replays. Growing without bound means acknowledgements are not arriving. |
| `bool get isEasing` | |

Setting `context` holds one world rather than a snapshot per buffered input, so a replay uses the newest world. That is a different approximation, not a strictly better one: the inputs being replayed happened under a world that has since moved. An application needing the exact history carries a snapshot in its own input type instead.

### Class `PlayerConfig`

```dart
class PlayerConfig {
  const PlayerConfig({
    int inputBuffer = 256,
    double smoothingSecs = 0.1,
    Easing easing = linear,
  });
}
```

`inputBuffer` should cover the most inputs that can be in flight at once: input rate times worst round trip. `smoothingSecs` of zero snaps.

### Class `HeldInputPredictor`

```dart
class HeldInputPredictor<S, I, C> {
  HeldInputPredictor({
    required S initial,
    required I initialInput,
    required S Function(S state, I input, double dtSecs, C ctx) integrate,
    required S Function(S a, S b, double t) lerp,
    required C context,
    HeldInputConfig config = const HeldInputConfig(),
    double Function(S a, S b)? distance,
    double? teleportBeyond,
  });
}
```

A locally dead-reckoned entity whose server holds its input and integrates it every tick.

There is **no separate logical and render state** here, unlike [`PredictedPlayer`](#class-predictedplayer): the correction is applied continuously to the state itself, so there is no exact value being smoothed away.

`integrate` is the rule the *server* runs, shared rather than re-derived. Anything the server does that this leaves out arrives as a permanent correction.

| Member | Notes |
|---|---|
| `void hold(I input)` | Sets the input the server is holding. Call whenever intent changes, independently of when it is transmitted: what is sent is a bandwidth decision, what is integrated is a simulation one. |
| `I get held` | |
| `void advance(double dtSecs)` | Dead reckons one step. Does nothing while frozen. |
| `S project(S authoritative, double ageSecs)` | Where the server's state has probably got to by now. An authoritative packet describes the past by one one-way delay, so correcting straight to it would pull the entity backward by whatever it travelled meanwhile. Public so you can measure the disagreement yourself. |
| `Correction<S> reconcile(S authoritative, double ageSecs)` | Bends the prediction toward `project(authoritative, ageSecs)` by the blend, and reports the move. |
| `void teleport(S state)` | A spawn, respawn or teleport. Not a correction, so nothing is eased. |
| `S get logical`, `S render()` | Identical: there is no separate exact value to preserve. |
| `bool active` (get/set) | False while the server is holding the entity still: dead, stunned, mid respawn. Then `reconcile` tracks the server exactly rather than inventing a correction every packet. |
| `C context` (get/set) | |

`distance` and `teleportBeyond` are opt-in discontinuity detection, taken as arguments rather than required on the state type so applications that do not want it are not made to define a metric. Set `teleportBeyond` well above any correction ordinary play produces.

### Class `HeldInputConfig`

```dart
class HeldInputConfig { const HeldInputConfig({double blend = 0.25}); }
```

`blend` is the fraction of the remaining gap to the server closed on each reconcile. Higher converges faster and follows the server more tightly; lower stays smoother and leads more on local input. **Zero disables correction entirely**, which is pure dead reckoning and drifts without bound.

### Class `RemoteView`

```dart
class RemoteView<S, V> {
  RemoteView({
    required int bufferSize,
    required int maxExtrapolationMs,
    required S Function(S a, S b, double t) lerp,
    required S Function(S state, V velocity, double dtSecs) extrapolateBy,
  });
}
```

One remote entity's samples, and the decision about what to draw from them.

Holds the interpolate, extrapolate or hold choice internally and returns the right state, rather than handing you a starvation callback to invert control over.

| Member | Notes |
|---|---|
| `void push(int timeMs, S state, V velocity)` | Records a sample. The velocity is kept beside it for extrapolation. |
| `S? render(int? target, [RenderOpts opts])` | What to draw, or null before the first sample. A null `target` gives the newest sample. |
| `S? get latest`, `V? get latestVelocity`, `int? get latestTimestamp` | |
| `int? get oldestTimestamp`, `int get length`, `bool get isEmpty` | |
| `void clear()` | |

#### Property `overExtrapolations`

`int`. How many renders asked for a time further past the newest sample than `maxExtrapolationMs`, and were served the capped coast instead.

Not in the Rust original, which logs a warning. Holding at the cap is a legitimate outcome, so this is not an error, but reaching it **steadily** means this entity's packets have stopped arriving and the view is drawing a guess that has stopped improving.

#### Class `RenderOpts`

```dart
class RenderOpts { const RenderOpts({bool interpolate = true, bool extrapolate = false}); }
```

`interpolate: false` draws the raw newest snapshot, which jumps at the server rate. `extrapolate: true` dead reckons along the last velocity when the buffer has nothing ahead of the target, instead of holding the newest.

**Extrapolation caps the duration, not the result.** The entity coasts to the limit and stops there. Returning the raw newest sample past the limit is the obvious reading of "clamp" and it is a discontinuity: at the limit the entity has coasted `velocity * cap` forward, and one millisecond later it would be drawn back at the sample, a jump of the whole extrapolation window in the wrong direction, flickering under jitter around the boundary.

## 5. Prediction and reconciliation

The pieces [`PredictedPlayer`](#class-predictedplayer) wires together, for a loop you want to drive yourself.

### Class `PredictedEntity`

```dart
class PredictedEntity<S, Op> {
  PredictedEntity(S initialState);
  S predicted;              // what to draw
  S authoritative;          // the last state the server asserted
  int acknowledgedSeq;
}
```

| Member | Notes |
|---|---|
| `void applyLocal(Op op, int seq, ClientInputBuffer<Op, S> buffer, S Function(S, Op) apply)` | Applies an input locally and records it for replay. |
| `int reconcile(S newAuthoritative, int serverAckSeq, ClientInputBuffer<Op, S> buffer, S Function(S, Op) apply)` | Snaps and replays. Returns **how many inputs were replayed**, which is the number a diagnostic wants. |

### Class `ClientInputBuffer`

```dart
class ClientInputBuffer<Op, S> {
  ClientInputBuffer(int maxSize);
}
```

A history of inputs sent to the server, for prediction and reconciliation. Fixed capacity: the oldest is discarded when full, because an input older than the buffer can no longer be replayed and holding it unbounded turns a stalled server into a memory leak.

| Member | Notes |
|---|---|
| `void record(int seq, Op op, S stateBeforeOp)` | `stateBeforeOp` is the predicted state immediately before `op` was applied locally. |
| `void acknowledgeUpTo(int ackSeq)` | Drops everything up to and including `ackSeq`. |
| `Iterable<BufferedInput<Op, S>> unacknowledgedAfter(int ackSeq)` | In order. What reconciliation replays. |
| `S? stateBefore(int seq)` | The recorded pre-state, if still held. |
| `int get length`, `bool get isEmpty`, `void clear()` | |

#### Property `overflowed`

`int`. How many inputs were dropped because the buffer was full.

Not in the Rust original, which logs a warning instead. **A non-zero value means replay is already incomplete.**

### Class `BufferedInput`

```dart
class BufferedInput<Op, S> {
  const BufferedInput({required int sequenceNumber, required Op op, required S stateBeforeOp});
}
```

## 6. Interpolation

### Class `InterpolationClock`

```dart
class InterpolationClock { InterpolationClock(int delayMs); }
```

Where on the server timeline to render, kept a fixed delay behind the estimated server clock.

The estimate **free-runs** on `advance` rather than snapping on every packet, so the render target moves smoothly. Milliseconds throughout: the Rust original is generic over its timestamp type, and Dart has no numeric trait bounds worth the ceremony.

| Member | Notes |
|---|---|
| `void observe(int serverTimeMs)` | The first observation starts the clock; later ones are ignored. |
| `void advance(int dtMs)` | No effect before the first observe. |
| `int? get target` | The estimate minus the delay, floored at zero. Null before the first observation. This is the T that everything in a frame should be evaluated at. |
| `bool get started` | |
| `int delay` (get/set) | Settable for a client that sizes its buffer dynamically. |
| `void resync(int newestServerTimeMs, double strength)` | Steers the *position* toward the newest server time by `strength` in 0 to 1. Call in place of `observe` on each packet. |
| `void observeRate(int newestServerTimeMs, double maxRateAdjust)` | The rate-based cousin: adjusts the estimate's *speed* so it glides into alignment rather than jumping. Behind the newest, run slightly fast; ahead of it, which means interpolation is starving, run slightly slow. Pair with `advanceScaled`. |
| `void advanceScaled(int dtMs)` | Advances scaled by the playback rate. Identical to `advance` while the rate is 1. |
| `double get playbackRate` | 1 is real time. For a readout, or to spot a clock under sustained correction. |
| `void reset()` | Un-starts the clock, keeping the delay. Not in the Rust original, which rebuilds the value; Dart callers hold this behind a `final` field and a resume needs the estimate thrown away without the holder being rebuilt. |

### Class `SnapshotBuffer`

```dart
class SnapshotBuffer<S> {
  SnapshotBuffer({required int maxSize, required S Function(S a, S b, double t) lerp});
}
```

Holds recent snapshots and interpolates between the two that bracket a render target. Throws `ArgumentError` for a `maxSize` below 2.

| Member | Notes |
|---|---|
| `void add(int timestampMs, S state)` | Inserts **in timestamp order**, so a reordered packet still lands correctly. A duplicate timestamp replaces the earlier state. |
| `S? at(int targetMs)` | Between two snapshots it interpolates. Outside the buffer it **clamps to the nearest end rather than extrapolating**: extrapolation is a separate decision with its own failure mode, and silently doing it here would hide a starving stream. |
| `int get length`, `bool get isEmpty`, `void clear()` | |
| `int? get newestTimestamp`, `int? get oldestTimestamp` | |

### Class `ServerSnapshot`

```dart
class ServerSnapshot<S> { const ServerSnapshot(int timestampMs, S state); }
```

### Class `RenderTimeline`

```dart
class RenderTimeline {
  RenderTimeline({int delayMs = 100, double smoothing = 0.05});
  final InterpolationClock clock;
  final ArrivalMonitor arrival;
}
```

The render clock and the measurements that size it, joined to a game loop's `dt`.

A game loop has the one thing a client library does not: a `dt` every frame. This joins them, so the render target advances with the loop rather than with packet arrivals. Seconds, because that is what every loop hands out.

**No Rust counterpart**, since Rust has no loop to join to. [`plaza_flame`](../plaza_flame/API_REFERENCE.md#property-plazatimeline) drives one for you.

| Member | Notes |
|---|---|
| `void observe(int stampMs, int recvMs)` | Feeds both the arrival monitor and the clock. |
| `void resync(int newestStampMs, [double strength = 0.1])` | |
| `void advance(double dtSeconds)` / `void advanceScaled(double dtSeconds)` | Seconds in, milliseconds inside. |
| `int? get target` | Where on the server timeline to draw. |
| `int delayMs` (get/set) | |
| `double get neededDelayMs` | What the measured stream says the delay should be. |
| `bool get underBudget` | Whether the delay in force is shorter than the stream needs, which is the condition that starves interpolation. |
| `void reset()` | Drops the measurements and un-starts the clock, keeping the delay. For a resume. |

**The delay is not adapted automatically.** A delay that follows the link hides bad links instead of reporting them, so `neededDelayMs` is a reading and moving the delay stays your decision.

## 7. Extrapolation

### Class `ExtrapolationBase`

```dart
class ExtrapolationBase<S, V> {
  ExtrapolationBase({
    required S state,
    required V velocity,
    required int serverTimestamp,
    required ClientTimeMs clientReceiptTimeMs,
    required S Function(S state, V velocity, double dtSecs) extrapolateBy,
  });
}
```

The last authoritative state and velocity for an entity, as the basis for extrapolation.

#### Method `at`

```dart
S at(ClientTimeMs targetClientRenderTimeMs, int maxExtrapolationDurationMs)
```

The state extrapolated to the target, **capping the duration** at `maxExtrapolationDurationMs` past receipt. A target before receipt returns the base state: extrapolation predicts forward, and the past is interpolation's job.

The Rust signature returns an `Option` whose `None` no path produces, so this returns the state directly.

#### Property `overExtrapolations`

`int`. How many calls to `at` asked for a time past receipt by more than the cap they were given.

The Rust original logs a warning here, and says at length what it usually means: reaching this **steadily** is almost never a starved link, it is a **render target computed the wrong way**. A target derived from an absolute clock estimate sits ahead of the newest sample by the whole link delay, so the view never interpolates and every entity is drawn held or dead reckoned. On screen that is remote entities stuttering or overshooting.

### Class `TrajectoryPredictor`

```dart
class TrajectoryPredictor {
  TrajectoryPredictor({required double damping, required int maxHorizonMs});
}
```

Second-order dead reckoning: coasting a remote entity through a gap using where it was *heading*, not just how fast it was going.

[`ExtrapolationBase`](#class-extrapolationbase) coasts on the velocity a snapshot carried, which is first order and therefore exactly wrong for anything turning: a target on a curve is projected straight off the tangent, and the longer the gap the further off it flies.

**Scalar on purpose**, matching [`ScalarKalman`](#class-scalarkalman): run one per axis. A generic-over-state version would need a vector-space bound every consumer would then have to satisfy, for arithmetic the consumer can do in two lines.

| Member | Notes |
|---|---|
| `void observe(int timeMs, double value)` | Keeps the last three. Samples at or before the newest are **ignored**: a straggler arriving out of order would invert the fitted derivatives and send the prediction backwards. |
| `double? predict(int timeMs)` | Null until a sample has arrived. With one sample it holds; with two it is first order; with three it is the damped curve. Degrading by sample count rather than refusing to answer is what lets you use it from the first packet. Times before the newest sample use the same polynomial, so it interpolates as readily as it extrapolates. |
| `double? get velocity` | From the newest pair, per second. Null below two samples. |
| `double? get acceleration` | Across the two most recent intervals, per second squared, **centred**. Null below three samples. Undamped: `predict` applies the damping. |
| `int? get newestTime`, `int get samples` (0 to 3), `void reset()` | |

`damping` scales the acceleration term: 0 is plain constant-velocity dead reckoning, 1 is the full quadratic. **Around 0.5 is the usual choice**, because a fitted acceleration is the noisiest thing three samples can tell you and trusting it fully turns measurement noise into visible overshoot.

`maxHorizonMs` clamps how far past the newest sample a prediction may reach; beyond it the projection is evaluated *at* the horizon and held, which stops a lost stream flinging an entity off the map. There is no safe unbounded setting, which is why it is a constructor argument rather than an option.

## 8. Smoothing

### Class `ErrorSmoother`

```dart
class ErrorSmoother<S> {
  ErrorSmoother(double durationSecs, {Easing easing = linear});
  Easing easing;
}
```

Eases a rendered position toward a logical one after a correction.

**Holds no copy of the logical state**: the live value is passed to `sample` each frame. The blend *across states* is the `lerp` you supply; the easing only says how far along to be. The two are independent.

A duration of zero makes every correction snap, which disables smoothing without branching at the call site.

| Member | Notes |
|---|---|
| `void beginFrom(S renderedBeforeCorrection)` | Starts easing from where the entity was last drawn. Calling again mid-ease restarts from the new point. |
| `void advance(double dtSecs)` | No effect when not easing. |
| `S sample(S logical, S Function(S a, S b, double t) lerp)` | Where to draw this frame. Blends from the captured pre-correction position toward the live `logical`, which keeps moving as prediction continues. |
| `void reset()` | Abandons any ease. **For a discontinuity**: a teleport, a respawn, a level load. Easing across one slides the entity through everything in between, which is worse than the snap the ease exists to avoid. |
| `bool get isEasing` | |

### Easing functions

```dart
typedef Easing = double Function(double t);
```

| Function | Shape | When |
|---|---|---|
| `linear` | identity | Constant-speed catch-up. The default. |
| `smoothstep` | zero velocity at both ends | The usual choice for hiding a correction. |
| `easeOutCubic` | quick, settling softly | When the correction should visibly begin at once but land gently. |
| `easeInQuad` | gentle then fast | Prefer over `easeInCubic` whenever the motion must stay *visible* for its whole duration. |
| `easeInCubic` | barely moves, then rushes | **Wrong for a reconciliation correction.** Right for something drawn toward a target under a force that grows as it closes. |
| `easeInOutQuad` | gentle at both ends | A lighter `smoothstep`. |

Cubic covers only 12.5% of the distance in the first half of the time, which over a short animation reads as an object sitting still and then teleporting. Quadratic covers 25% and still finishes fast.

## 9. Estimators

### Class `RttEstimator`

```dart
class RttEstimator { RttEstimator([double alpha = 0.1]); }
```

Smooths round-trip samples into a stable estimate: an exponential moving average, the running minimum, and RFC 6298 mean deviation. The minimum approximates true latency because jitter only ever adds delay, never subtracts it.

`alpha` is the moving-average weight of each new sample, clamped to `(0, 1]`. Smaller is steadier but slower to react.

| Member | Notes |
|---|---|
| `void observe(int rttSampleMs)` | |
| `void observePong(int originTimeMs, int nowMs)` | **Saturating**, so a reply stamped after its own arrival reads as zero rather than as a negative round trip that then poisons the average. Rust's `saturating_sub` has no Dart operator; see [section 13](#13-saturating-arithmetic). |
| `double? get rttMs`, `double? get oneWayMs`, `double? get minRttMs` | Null before the first sample. |
| `double? get jitterMs` | Smoothed mean deviation. Size a dynamic interpolation buffer from this, larger when the connection is unstable. |
| `void clear()` | |

### Class `ClockSyncEstimator`

```dart
class ClockSyncEstimator { ClockSyncEstimator(int window); }
```

Fits the client-to-server clock offset **and skew** by least squares over a sliding window.

Offset alone treats the server clock as a fixed distance away. Real clocks run at slightly different rates, so over a long session the true offset ramps, and a fitted line tracks that ramp where an average lags it.

**What it cannot recover**: a round trip measures total delay, not each leg, so where the network is asymmetric the one-way offset is unrecoverable from RTT alone. Regression recovers the drift rate cleanly; it does not recover the asymmetric constant. Size the interpolation buffer to absorb the residual.

`window` is at most this many recent measurements; 16 to 64 is typical. Throws `ArgumentError` below 2, since a line needs two points.

| Member | Notes |
|---|---|
| `void observe(double localMs, double offsetMs)` | `offsetMs` is `serverTime - localTime` observed when the local clock read `localMs`. |
| `void observeExchange(double localSend, double serverRecv, double localRecv)` | Derives the offset from a symmetric round trip. Where delay is asymmetric this offset carries that error. |
| `double? offsetAt(double localMs)` | Along the fitted line, so it interpolates within the window and extrapolates past it. With one sample, that sample's offset. |
| `double? serverTimeAt(double localMs)` | `localMs` plus the fitted offset. |
| `double get skew` | How fast the offset changes per unit of local time. Multiply by 1e6 for parts per million. Zero until a line can be fit. |
| `bool get isReady`, `int get sampleCount`, `void clear()` | |

Dart `double` is IEEE 754 binary64, matching the Rust `f64`, so this port is bit-comparable.

### Class `ArrivalMonitor`

```dart
class ArrivalMonitor { ArrivalMonitor(double smoothing); }
```

Smoothed statistics over one stream's arrivals: the terms of the render-delay budget, **measured rather than configured**.

A client cannot be told the send rate or the delay, and being told would be worse anyway: a configured rate is wrong exactly when the server changes it, which is when it matters.

Two decisions the Rust original records as learned the hard way. **The buffer covers irregularity, not delay**, so the jitter term is the smoothed mean deviation of lateness rather than the lateness itself: a steady 200ms link needs no more buffer than a steady 20ms one. And **the interval is measured between declared stamps, not arrivals**, because two packets can arrive in one poll and still describe moments an interval apart.

`smoothing` is the EWMA weight, 0 to 1. Around 0.05 follows a link's drift without chasing individual packets.

| Member | Notes |
|---|---|
| `void observe(int stamp, int recv)` | `stamp` is the declared server time a packet describes, `recv` the client's synced estimate of server time at arrival. Call for **every** packet, reordered or not: a stamp older than the newest still updates lateness, because it *is* late and that is data, but never the interval, which is measured forward only. |
| `double get intervalMs` | The send interval as it actually is, whatever the server was configured to. |
| `double get latenessMs` | With an honest clock sync, the link's one-way delay plus whatever error the sync carries. |
| `double get jitterMs` | The irregularity the buffer exists to cover. |
| `double get neededDelayMs` | `lateness + jitter + interval`. Whether to *adapt* to it is your decision, since a delay that follows the link hides bad links. |
| `bool get warmedUp` | Whether at least two forward stamps have been seen, so an interval exists. |
| `void reset()` | For a resume. |

Lateness is seeded by a flag rather than a zero sentinel, because zero is a legitimate mean: a loopback client's lateness really is 0ms, and treating that as unseeded re-seeds on every packet and freezes the jitter at its initial value.

### Class `ScalarKalman`

```dart
class ScalarKalman {
  ScalarKalman(double processNoise, double measurementNoise);
  factory ScalarKalman.seeded(double processNoise, double measurementNoise,
      {required double estimate, required double variance});
}
```

A one-dimensional Kalman filter over a scalar signal.

[`RttEstimator`](#class-rttestimator) smooths with a fixed-weight moving average: cheap, tuning-free and the right default. A moving average trusts every sample equally for ever, though. This tracks how *confident* it is and weights each measurement against that, so it settles quickly then rejects jitter once settled.

Two knobs, and they are the point of a building block:

- **process noise** (Q): how much the true value is expected to wander between samples. Higher trusts new measurements more, faster and jumpier.
- **measurement noise** (R): how noisy each reading is. Higher smooths harder, slower and steadier.

| Member | Notes |
|---|---|
| `double observe(double measurement)` | Folds in a measurement and returns the updated estimate. The first call takes the measurement as the estimate. |
| `double get estimate`, `double get variance` | Variance shrinks as it settles, grows under process noise. |
| `double get lastGain` | 0 to 1: near 1 while settling and trusting measurements, near 0 once settled and rejecting jitter. |
| `bool get isInitialized` | |
| `set processNoise`, `set measurementNoise` | Retunable live. Measurement noise is floored just above zero to keep the gain finite. |
| `void reset()` | The next measurement re-seeds. |

### Class `CorrectionMonitor`

```dart
class CorrectionMonitor {
  CorrectionMonitor({double smoothing = 0.03, double sigma = 4.0, double floor = 0.0, int warmup = 32});
}
```

A running picture of prediction error, and an **adaptive** test for what counts as abnormal.

There is no fixed normal. A thirty-pixel correction is unremarkable at one send rate and alarming at another, and the same holds across latency settings. A constant threshold reports whatever it was tuned against, so it goes quiet exactly when conditions change and noisy for reasons unrelated to any bug.

| Member | Notes |
|---|---|
| `bool record(double magnitude)` | Folds a correction into the baseline and reports whether it was abnormal. |
| `bool isAbnormal(double magnitude)` | Without recording it. |
| `double get threshold` | `norm + band`. |
| `double get band` | The band above the mean, never below the floor. |
| `double get norm` | What "normal" currently means. |
| `double get peak` | The largest correction ever recorded, unclamped. |
| `(int, int) get counts` | Recorded, and how many were abnormal. |
| `bool get isWarmingUp` | |
| `void reset()` | Forgets the baseline, keeping the tuning. |

**The sample is clamped to the threshold before it updates the baseline.** Without that, one respawn-sized correction lifts the mean and variance so far that genuine problems hide underneath for the next thousand packets. Clamping still lets a *sustained* shift move the baseline, which is what you want.

Two things differ while warming up: nothing is flagged, because a baseline starting at zero says every correction is enormous; and the baseline is averaged **exactly** rather than exponentially, because an exponential average approaches the truth from zero and would still be far short of it when flagging began, so the first real samples would trip a threshold built from a norm never reached.

**Set the floor.** A spell of near-perfect prediction drives the variance toward zero, and without a floor the band collapses with it and every pixel of ordinary jitter reads as an outlier. It answers "how large a correction do I not care about, ever".

### Class `Correction`

```dart
class Correction<S> { const Correction({required S seen, required S settled}); }
```

A correction, as the two states it moved between: `seen` is where the entity was being drawn before it landed, `settled` is the logical state after snapping and replaying.

**No distance metric is imposed**: that would put a constraint on every user for the benefit of the ones that want telemetry. The caller knows its own units, so the subtraction is its business.

## 10. Bookkeeping

### Class `AckWindow`

```dart
class AckWindow {
  AckWindow();
  factory AckWindow.fromEncoded(int newest, int mask);
}
const int ackWindow = 64;   // how far back the mask reaches
```

A newest sequence plus a bitmask of the ones before it, the shape every reliable-over-unreliable protocol uses.

| Member | Notes |
|---|---|
| `(int, int)? encode()` | The pair to put on the wire, or null if nothing has arrived. |
| `bool observe(int seq)` | Records an arrival, returning whether it was new. Handles reordering: a straggler arriving after a newer packet lands in its own slot rather than being taken for the new newest. |
| `int? get newest`, `int get mask` | Bit `i` is `newest - 1 - i`. |
| `bool contains(int seq)` | Anything outside the window is false, including sequences newer than the newest seen. |
| `Iterable<int> missingSince(int oldest)` | The gaps up to the newest, ascending: what a sender resends. Clamped to the window, so a peer that has fallen far behind asks for a bounded amount of work. |
| `int get receivedInWindow` | How many slots are filled, the newest included. |
| `void reset()` | |

#### Method `contiguousBase`

```dart
int? contiguousBase(int first)
```

The newest sequence such that **everything** from `first` up to it arrived.

Not the same as `newest`, and the difference is load-bearing. A protocol that **retransmits** wants the mask. A protocol that **re-derives** wants a state the peer provably reached, and receiving N+1 after losing N does not put a peer in the state N+1 implies: whatever N announced and N+1 had no reason to repeat is gone. Taking the newest set bit hands the sender a state that never existed, and the resulting divergence is permanent and close to invisible. Measured, it made loss recovery statistically indistinguishable from no recovery at every loss rate.

Null when the run is empty, covering two cases a caller treats alike: `first` did not arrive, or it is older than the window can speak about. Neither is a reason to move the frontier backwards.

### Class `SetDigest`

```dart
class SetDigest {
  SetDigest();
  factory SetDigest.fromKeys(Iterable<int> keys);
}
int mix64(int x);   // SplitMix64's finalizer
```

An order-independent digest of a set of keys, maintainable incrementally.

A delta-relevance stream has a silent failure mode: the client applies entered and left deltas to keep a local mirror, and if one is lost or misapplied the mirror is wrong for good, with no symptom. Bandwidth looks normal, positions look normal, and the only evidence is on the screen. The cure is for both sides to summarise their set cheaply and compare.

Order independence is what shapes it: two peers holding the same set may iterate in different orders. Summation gives that, and unlike XOR it does not silently cancel duplicates. Because the combine is addition, a key can be added or removed in constant time.

**This must agree with the Rust implementation bit for bit.** If each side computed its own fold, a disagreement in the arithmetic would be indistinguishable from a disagreement about the world, and the recovery machinery would fire forever chasing a bug that was only ever in the hashing. `mix64` uses `>>>` rather than `>>`, because Dart's `>>` sign-extends and this is a `u64` algorithm where every shift must be logical. `fixtures/digests.txt` pins the values, including keys above 2^63 where the two would otherwise part company.

| Member | Notes |
|---|---|
| `void insert(int key)` | Adding the same key twice is deliberately **not** idempotent: the digest tracks multiplicity, so a double-insert is itself a detectable mistake. |
| `void remove(int key)` | Exactly undoes an `insert`. |
| `int get digest` | The value to compare across the wire. Folds in the cardinality, so two sets whose key hashes happen to sum alike still differ if their sizes do. |
| `int get length`, `bool get isEmpty`, `void clear()` | |

### Class `SlotKey`

```dart
class SlotKey {
  const SlotKey(int index, int generation);
  static SlotKey decode(int key);
}
```

A storage slot and the generation of its current occupant.

A server keeping entities in a dense recycled array has the cheapest possible identifier in the array index, and it is wrong in a way that is very hard to see. Slot 41 dies and is refilled on the same tick; every message about the old occupant still in flight now names the new one, and neither side can notice, because the index is valid and the message is well formed.

**Both sides must encode the pair identically**, or their digests disagree about a world they hold identically. That is why this is a type rather than two agreeing comments, and why `encode` is pinned by a conformance fixture.

| Member | Notes |
|---|---|
| `int encode()` | `(index << 16) | generation`. An index past 2^48 would collide; nothing this is for comes close. |
| `SlotKey ungenerational()` | The same slot with its generation dropped. For running deliberately without generations, which is how you demonstrate what they are for. |
| `bool sameOccupant(SlotKey other)` | Same slot *and* same occupant. |

Value equality and `hashCode`, so keys work in sets and maps.

### Class `SlotAllocator`

```dart
class SlotAllocator { SlotAllocator({ReusePolicy policy = ReusePolicy.lifo}); }
enum ReusePolicy { lifo, fifo }
```

Hands out [`SlotKey`](#class-slotkey)s over a dense index space, recycling freed slots and bumping a generation so stale handles stay detectable.

**It does not store your entities.** Keep them in a list indexed by `SlotKey.index`, which is what the rest of these utilities expect.

**The ceiling, stated out loud.** The generation is 16 bits, so a single slot freed 65,536 times wraps and a handle from exactly that many reuses ago aliases the current occupant. Nothing can detect the wrap, so the mitigation is width and, for a long session, `ReusePolicy.fifo` to spread reuse across the index space instead of hammering the same slots.

| Member | Notes |
|---|---|
| `SlotKey alloc()` | Indices are dense: the space only grows when nothing is free, so it settles at the high-water mark of simultaneously live entities rather than the total ever created. |
| `bool free(SlotKey key)` | Returns whether it freed anything: a key naming an occupant already gone is **refused** rather than freeing whoever moved in. The generation is bumped here, on free, rather than when the slot is next taken, so an outstanding handle stops naming anything the moment its subject dies. |
| `bool isLive(SlotKey key)`, `bool isOccupied(int index)` | |
| `SlotKey? keyAt(int index)` | How a bare index becomes a handle that can go on the wire. |
| `Iterable<SlotKey> get keys` | Every live key, in index order. |
| `int get length` | Live count. |
| `int get indexSpace` | How many indices exist, live or free. **This and not `length` is the number to size a list by.** |
| `void clear()` | Bumps each live generation so outstanding handles are invalidated rather than silently matching a rebuilt world. Keeps the index space. |
| `ReusePolicy policy` (get/set) | Neither is more correct. Prefer `lifo` unless something downstream cares about clustering, and if it does, measure rather than assume. |

### Class `DeltaMirror`

```dart
class DeltaMirror<E> { DeltaMirror({bool generational = true}); }
```

The client's mirror of a streamed entity set: the keying, the agreement and the counters. The entity type is yours, so an application keeps whatever it needs per entity beside it.

**This must agree with the server's `DeltaBaseline` exactly.** In Rust that is guaranteed by `plaza_server_utils` re-exporting the very same type. A Dart port cannot inherit that guarantee, so the operations are pinned by conformance fixtures instead.

| Member | Notes |
|---|---|
| `void begin(int seq, {required bool fullBaseline})` | Opens a packet: notes what the wire lost, acknowledges the sequence, and **clears the mirror** if this packet is a full baseline. Call once per packet, before applying anything in it. A baseline is the server's repair for a mirror it can no longer reach by deltas, so the old contents must go rather than be merged with. |
| `void insert(SlotKey key, E entity)` | Files an entity, replacing whatever was in the slot. |
| `E? remove(SlotKey key)` | Removes only if the key names the occupant actually held. A generation mismatch counts as a stale reference and removes nothing, which is the entire point: without the check this deletes a live entity that merely inherited the slot. |
| `E? operator [](SlotKey key)` | Null on a generation mismatch, without counting it. |
| `E? forUpdate(SlotKey key)` | For applying a sample. Counts a mismatch as a stale reference. Returning null rather than the current occupant is what keeps a position meant for a dead entity off a live one. |
| `bool update(SlotKey key, E entity)` | |
| `bool contains(SlotKey key)` | |
| `Agreement settle(int expected)` | Closes a packet: recomputes the digest and compares. The check a lost or malformed removal cannot hide from, because it is over the whole set rather than over the messages that happened to arrive. |
| `Divergence divergenceFrom(Iterable<int> serverKeys)` | For the debugging mode that ships the truth beside the digest. Cheap enough to call on a mismatch, far too expensive to send every packet. |
| `int get digest` | As of the last `settle`. Send this on the next acknowledgement. |
| `int computeDigest()` | Of everything held right now. |
| `AckWindow get acks` | |
| `Iterable<SlotKey> get keys`, `Iterable<(SlotKey, E)> get entries`, `Iterable<E> get values` | Index order. |
| `int get length`, `bool get isEmpty`, `void clear()`, `int? get appliedSeq` | |
| `bool generational` (get/set) | With generations off, every reference matches whatever occupies the slot, which is the bug generations exist to prevent, available on demand so it can be demonstrated. |

#### Counters

| Counter | Meaning |
|---|---|
| `int get framesLost` | From gaps in the sequence. **The direct measure**, and what separates "the network dropped it" from "we corrupted it". |
| `int get staleRefs` | References to occupants that had already gone. Climbing means the server is naming entities this mirror has moved past. |
| `int get divergences` | Times `settle` disagreed with the server. |

#### Sealed class `Agreement`

```dart
sealed class Agreement { bool get agreed; }
class Agreed extends Agreement {}
class Diverged extends Agreement { final int held; final int expected; }
```

`Diverged` carries both digests so a report can say so, though neither number says *how*. For that, use `divergenceFrom`.

#### Class `Divergence`

```dart
class Divergence {
  const Divergence({required List<SlotKey> extra, required List<SlotKey> missing});
  bool get isEmpty;
}
```

Which side the difference falls on names the bug: `missing` means something was lost or never sent, `extra` means a removal never landed or was rejected.

### Class `InputCoalescer`

```dart
class InputCoalescer<I> {
  InputCoalescer(int keepaliveMs);
  bool enabled;
}
```

Decides whether this frame's input needs to go on the wire. Against a server that holds an input and integrates it every tick, sending the same direction sixty times a second says nothing it does not know.

What is *transmitted* is a bandwidth decision; what is *integrated* is a simulation decision. Keeping them separate is what makes coalescing safe: local prediction advances every tick whatever the wire is doing. It also means this pairs with a held-input server and **not** with one that consumes one input per step, where dropping repeats drops actual movement.

**The keepalive is not optional.** Sending purely on change fails under loss: the server holds the last direction it received, so a *dropped* change is not a missing update but a wrong state that persists until the player presses something else. It reads as the controls sticking and looks nothing like packet loss. Pick the interval against how long a wrong direction is tolerable, not against bandwidth.

| Member | Notes |
|---|---|
| `bool shouldSend(I input, int nowMs)` | Requires a meaningful `==` on `I`. |
| `I? get lastSent`, `void reset()` | |
| `bool enabled` | False makes every call return true, for comparing against the uncoalesced stream. |

### Class `TickNamer`

```dart
class TickNamer {
  TickNamer({required int stepMs, int playoutDelayMs = 0});
  int playoutDelayMs;
}
```

Names the tick an input is meant for, **floored by what the stream has proven**.

The clock names the tick; the newest arrived stamp bounds it from below. The server wrote that stamp, so server time is provably past it, and aiming behind it is a rejection bought in advance.

This matters most after a resume, when a clock fit can trail the stream by hundreds of milliseconds while its window refills. Measured in the Rust `horde_playground`: aiming five ticks behind a four-tick accepting window dropped every input. The floor keeps them inside the window with no clock involved at all.

It only ever lifts the aim, and never past the ideal: a stamp trails true server time by the one-way delay, so `stamp + depth` is at most where a perfect clock would have aimed.

| Member | Notes |
|---|---|
| `void observeStamp(int stampMs)` | Only ever moves forward. |
| `int tickFor(int serverNowMs)` | An intention, not a claim: the server decides whether that tick is open. |
| `bool floorApplies(int serverNowMs)` | Whether the floor is doing the work, which is the signal that the clock is trailing the stream. |
| `int get newestStampMs`, `void reset()` | |

**No Rust counterpart in the crate.** Extracted from `horde_playground`'s client, where the rule still lives inline.

## 11. Timing

### Class `PlayoutBuffer`

```dart
class PlayoutBuffer<T> { PlayoutBuffer({required int maxQueued, required int lostAhead}); }
enum Admission { queued, timelineLost }
```

The playout queue: push on arrival, pop what is due at the render instant.

`stamp` is the instant a packet describes, in the application's units; `order` is its sequence number, which is what playout is **ordered by**, so deltas compose in the order the server built them even when arrivals interleave.

`maxQueued` bounds the queue absolutely: size it several times past what an honest buffer holds at the deepest render delay and fastest send rate, so reaching it means something is wrong rather than merely slow. `lostAhead` is the discontinuity threshold; match it with the server's stalled-subscriber threshold, so both sides agree on when a gap stops being jitter.

| Member | Notes |
|---|---|
| `Admission push(int stamp, int order, T item, int? renderAt)` | `renderAt` null before the timeline has started, during which nothing can be late and nothing can be a discontinuity. |
| `T? popDue(int renderAt)` | The oldest packet whose instant the clock has reached, in sequence order. Call in a loop each tick until it returns null. |
| `void timelineLost()` | The transport's verdict arriving from outside: a resume backlog discarded unread, a reconnect. Drops everything but the newest. |
| `int get underruns` | Packets that arrived after the instant they describe had been drawn, by a margin jitter produces. **The number that says the render delay is too small for this link.** |
| `int get restarts` | How many stalls were survived. Counted per restart rather than per packet dropped, so it does not scale with how large a given backlog happened to be. |
| `Iterable<T> get items`, `int get length`, `bool get isEmpty` | |

#### Enum `Admission`

`Admission.queued` means nothing to do until the clock reaches it.

**`Admission.timelineLost` is a request to the caller.** The gap is a discontinuity, not a delay. The buffer has already dropped everything but the newest packet; you must now restart your own timeline: re-anchor the render clock on what just arrived and **drop derived state, the entity mirror above all**, so the stream's own recovery rebuilds it.

### Class `FixedTimestep`

```dart
class FixedTimestep {
  FixedTimestep.fromStepMs(int stepMs, {int maxFrameMs = defaultMaxFrameMs});
  factory FixedTimestep.fromHz(int hz, {int maxFrameMs = defaultMaxFrameMs});
}
const int defaultMaxFrameMs = 250;
```

Turns real elapsed time into a whole number of fixed simulation steps. Engine-agnostic on purpose: some Dart engines provide a fixed step and some do not.

`fromHz` uses integer division, so rates that do not divide 1000 evenly truncate: 60Hz is 16ms rather than 16.667. Deliberate at millisecond resolution, and it means both sides of a wire agree exactly as long as they agree on the rate.

`defaultMaxFrameMs` is a quarter of a second, or fifteen steps at 60Hz: enough that an ordinary hitch is caught up smoothly, small enough that a resumed tab skips ahead instead of grinding through the minutes it was asleep.

| Member | Notes |
|---|---|
| `Steps advance(int elapsedMs)` | The accumulator is drained **here** rather than as the steps are consumed, so the time is spent whether or not the caller runs every step. |
| `int stepMs` (get/set) | Changing the step of a *simulation* is not free the way changing a send rate is: the step size is part of the rule, so two peers integrating at different steps diverge even running identical code. |
| `double get stepSecs`, `int get pendingMs` | |
| `double get alpha` | How far between the last step and the next, 0 to 1. For rendering between fixed steps: interpolating the drawn state by this removes the stutter a fixed step shows when the step rate and the refresh rate disagree. Worth knowing it exists, because the usual first diagnosis of that stutter is that the step rate is too low. |
| `int get droppedMs` | Elapsed time the catch-up cap refused, in total. Real time the simulation never ran. Non-zero after a backgrounded tab or a sleeping machine, and worth surfacing: a world quietly behind wall time explains a whole class of "it desynced and I do not know when". |
| `int maxFrameMs` | Lower means a resumed tab catches up less and skips more. |
| `void reset()` | Discards the carried remainder. Leaves `droppedMs` alone, which is a session total. |

### Class `Steps`

```dart
class Steps extends Iterable<int> { final int stepMs; }
```

The steps one `advance` paid for. Each item is the step duration in milliseconds, which is the value the simulation must advance by. **Taking it from here rather than from the frame delta is what stops a caller stepping by the wrong amount.**

### Class `Periodic`

```dart
class Periodic {
  Periodic(int intervalMs);
  factory Periodic.fromHz(int hz);
}
```

Something that should happen every interval. The same accumulator as [`FixedTimestep`](#class-fixedtimestep) with a different consumption rule, and separate because the two answer different questions. A fixed step asks "how much simulation does this frame pay for", where every step must run or the world falls behind. A period asks "is it time yet", where the work is usually idempotent and running it twice in one frame is waste rather than correctness.

| Member | Notes |
|---|---|
| `bool due(int elapsedMs)` | Says whether the period elapsed, **at most once**. The remainder carries, so the average rate stays exact, and time beyond a single interval is kept rather than discarded, so a long frame is repaid on the following ones rather than resetting the phase. |
| `int advance(int elapsedMs)` | How many whole periods the elapsed time covers. For work where each occurrence matters (spawning a wave, firing a weapon). |
| `int intervalMs` (get/set), `int get remainingMs`, `void reset()` | Setting keeps whatever has accumulated, so a change takes effect from now rather than restarting the period. |

## 12. Rollback

The peer-to-peer deterministic model, not the server-authoritative one. Each peer predicts every other peer's input and rolls back when a confirmation disproves a guess.

`Frame` is an `int` frame index. Rollback counts in fixed frames, not wall time: two peers agree on "frame 900", never on a millisecond.

### Class `RollbackSession`

```dart
class RollbackSession<S, I> {
  RollbackSession({
    required S initialState,
    required List<I> neutralInputs,
    required S Function(S state, List<I> inputs) advance,
    I Function(I last, Frame frame)? predictor,
    RollbackConfig config = const RollbackConfig(),
  });
}
```

The whole rollback loop for one peer, wired: a [`StateHistory`](#class-statehistory), an [`InputTimeline`](#class-inputtimeline) per player, and the current frame, driving the predict / detect / rollback / re-simulate cycle.

Each peer runs its own session and calls its local player index the "local" one; the two are otherwise identical, which is the point, both re-simulate to the same state from the same inputs.

**`advance` must be deterministic**: same state and inputs in, same state out, every time and on every peer. That is what rollback rests on.

**`I` must have a meaningful `==`**: that comparison is how a confirmation is judged against the guess it replaces. A type with identity equality reports every confirmation as a misprediction.

`predictor` is null by default, meaning [`repeatLastInput`](#function-repeatlastinput). Dart cannot name that as a default value here, because a constant tearoff may not close over a type parameter.

| Member | Notes |
|---|---|
| `void queueLocalInput(int player, I input)` | Local inputs are known before their frame runs, so they are never mispredicted. Call once per frame before `advanceFrame`. |
| `void confirmRemoteInput(int player, Frame frame, I input)` | If it contradicts the guess used for an *already-simulated* frame, the session marks that frame for rollback on the next `advanceFrame`. |
| `void advanceFrame()` | Rolls back and re-simulates if needed, then simulates the current frame, predicting any input not yet known. |
| `void resolvePendingRollback()` | Applies a pending correction without simulating a new frame. `advanceFrame` does this first, so a normal loop never calls it; it is public because the last confirmations of a session arrive after its final frame, and settling on them is the only way to compare the present against a fully-known ground truth. |
| `S get state` | The world as it stands now: the present the peer renders. Includes every predicted input still awaiting confirmation. |
| `S? stateAt(Frame frame)` | The **saved** state, so for a fully confirmed frame it is identical on every peer. That equality is the determinism guarantee. Returns the present for the current frame. |
| `bool isFrameConfirmed(Frame frame)` | Whether every player's input is confirmed. A delay-based peer waits for this; a rollback peer ignores it and predicts. This only reports. |
| `Frame? confirmedFrame(int player)` | |
| `Frame get currentFrame`, `int get numPlayers` | |
| `bool rollbackEnabled` (get/set) | With it off the session still predicts and advances but never restores or re-simulates. Not a way to ship, since predictions never corrected drift a peer out of sync, but it isolates what rollback buys, and it is the mechanism a delay-based front end disables. |
| `int get lastRollbackFrames` | Frames re-simulated by the most recent `advanceFrame`, zero if it did not roll back. |
| `int get maxRollbackFrames`, `int get rollbackCount` | |

### Class `RollbackConfig`

```dart
class RollbackConfig { const RollbackConfig({int maxRollbackFrames = 240}); }
```

Bounds the state and input history retained, so it must comfortably exceed the worst prediction horizon (round-trip latency in frames). Default 240, four seconds at 60fps.

### Function `repeatLastInput`

```dart
I repeatLastInput<I>(I last, Frame frame)
```

The default predictor: repeat the last confirmed input unchanged. Right whenever a player holds their input steady, which dominates most games.

### Class `StateHistory`

```dart
class StateHistory<S> { StateHistory(int capacity); }
```

A frame-indexed ring of whole-world state snapshots. Only the most recent `capacity` frames are kept, which is the maximum distance you can ever roll back.

| Member | Notes |
|---|---|
| `void save(Frame frame, S state)` | Intended use is contiguous: append `frame == latest + 1`, or overwrite a frame already inside the window (re-simulation does this). A save that skips ahead of the window **resets** it, so the buffer never holds a gap. |
| `S? restore(Frame frame)` | Null if evicted or never saved. |
| `Frame? get oldestFrame`, `Frame? get latestFrame` | |
| `int get length`, `bool get isEmpty`, `void clear()` | |

#### Property `resets`

`int`. How many saves fell outside the window and reset it.

Not in the Rust original, which logs a warning. **Non-zero means frames were saved non-contiguously, which rollback assumes never happens.**

### Class `InputTimeline`

```dart
class InputTimeline<I> { InputTimeline(int capacity); }
```

The inputs known for one input source, by frame, with the gaps predicted. A confirmed input is one the source actually produced; an unconfirmed frame is **predicted** by repeating the last confirmed input at or before it.

| Member | Notes |
|---|---|
| `void confirm(Frame frame, I input)` | Frames may arrive out of order, since a resent input can fill a gap left by a lost packet. Missing frames in between are held as gaps until they too are confirmed. A frame older than the retained window is dropped, since it is already past the rollback horizon. |
| `I? confirmedAt(Frame frame)` | Null if that frame is unconfirmed (predicted) or outside the window. |
| `I? lastConfirmedAtOrBefore(Frame frame)` | The basis for predicting `frame` when it is not itself confirmed. |
| `Frame? get lastConfirmedFrame`, `Frame? get oldestFrame` | |
| `int get length`, `bool get isEmpty` | |

## 13. Saturating arithmetic

Rust's checked arithmetic, for the ports that rely on it. Dart's operators wrap on overflow, so `a + b` already matches Rust's `wrapping_add` and needs nothing here. That is load-bearing in [`SetDigest`](#class-setdigest), where the whole point is to reproduce `u64` wrapping arithmetic exactly. What Dart has no operator for is **saturating**, which several ports depend on to keep a bad measurement from becoming a negative one.

**The limit worth stating**: these reproduce Rust's `i64` semantics exactly, because Dart's `int` has the same range and the same two's-complement behaviour. They do **not** reproduce the `u64` versions, because Dart has no `u64` to saturate within. Every use in this package is a millisecond timestamp or a duration, where the meaningful floor is zero. If something ever carries genuine `u64` semantics, the answer is `BigInt` or a documented bound, not this.

```dart
const int intMax = 0x7FFFFFFFFFFFFFFF;   // Rust's i64::MAX
const int intMin = -0x8000000000000000;  // Rust's i64::MIN

int saturatingSub(int a, int b);         // floored at ZERO, not intMin
int saturatingSubSigned(int a, int b);   // clamped to the full signed range
int saturatingAdd(int a, int b);
int saturatingMul(int a, int b);
int? checkedAdd(int a, int b);           // null on overflow
int? checkedSub(int a, int b);
```

`saturatingSub`'s zero floor is deliberate: every caller here is subtracting timestamps, where a negative result means the inputs were impossible (a reply stamped before it was sent, a packet arriving before the moment it describes) and the honest reading is "no elapsed time" rather than a negative duration that then poisons a smoothed average. Use `saturatingSubSigned` for a difference legitimately allowed to be negative, such as a clock offset.

## 14. Math

Optional basic types, provided so this package can stay dependency-free and as the interpolate and extrapolate rules for them. **A Flutter or Flame application already has `vector_math`**, whose `Vector2` is mutable and better integrated with everything around it. These are not competing with it: pass your own library's lerp and extrapolate functions to the primitives that take them.

```dart
const double doubleEpsilon = 2.220446049250313e-16;
double lerpDouble(double a, double b, double t);
```

The Rust original guards its normalisations with `f32::EPSILON`. Dart has no float32, so `doubleEpsilon` is the `double` equivalent, which makes the guard tighter rather than looser.

### Classes `Vec2` and `Vec3`

Immutable, const-constructible, with value equality.

```dart
const Vec2(double x, double y);          static const Vec2 zero, one;
double get length, lengthSquared;
Vec2 normalize();                        // zero for a vector too short to have a direction
operator + - * / and unary -
Vec2 lerp(Vec2 other, double t);         // the rule to hand a SnapshotBuffer
Vec2 extrapolate(Vec2 velocity, double dtSecs);   // the rule to hand a RemoteView
```

`Vec3` is the same surface with a `z`.

### Class `Quat`

```dart
const Quat(double x, double y, double z, double w);
static const Quat identity;
double dot(Quat other);
Quat normalize();
Quat slerp(Quat end, double t);   // spherical, taking the shorter arc
Quat multiply(Quat rhs);          // Hamilton product: composes two rotations
```

`slerp` negates the target when the dot product is negative, so it always takes the shorter arc, and falls back to normalised linear interpolation above a dot of 0.9995 where the two are indistinguishable and the trigonometric form loses precision.

## 15. Network simulation

A **separate entry point**, matching the Rust crate's `net-sim` feature gate: this is a test and demo aid, not part of the client API, and an application should not pull it in by accident.

```dart
import 'package:plaza_client_utils/net_sim.dart';
```

### Class `LatencyLink`

```dart
class LatencyLink<T> {
  LatencyLink({PacketOrdering ordering = PacketOrdering.ordered});
  PacketOrdering ordering;
}
```

A one-way time-ordered delay queue.

| Member | Notes |
|---|---|
| `void send(...)` | Hands a packet to the wire. It may be delayed by the latency plus up to the jitter, or dropped with probability `lossPct / 100`. |
| `List<T> drainDue(int nowMs)` | Every packet whose delivery time has arrived, oldest delivery first. |
| `void enqueueAt(int deliverAtMs, T packet)` | Bypasses latency, jitter and loss. For tests that need a specific arrival order. |

### Enum `PacketOrdering`

`PacketOrdering.ordered` is **the default**: jitter delays a packet, possibly past its successors, but never ahead of its predecessors. That is what TCP, WebSocket, QUIC streams and any ordered channel actually do, so the jittered delivery time is clamped to at least the previous packet's.

`PacketOrdering.unordered` lets jitter reorder freely, so a later packet can arrive first, which is what raw UDP does. **Choose it deliberately**: a delta stream that assumes ordering will diverge under it, which is a real finding on a datagram transport and a phantom on an ordered one.

Loss is independent of ordering: an ordered transport still loses whole connections and, at this level of abstraction, still models a dropped application message.

### Class `Rng`

```dart
class Rng {
  Rng(int seed);
  double unit();        // [0, 1)
  int upTo(int n);      // [0, n] inclusive
}
```

A seeded xorshift64. **The same algorithm and the same seeding as the Rust side**, so a scenario scripted in Rust and one scripted in Dart make the same jitter and loss decisions.

`unit` draws from the top 24 bits, so the quotient is exact in both `f32` and `double` and agrees with Rust exactly rather than approximately.
