# `plaza_client_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The client half of real-time networking: making a server's authoritative updates feel immediate and smooth. Client-side prediction, server reconciliation, interpolation, and extrapolation. Plus the other netcode family, peer-to-peer deterministic [rollback](#rollback-netcode).

The server half lives in [`plaza`](../core/) under `game_common::reconciliation`. Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_client_utils = "0.1"
```

**No workspace dependencies.** This crate pulls in `thiserror` and `tracing` and nothing else, deliberately, so wasm builds and game-engine plugins do not drag in a server's async runtime. It is pure logic: no transport, no serialization, no engine coupling. You feed it what you receive and read back what to render.

You do not need a Plaza server to use it. Anything speaking a sequence-numbered-input protocol works.

## Two ways in

**The drop-in bundles.** Most clients wire the primitives the same way, so two types package the whole job:

- `PredictedPlayer` is your controlled entity: feed it inputs and server packets, read a render position back. Predict, reconcile, and smooth, wired.
- `RemoteView` is an entity you do not control: `push` snapshots, `render` a state. Interpolation, extrapolation, and the starvation handling in between, wired.

**Or the primitives underneath**, if you want finer control. The bundles are built from them and nothing more:

| Problem | Piece |
|---|---|
| Local input should feel instant, but the server decides | `PredictedEntity` + `ClientInputBuffer` |
| Other players' updates arrive discretely and jittery | `SnapshotBuffer` (+ `InterpolationClock` for the render target) |
| Updates stop arriving for a moment | `ExtrapolationBase` |
| A correction snaps the local entity to a new spot | `ErrorSmoother` |
| The render clock drifts as latency changes | `InterpolationClock::resync` (position) or `observe_rate` (playback-rate glide) |
| Measuring round-trip latency to the other end | `RttEstimator` (with `plaza_wire`'s `Ping`/`Pong`) |
| Tracking clock offset **and drift** against a server | `ClockSyncEstimator` (least-squares offset + skew) |
| Optimally smoothing one noisy signal (jitter, latency) | `ScalarKalman` (a 1D Kalman filter) |
| Telling the other side what arrived, in twelve bytes, however bad the link | `ack::AckWindow` (a sliding-window bitmask) |
| Coasting a *turning* entity through a long gap, not off its tangent | `trajectory::TrajectoryPredictor` (a damped quadratic fit) |

Each is usable on its own. The [`netcode_playground`](../examples/netcode_playground/) example wires the whole picture together interactively, through the bundles.

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

## Acknowledgement and second-order dead reckoning

Two later additions, both with a measured regime narrower than they first appear. Both are documented at length in [API_REFERENCE.md](API_REFERENCE.md); the summary of what measurement said:

**`ack::AckWindow`** is a sequence number plus a bitmask of the 64 before it: one fixed-size record of exactly what arrived, so a sender can resend only the gaps. Fixed size is the point, a link losing half its packets reports in the same twelve bytes as a perfect one. Measured in [`rollback_playground`](../examples/rollback_playground/), swapping blind input redundancy for ack-driven redundancy cut bandwidth 28% on a clean link and *raised* it 45% at 50% loss, crossing over around 12%. The finding worth carrying is the one that was not obvious: blind redundancy makes a fixed number of attempts and then gives up, while ack-driven retries until acknowledged, so at 55% loss it converged where blind did not. The two policies are bounded effort against bounded outcome, not cheap against expensive. Its own limit is the history window, not the attempt count.

**`trajectory::TrajectoryPredictor`** fits a damped quadratic through the last three samples of one scalar, so a turning entity coasted through a gap follows its curve instead of leaving on the tangent. Scalar deliberately, matching `ScalarKalman`: run one per axis rather than forcing a vector-space bound on every consumer. In isolation it cuts the error over a 100 ms gap on a circular path by 45%. In [`netcode_playground`](../examples/netcode_playground/) it does **nothing at all** at a normal server rate, and that negative result is the more useful half: the correction goes as the gap squared, and an adaptive buffer keeps the render target within a few milliseconds of the newest snapshot, so there is no gap to improve. It starts paying below about 10 Hz (7% at 5 Hz). Reach for it when your snapshot interval is long, not because it is the better algorithm.

## Math types

`Vec2`, `Vec3`, and `Quat` ship so the crate is usable standalone. If you already have `glam` or `nalgebra`, implement `Interpolatable` and `Extrapolatable` for your own types instead: every buffer here is generic over them.

## Status

Experimental. The API changes.
