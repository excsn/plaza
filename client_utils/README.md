# `plaza_client_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The client half of real-time networking: making a server's authoritative updates feel immediate and smooth. Client-side prediction (for either server input model), server reconciliation, interpolation, extrapolation, and the mirror that holds a streamed entity set and can prove it still agrees with the server. Plus the other netcode family, peer-to-peer deterministic [rollback](#rollback-netcode).

The server half lives in [`plaza`](../core/) under `game_common::reconciliation`. Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_client_utils = "0.6"
```

**No workspace dependencies.** This crate pulls in `thiserror` and `tracing` and nothing else, deliberately, so wasm builds and game-engine plugins do not drag in a server's async runtime. It is pure logic: no transport, no serialization, no engine coupling. You feed it what you receive and read back what to render.

You do not need a Plaza server to use it. Anything speaking a sequence-numbered-input protocol works.

## Two ways in

**The drop-in bundles.** Most clients wire the primitives the same way, so three types package the whole job:

- `PredictedPlayer` is your controlled entity when the server consumes **one input per simulation step**: feed it inputs and server packets, read a render position back. Predict, reconcile, replay, and smooth, wired.
- `HeldInputPredictor` is your controlled entity when the server **holds a direction and integrates it every tick**: dead reckon locally, ease toward each authoritative sample. See [which predictor](#which-predictor).
- `RemoteView` is an entity you do not control: `push` snapshots, `render` a state. Interpolation, extrapolation, and the starvation handling in between, wired.

**Or the primitives underneath**, if you want finer control. The bundles are built from them and nothing more:

| Problem | Piece |
|---|---|
| Local input should feel instant, but the server decides | `PredictedEntity` + `ClientInputBuffer` |
| Other players' updates arrive discretely and jittery | `SnapshotBuffer` (+ `InterpolationClock` for the render target) |
| Updates stop arriving for a moment | `ExtrapolationBase` |
| A correction snaps the local entity to a new spot | `ErrorSmoother` |
| Knowing whether a correction was normal, without a threshold you tuned once | `CorrectionMonitor` (running mean and variance, adaptive) |
| The render clock drifts as latency changes | `InterpolationClock::resync` (position) or `observe_rate` (playback-rate glide) |
| A variable frame has to drive a fixed-step simulation | `timestep::FixedTimestep` (and `Periodic` for "is it time yet") |
| Measuring round-trip latency to the other end | `Timeline` (a probe's bookkeeping), over a `plaza_wire` `Kind::Ping` frame |
| Estimating server time when the clock fit is cold or trailing the stream | `Timeline::server_time_ms`, floored by the newest server stamp (`note_stamp`) carried forward at wall rate |
| Arithmetic that must agree to the bit across builds, because the wire carries causes rather than state | `fixed` (`Fx`, `P`), feature `fixed` |
| Tracking clock offset **and drift** against a server | `ClockSyncEstimator` (least-squares offset + skew) |
| Optimally smoothing one noisy signal (jitter, latency) | `ScalarKalman` (a 1D Kalman filter) |
| Telling the other side what arrived, in twelve bytes, however bad the link | `ack::AckWindow` (a sliding-window bitmask) |
| Coasting a *turning* entity through a long gap, not off its tangent | `trajectory::TrajectoryPredictor` (a damped quadratic fit) |
| Sending an input only when it changes, without the state sticking on a lost packet | `coalesce::InputCoalescer` |
| Holding the client side of a streamed entity set, and proving it still agrees | `mirror::DeltaMirror` (+ `SetDigest`, `SlotKey`, `SlotAllocator`) |

Each is usable on its own. The [`netcode_playground`](../examples/netcode_playground/) example wires the whole picture together interactively, through the bundles.

## Which predictor

The two local-player bundles differ by how the **server** consumes input, not by how the client feels, and choosing wrong is silent: it shows up as a prediction that is always slightly behind.

| the server | use |
|---|---|
| consumes one input per simulation step | `PredictedPlayer` (replay unacknowledged inputs) |
| holds an input and integrates it every tick | `HeldInputPredictor` (dead reckon and ease) |

Replaying inputs against a server of the second kind double counts, and it gets worse the more you economise on bandwidth, because one coalesced input can cover a long stretch of simulation. `InputCoalescer` pairs with `HeldInputPredictor` for exactly that reason and explicitly not with `PredictedPlayer`, whose server needs every input.

Both share the same lifecycle vocabulary, so they read as one family: `set_active` (the server is holding this entity still, so stop integrating into it), `teleport` (a discontinuity, which must not be eased), a prediction context (`set_context`, for a rule that needs the world to run), and a `Correction` returned from `reconcile` for `CorrectionMonitor` to measure.

## Four principles worth knowing before you predict or render anything

None is enforceable by a type, and between them they account for every netcode bug found while building the playground examples. They prevent bugs, where everything else here only recovers from them. The first two are about simulation, the last two about rendering.

**A shared rule must be shared code, not code written twice.** The `apply` you hand a predictor is meant to *be* the server's step function, not a client approximation of it. Anything the server does that your copy leaves out arrives as a permanent correction: it looks like network jitter, it is largest exactly when it is most visible, and it is expensive to find later. If your rule needs the world to run (gravity, wind, a moving platform), that is what the context parameter is for; being unable to pass the world in is what pushes people into writing the second, lesser rule.

**Prediction is presentation; shared rules consume authoritative state.** Feeding a locally predicted position into a rule that *both* sides run creates a second, divergent world, and every packet then fights the local one. Prediction drives the camera and the local player's own marker. The rules both sides run read the authoritative state, even though it is older.

**One instant per frame.** A client that renders in the past picks a single instant T for the whole frame, and everything is evaluated at T: not only where entities are drawn, but everything a behaviour rule reads while producing the frame, aim targets and chase context included. An entity simulated to T while reading a target from the newest packet is two timelines in one scene, and the seam between them is a bug whether or not it is visible yet.

**The timeline comes from declaration, not arrival.** Transport facts, round trips and jitter and arrival times, may size buffers and admit or refuse connections. They never decide which moment is on screen or when an input executes; those are declared numbers the server chooses and publishes. A render clock steered by packet arrival hides bad links instead of reporting them, lets every client pick a different "now", and quietly makes ping an input to the game.

All four are drawn from measurement rather than taste: see [examples/LEARNINGS.md](../examples/LEARNINGS.md).

## Rollback netcode

The pieces above serve the *server-authoritative* model. The `rollback` module serves the other family, *peer-to-peer deterministic lockstep*: there is no server, peers run the **same deterministic simulation** and exchange only inputs. A peer cannot wait for a remote input and stay responsive, so it **predicts** the missing one (repeat the last), simulates ahead, and **rolls back** to re-simulate when the real input arrives and disagrees. Determinism is what makes the re-simulation match the other peer.

| Problem | Piece |
|---|---|
| Save whole-world states to restore on a misprediction | `rollback::StateHistory` |
| Know one player's inputs, predict the frames you do not have | `rollback::InputTimeline` |
| Run the whole predict / detect / rollback / re-simulate loop | `rollback::RollbackSession` |

`RollbackSession` is the drop-in bundle here, the rollback counterpart to `PredictedPlayer`: you supply a deterministic `fn(&State, &[Input]) -> State` and it does the rest. The [`rollback_playground`](../examples/rollback_playground/) example runs two peers over a simulated wire and shows they stay identical.

## Building a netcode client

The recommended shape, in order:

```rust,ignore
// Your controlled entity.
let mut me = PredictedPlayer::new(start, PlayerConfig::default(), apply_move, lerp_pos);
// One per remote entity, plus a clock for the shared render target.
let mut remotes: HashMap<Id, RemoteView<State, Velocity>> = HashMap::new();
let mut clock = InterpolationClock::new(interpolation_delay_ms);

