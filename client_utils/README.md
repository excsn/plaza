# `plaza_client_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The client half of real-time networking: making a server's authoritative updates feel immediate and smooth. Client-side prediction (for either server input model), server reconciliation, interpolation, extrapolation, and the mirror that holds a streamed entity set and can prove it still agrees with the server. Plus the other netcode family, peer-to-peer deterministic [rollback](#rollback-netcode).

The server half lives in [`plaza`](../core/) under `game_common::reconciliation`. How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

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

## Rollback netcode

The pieces above serve the *server-authoritative* model. The `rollback` module serves the other family, *peer-to-peer deterministic lockstep*: there is no server, peers run the **same deterministic simulation** and exchange only inputs. A peer cannot wait for a remote input and stay responsive, so it **predicts** the missing one (repeat the last), simulates ahead, and **rolls back** to re-simulate when the real input arrives and disagrees. Determinism is what makes the re-simulation match the other peer.

| Problem | Piece |
|---|---|
| Save whole-world states to restore on a misprediction | `rollback::StateHistory` |
| Know one player's inputs, predict the frames you do not have | `rollback::InputTimeline` |
| Run the whole predict / detect / rollback / re-simulate loop | `rollback::RollbackSession` |

`RollbackSession` is the drop-in bundle here, the rollback counterpart to `PredictedPlayer`: you supply a deterministic `fn(&State, &[Input]) -> State` and it does the rest. The [`rollback_playground`](../examples/rollback_playground/) example runs two peers over a simulated wire and shows they stay identical.

## Status

Experimental. The API changes.