// On local input: predict now, send the numbered input.
let seq = me.input(mv);
send(SequencedClientInput { sequence_number: seq, input_data: mv });

// On a packet: reconcile yourself, push remotes, advance the clock.
me.reconcile(update.authoritative_player_state, update.last_processed_input_seq);
clock.observe(snapshot.server_time);
remotes.entry(id).or_insert_with(|| RemoteView::new(12, 500)).push(snapshot.server_time, state, velocity);

// Each frame: advance and draw.
me.advance(frame_dt_secs);
draw(&me.render());
for (_, view) in &remotes {
  if let Some(s) = view.render(clock.target(), RenderOpts::default()) { draw(&s); }
}
```

`apply_move` is the shared simulation step, the same rule the server applies; `lerp_pos` blends two states for correction smoothing. See [`netcode_playground`](../examples/netcode_playground/) for a complete, running version.

## Which clock drives what

Every piece here takes a `dt` or timestamps and is deliberately clock-agnostic: *you* choose which clock feeds each one. That split matters, and it is how a local pause works.

- **Wall-clock time** drives anything about the network: `InterpolationClock`, `RttEstimator`, and the `ErrorSmoother`'s ease. Network delay is real and does not stop, so these keep running on real seconds.
- **Game time** drives the simulation: `apply` and prediction. To pause locally (a menu, a cutscene), feed `dt = 0` to the game step while still feeding real `dt` to interpolation and RTT.

In an authoritative multiplayer game you cannot truly pause the shared world, the server keeps ticking, so a "pause" is a local overlay over a world that moves on. The clock-agnostic design is what lets you build that: freeze the simulation without freezing the netcode.

## The prediction loop

```rust,ignore
// Act now; record for replay.
predicted.apply_local_input_and_predict(&op, seq, &mut inputs, apply_move);
send_to_server(SequencedClientInput { sequence_number: seq, input_data: op });
seq += 1;

// The server replies: snap to truth, replay what it had not yet seen.
predicted.reconcile_with_server_state(
  update.authoritative_player_state,
  update.last_processed_input_seq,
  &mut inputs,
  apply_move,
);
```

`apply_move` is the shared simulation step, the same rule the server applies. Both sides must agree, or prediction fights the server every frame. A misprediction corrects itself on the next reconciliation; that is the design, not a failure.

## Smoothing remote entities

Remote players arrive as discrete snapshots. Render slightly in the past and interpolate between the two that bracket that moment:

```rust,ignore
remote.add_snapshot(snapshot.server_time, snapshot.into_state());
let render_time = estimated_server_time.saturating_sub(INTERPOLATION_DELAY);
if let Some(state) = remote.get_interpolated_state(render_time) {
  draw(state);
}
```

Pick a delay slightly larger than your typical gap between snapshots, one to two server ticks. Too small and the buffer runs dry; too large and remote entities visibly lag.

## Interpolating at a low send rate

A straight line between two snapshots is right when they are close together and wrong when they are not. At 60 snapshots a second the error across a 16 ms chord is invisible. At 10, the chord flattens 100 ms of a curved path into a line and the entity visibly corners: it slides to each sample, changes direction, slides to the next.

`HermiteView` is the answer Fiedler reaches for in [snapshot interpolation](https://gafferongames.com/post/snapshot_interpolation/): a cubic spline through both samples that also leaves along the velocity recorded at each, so the seams stop being corners. On a 10-unit circle sampled at 10 Hz and drawn at 60, worst error over a second is **0.0003 against linear's 0.1231**; expect less on a path that turns sharply, since a cubic is near-exact on a smooth one given true derivatives.

It is a separate type rather than a flag on `RemoteView` for a concrete reason: a spline needs the velocity at **both** ends, and `RemoteView` keeps one, for dead reckoning past the newest sample. Worth the second velocity on the wire below roughly 20 snapshots a second, and not much above it.

## Acknowledgement and second-order dead reckoning

Two later additions, both with a measured regime narrower than they first appear. Both are documented at length in [API_REFERENCE.md](API_REFERENCE.md); the summary of what measurement said:

**`ack::AckWindow`** is a sequence number plus a bitmask of the 64 before it: one fixed-size record of exactly what arrived, so a sender can resend only the gaps. Fixed size is the point, a link losing half its packets reports in the same twelve bytes as a perfect one. Measured in [`rollback_playground`](../examples/rollback_playground/), swapping blind input redundancy for ack-driven redundancy cut bandwidth 28% on a clean link and *raised* it 45% at 50% loss, crossing over around 12%. The finding worth carrying is the one that was not obvious: blind redundancy makes a fixed number of attempts and then gives up, while ack-driven retries until acknowledged, so at 55% loss it converged where blind did not. The two policies are bounded effort against bounded outcome, not cheap against expensive. Its own limit is the history window, not the attempt count.

**`trajectory::TrajectoryPredictor`** fits a damped quadratic through the last three samples of one scalar, so a turning entity coasted through a gap follows its curve instead of leaving on the tangent. Scalar deliberately, matching `ScalarKalman`: run one per axis rather than forcing a vector-space bound on every consumer. In isolation it cuts the error over a 100 ms gap on a circular path by 45%. In [`netcode_playground`](../examples/netcode_playground/) it does **nothing at all** at a normal server rate, and that negative result is the more useful half: the correction goes as the gap squared, and an adaptive buffer keeps the render target within a few milliseconds of the newest snapshot, so there is no gap to improve. It starts paying below about 10 Hz (7% at 5 Hz). Reach for it when your snapshot interval is long, not because it is the better algorithm.

## The client half of a delta-relevance stream

A server streaming interest-managed entities sends *entered* and *left* and lets each client keep a mirror. `plaza_server_utils::DeltaBaseline` owns the server's half; `mirror::DeltaMirror` is the half that has to agree with it, and the two are keyed by the same `SlotKey` and checked by the same `SetDigest`, which live here because a browser client needs them and must not inherit a server to get them.

`DeltaMirror` applies the packet, checks generations, counts sequence gaps, folds the digest and compares it. The rule it carries is the reason it is a type rather than a snippet: **apply every packet, whatever baseline it names.** The instinct is the opposite, and that instinct is right for a *relative* delta protocol. These deltas carry absolute values, so applying them is idempotent and applying a superset is harmless, while discarding what you cannot rebase starves the mirror. Measured in `horde_playground`: a version that discarded emptied its mirror out at 25% loss while every agreement check read perfect, because the checks only ran over what had been applied.

Its three counters stay separate on purpose. Sequence gaps mean the wire lost something; stale references mean a message named an occupant this mirror no longer holds; digest divergences are the symptom no counter predicts. "Forty mismatches and zero frames lost" and "forty mismatches and forty frames lost" are different bugs.

## Fixed steps and periods

`timestep::FixedTimestep` turns however long the last frame took into whole fixed steps. Three things it owns that six hand-written copies in this repo each got differently:

- **The clamp.** A backgrounded tab, a resumed laptop or a debugger breakpoint returns an enormous delta; uncapped, the loop pays for all of it in one frame, which takes longer than a frame, which makes the next delta larger. Default cap is 250 ms.
- **The step is yielded, not assumed.** `advance` returns an iterator of the step *duration*, so a caller cannot accidentally integrate by the frame delta instead. That is not hypothetical: a client integrating by frame delta against a fixed-step server drifts continuously and reads exactly like network jitter. Same rule is not enough; same timestep is required.
- **Time refused is counted.** `dropped_ms` is real time the simulation never ran, and a world quietly behind wall time explains a whole class of "it desynced and I do not know when".

`Periodic` is the same accumulator with a different consumption rule, and the split is deliberate. A fixed step asks "how much simulation does this frame pay for", where every step must run. A period asks "is it time yet", where the work is usually idempotent and running it twice in one frame is waste. So `due` fires at most once per advance and `advance` reports every occurrence.

## Math types

`Vec2`, `Vec3`, and `Quat` ship so the crate is usable standalone. If you already have `glam` or `nalgebra`, implement `Interpolatable` and `Extrapolatable` for your own types instead: every buffer here is generic over them.

## Status

Experimental. The API changes.

## What the counters are for

Four places in this crate degrade rather than fail, and each one counts how often it did. A `warn!` fires beside them, and the log line is not the useful half: it says a thing happened once, into a stream nobody aggregates, at the moment the game is too busy to read it. The number says whether it is climbing, which is the only version of the question worth asking.

- **`ClientInputBuffer::overflowed`**: inputs dropped because the buffer was full. Past the first one a reconciliation can no longer replay everything the server has not acknowledged, so the prediction is not merely late, it is wrong by whatever those inputs did.
- **`StateHistory::resets`**: saves that fell outside the window. Rollback assumes contiguous frames, so this should stay zero forever; non-zero means the window was rebuilt from a single frame and the next correction finds nothing to restore.
- **`ExtrapolationBase::over_extrapolations`** and **`RemoteView::over_extrapolations`**: projections held at the cap. Not an error, and the steady case is the diagnosis: it means the render target is being computed ahead of the newest sample rather than trailing it, so remote entities are dead reckoned every frame and never interpolated. The fix is the render clock, not the cap.

They are cheap enough to read every frame, which is the intended use: put them on a debug overlay next to the round trip. `plaza_flame::PlazaStats` exists for exactly that.
