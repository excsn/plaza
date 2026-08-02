# INDEX, where the code lives

Companion to [README.md](README.md) (what Plaza is). This file answers "where do I look" and "where do I add code".

## Documents

| File | What it is for |
|---|---|
| [README.md](README.md) | What Plaza is, and the crate map. |
| [examples/LEARNINGS.md](examples/LEARNINGS.md) | What the playgrounds taught: the principles, the bug catalogue, the diagnostic playbook. |

Each crate also carries a `README.md` (how to use it) and an `API_REFERENCE.md` (its full public surface).

## Crates

| Crate | Path | Role |
|---|---|---|
| `plaza` | [core/](core/) | The controller loop, the four traits an app implements, and optional building blocks. Depends on no other workspace crate. |
| `plaza_session` | [session/](session/) | Real transports. One shared connection manager, thin per-protocol adapters, plus the optional listen-server HTTP layer (`actix_host`). |
| `plaza_lobby` | [lobby/](lobby/) | Rooms on a single server: spawn, list, join, reap. Talks to rooms only through core's `ControllerCommand`. |
| `plaza_client_utils` | [client_utils/](client_utils/) | Client-side prediction and smoothing. **No workspace dependencies**: deliberately, so wasm and engine-plugin builds stay free of the server's async runtime. |
| `plaza_server_utils` | [server_utils/](server_utils/) | Relevance streaming, delta baselines, lag-compensation rewind, aggregation, seats. Runtime-free and wasm-safe like the client crate, whose interpolation, digest and slot-key types it shares. Its only workspace dependency is that crate. |
| `plaza_wire` | [wire/](wire/) | The wire vocabulary shared by a server and its clients, runtime-free: the message envelope, the `WireCodec` trait, the common netcode payload types, and the build-time protocol version hash. |
| `plaza_ws` | [ws_client/](ws_client/) | One client-side WebSocket interface across desktop, browser and in-process. The counterpart to `plaza_session`, which is server-only by construction. |


Examples live in [examples/](examples/), one crate each, in their own workspace (their pinned renderers and wasm targets stay out of the publishable graph; `./check.sh` verifies both workspaces, and the feature list inside it is load-bearing). Plus [examples/playground_common/](examples/playground_common/): the four listen-server roles and their argument parsing, shared by the two playgrounds. Not part of the library, because argument parsing is an opinion every real application already has; it is a separate crate only because a wasm client needs the same vocabulary and must not inherit an HTTP server to get it.

## The loop (start here)

| Concern | Entry |
|---|---|
| The actor that owns state; `ControllerCommand`, the builder, `query_state` | [core/src/controller.rs](core/src/controller.rs) |
| What an app implements to change state: `StateLogic`, `LogicInput` | [core/src/state_logic.rs](core/src/state_logic.rs) |
| The transport trait, `MessageTarget`, `TargetedOp`, `SessionMessage` | [core/src/session/mod.rs](core/src/session/mod.rs) |
| Loopback transport for tests and local play | [core/src/session/in_process.rs](core/src/session/in_process.rs) |
| What a client is sent, per recipient; `SnapshotProvider`, `SnapshotContext` | [core/src/snapshot.rs](core/src/snapshot.rs) |
| Fixed-rate and virtual time stepping | [core/src/tick_driver.rs](core/src/tick_driver.rs) |
| `Agent`, and the `AgentId` blanket trait | [core/src/agent.rs](core/src/agent.rs) |
| Live counters the controller writes and anyone reads: tick duration, queue depth, ops | [core/src/stats.rs](core/src/stats.rs) |
| Error hierarchy | [core/src/error.rs](core/src/error.rs) |

**Reading order for a newcomer:** `state_logic.rs` → `controller.rs` → `session/mod.rs`. Everything else is optional.

## Core building blocks (`core/src`)

### `common/`: reusable infrastructure

| What | Entry |
|---|---|
| Event schedulers, generic over a tick (`u64`) or game-time (`Duration`) axis | [common/scheduler/event_scheduler.rs](core/src/common/scheduler/event_scheduler.rs) |
| Callback schedulers, same two axes | [common/scheduler/callback_scheduler.rs](core/src/common/scheduler/callback_scheduler.rs) |
| `SchedulerInstant`, `ScheduledEventId`, and the four public aliases | [common/scheduler/mod.rs](core/src/common/scheduler/mod.rs) |
| Finite state machines | [common/fsm.rs](core/src/common/fsm.rs) |
| Participant registry | [common/participants.rs](core/src/common/participants.rs) |
| Disconnect grace periods for reconnection | [common/reconnect.rs](core/src/common/reconnect.rs) |
| `Vec2`/`Vec3`/`Quat` for op payloads | [common/math.rs](core/src/common/math.rs) |

### `game_common/`: game patterns

| What | Entry |
|---|---|
| Last-processed input sequence per client | [reconciliation/client_input_tracker.rs](core/src/game_common/reconciliation/client_input_tracker.rs) |
| Fixed-delay input buffering for fairness | [reconciliation/delayed_input_processing.rs](core/src/game_common/reconciliation/delayed_input_processing.rs) |
| Rewind buffer for lag compensation; `Interpolatable` | [server_utils/src/history.rs](server_utils/src/history.rs) |
| Wire shapes for prediction: `SequencedClientInput`, `AuthoritativeStateUpdate`, `RemoteEntitySnapshot` | [reconciliation/op_payloads.rs](core/src/game_common/reconciliation/op_payloads.rs) |
| Turn and round traits, plus `RoundRobinTurnManager`, `SequentialRoundManager`, and `Phased` for holding a phase. Worked example: [card_table](examples/card_table/) | [flow_control/](core/src/game_common/flow_control/) |
| `Scorekeeper` trait and `HashMapScorekeeper` | [scorekeeping/](core/src/game_common/scorekeeping/) |
| `PlayerIntent` | [input_intent.rs](core/src/game_common/input_intent.rs) |

### `app_common/`: collaboration payloads

Op shapes for non-game apps: [locking/](core/src/app_common/locking/), [presence/](core/src/app_common/presence/), [ordered_collection_ops/](core/src/app_common/ordered_collection_ops/), [object_property_ops/](core/src/app_common/object_property_ops/). These are payload definitions, not engines, the application still writes the logic.

## `plaza_session`

Both transports share everything that is not socket I/O, which is why the adapters are thin.

| What | Entry |
|---|---|
| Connection registry, message targeting, serialization, the deserialize bridge (two-stage dispatch on the frame kind), and the `Session` impl both transports delegate to | [session/src/manager.rs](session/src/manager.rs) |
| The protocol handshake: `with_protocol` declares a version, every new connection is sent a `Hello`, and a peer's declaration is recorded per agent | [session/src/manager.rs](session/src/manager.rs) |
| Pluggable wire format; re-exports `WireCodec` and `JsonCodec` from [`plaza_wire`](#plaza_wire) | [session/src/codec.rs](session/src/codec.rs) |
| actix-web WebSocket adapter: `handle_connection` is the whole integration | [session/src/actix_ws.rs](session/src/actix_ws.rs) |
| Length-delimited TCP adapter | [session/src/tcp.rs](session/src/tcp.rs) |
| The listen-server HTTP layer (feature `actix_host`): `Host`, the stamped index, no-cache assets, the preflight, `lan_address`, `init_logging` | [session/src/host/server.rs](session/src/host/server.rs) |
| What routing a frame to a target costs, the agent index against the registry pass it replaced | [session/benches/broadcast.rs](session/benches/broadcast.rs) |
| What the transport carried and what it dropped rather than stalling for | [session/src/stats.rs](session/src/stats.rs) |
| Transport errors | [session/src/error.rs](session/src/error.rs) |

**To add a transport:** write a socket pump that calls `ConnectionManager::{register, forward_incoming, deregister}` and delegates the `Session` trait to a `TransportSession`. Build the per-client queue with `plaza::session::session_channel`, so the transport never names the channel crate. `tcp.rs` is the smaller of the two to copy.

## `plaza_wire`

| What | Entry |
|---|---|
| `WireCodec` trait (with `is_text` and `encode_into`), `JsonCodec`, and `MsgPackCodec` (feature `msgpack`) | [wire/src/lib.rs](wire/src/lib.rs) |
| Identity on the wire: `Agent`, `AgentId`. Here rather than in core because a wasm client cannot depend on core | [wire/src/envelope.rs](wire/src/envelope.rs) |
| Framing: the kind byte in front of every message, the skip-unknown rule that lets a frame kind be added later, and `ProtocolVersion` for the `Hello` handshake | [wire/src/frame.rs](wire/src/frame.rs) |
| What the codecs and the framing actually cost, with an allocation-counting allocator | [wire/benches/wire.rs](wire/benches/wire.rs) |
| Shared netcode payload vocabulary (`SequencedClientInput`, `AuthoritativeStateUpdate`, `RemoteEntitySnapshot`, `TimestampedClientAction`) | [wire/src/payloads.rs](wire/src/payloads.rs) |
| Build-time protocol version: hash the sources that define your messages, emit a `u32` (feature `build`, used from a `build.rs`) | [wire/src/build.rs](wire/src/build.rs) |

Split out from `plaza_session` so a client can share the server's encoding without inheriting its async runtime: pure serde, no tokio, no actix. Server code sees these through the `plaza_session` re-export and rarely names this crate directly.

## `plaza_lobby`

| What | Entry |
|---|---|
| Create / join / list / reap, and password verification | [lobby/src/manager.rs](lobby/src/manager.rs) |
| `RoomFactory`: what an app implements per game type | [lobby/src/factory.rs](lobby/src/factory.rs) |
| `RoomHandle` and the in-process implementation | [lobby/src/room.rs](lobby/src/room.rs) |
| Request/notice payload shapes | [lobby/src/op_payloads.rs](lobby/src/op_payloads.rs) |

## `plaza_client_utils`

| What | Entry |
|---|---|
| **Drop-in local player, one input per step** (predict + reconcile + smooth, bundled) | [predicted_player.rs](client_utils/src/predicted_player.rs) |
| **Drop-in local player, held input** (dead reckon + ease, for a server that integrates a held direction) | [held_input.rs](client_utils/src/held_input.rs) |
| **Drop-in remote entity** (interpolate + extrapolate + hold, bundled) | [remote_view.rs](client_utils/src/remote_view.rs) |
| **Rollback netcode** (deterministic lockstep: save-state history, input prediction, the `RollbackSession` bundle) | [rollback.rs](client_utils/src/rollback.rs) |
| Predict locally, reconcile against the server | [prediction.rs](client_utils/src/prediction.rs) |
| Unacknowledged input replay buffer | [input_buffer.rs](client_utils/src/input_buffer.rs) |
| What a reconciliation did, and an adaptive test for what counts as abnormal | [correction.rs](client_utils/src/correction.rs) |
| Snapshot buffering and interpolation for remote entities, plus the render clock | [interpolation.rs](client_utils/src/interpolation.rs) |
| Extrapolation when snapshots run out | [extrapolation.rs](client_utils/src/extrapolation.rs) |
| Easing a reconciliation correction over a few frames | [smoothing.rs](client_utils/src/smoothing.rs) |
| Fixed-size simulation steps out of a variable frame, and "is it time yet" beside it | [timestep.rs](client_utils/src/timestep.rs) |
| The client's mirror of a streamed entity set: apply, check generations, fold the digest, compare | [mirror.rs](client_utils/src/mirror.rs) |
| `(index, generation)` handles and the allocator that recycles them | [slot.rs](client_utils/src/slot.rs) |
| An order-independent digest of a set of `u64` keys, maintainable in O(1) | [digest.rs](client_utils/src/digest.rs) |
| Send an input only when it changes, plus the keepalive that makes that safe | [coalesce.rs](client_utils/src/coalesce.rs) |
| The playout queue: bounded, ordered by sequence, and the discontinuity verdict a resumed client acts on | [playout.rs](client_utils/src/playout.rs) |
| Measured arrival statistics and the render-delay budget they imply | [arrival.rs](client_utils/src/arrival.rs) |
| Client-side `Vec2`/`Vec3`/`Quat` with operators and slerp | [math.rs](client_utils/src/math.rs) |
| Delay, jitter and loss applied where the link is | [conditioner.rs](session/src/conditioner.rs) |
| Frames the session answers for itself (probes, their schedule) | [control.rs](session/src/control.rs) |
| Saying a one-shot op until the peer proves it heard | [oneshot.rs](examples/playground_common/src/oneshot.rs) |
| Round-trip latency estimation from probe samples | [rtt.rs](client_utils/src/rtt.rs) |
| A probe's epoch bookkeeping across reconnect and resume | [timeline.rs](client_utils/src/timeline.rs) |
| Sliding-window acknowledgement: a sequence number plus a 64-bit arrival mask, so a sender resends only the gaps (and `contiguous_base`, for a protocol that re-derives instead) | [ack.rs](client_utils/src/ack.rs) |
| Second-order dead reckoning: a damped quadratic through the last three samples of one scalar | [trajectory.rs](client_utils/src/trajectory.rs) |
| Clock offset **and skew** (drift) by least-squares regression | [clock_sync.rs](client_utils/src/clock_sync.rs) |
| Optimal 1D smoothing of a noisy signal (jitter, latency) | [filter.rs](client_utils/src/filter.rs) |
| Deterministic latency/jitter/loss network sim for tests, ordered by default (feature `net-sim`) | [net_sim.rs](client_utils/src/net_sim.rs) |

Note the intentional overlap with `core/src/common/math.rs`: keeping this crate dependency-free matters more than sharing sixty lines of POD types.

## `plaza_ws`

The client-side socket, with one interface over three backends. `plaza_session` is tokio/actix and server-only, so a client, and least of all a browser one, could not use it.

| What | Entry |
|---|---|
| The `Socket` trait, `Event`, `CloseReason`. Non-blocking `poll` into a caller-owned buffer, for frame loops with nowhere to put a future | [ws_client/src/lib.rs](ws_client/src/lib.rs) |
| In-process pair, so a host that plays runs the same client its joiners do | [ws_client/src/loopback.rs](ws_client/src/loopback.rs) |
| Desktop, `tungstenite` on a worker thread with a non-blocking stream | [ws_client/src/native.rs](ws_client/src/native.rs) |
| Browser under macroquad: `extern "C"` against our own JS plugin, no dependencies, because wasm-bindgen cannot coexist with miniquad's loader | [ws_client/src/miniquad.rs](ws_client/src/miniquad.rs), [ws_client/js/plaza_ws.js](ws_client/js/plaza_ws.js) |
| Dropping a resume backlog before parsing it, under the resume contract | [ws_client/src/backlog.rs](ws_client/src/backlog.rs) |
| Guards the silent-stub failure: parses a bundle's imports and fails if the plugin does not satisfy them | [ws_client/check_js_imports.py](ws_client/check_js_imports.py) |

## `plaza_server_utils`

The server-side counterpart, also runtime-free and wasm-safe. Shares `client_utils`'s `Interpolatable`/`ToF32` traits, so one state type feeds both a client's `SnapshotBuffer` and the server's rewind.

| What | Entry |
|---|---|
| Historical state rewind for lag compensation | [server_utils/src/history.rs](server_utils/src/history.rs) |
| Relevance / interest management: Morton keys, a spatial grid, a visibility-diff bitset, a hysteresis tier boundary | [server_utils/src/relevance.rs](server_utils/src/relevance.rs) |
| Per-subscriber delta bookkeeping: which packets landed, what to send and what to retract, the rebuild when a mirror has drifted, and the flow control that stops streaming to a reader that stopped reading | [server_utils/src/delta.rs](server_utils/src/delta.rs) |
| Tick-addressed input buffering: the accepting window, reject-not-correct, level and event semantics | [server_utils/src/input_schedule.rs](server_utils/src/input_schedule.rs) |
| Hierarchical aggregation: a Barnes-Hut tree that coarsens a distant crowd instead of culling it, for entities a client simulates rather than draws | [server_utils/src/aggregate.rs](server_utils/src/aggregate.rs) |
| A bounded number of seats, and a type that will not let you forget whether one is fresh | [server_utils/src/seats.rs](server_utils/src/seats.rs) |
| Running totals into rates, with the divide-by-zero guard every copy had to remember | [server_utils/src/meter.rs](server_utils/src/meter.rs) |

`SetDigest`, `SlotKey`/`SlotAllocator` and `DeltaMirror` are re-exported from `client_utils`, not defined here: both sides have to agree about them, and a browser client must not inherit a server to get them.

## Tests

| Where | Covers |
|---|---|
| [core/tests/in_process_pipeline.rs](core/tests/in_process_pipeline.rs) | The controller runtime end to end: join→snapshot, op→broadcast, tick driver, leave, shutdown. |
| [session/tests/tcp_roundtrip.rs](session/tests/tcp_roundtrip.rs) | A framed client's op reaching the controller, a broadcast reaching the client, and bind failure surfacing. |
| [lobby/tests/lobby_manager.rs](lobby/tests/lobby_manager.rs) | Room lifecycle, passwords, filters, reaping: including regression tests for the join deadlock and premature reaping. |
| `core/src/common/scheduler/*` | Scheduler semantics, inline. |
| `core/src/game_common/reconciliation/*` | Input tracking, delayed input, historical buffer, inline. |
| `client_utils/src/*` | Prediction, interpolation, extrapolation, smoothing, timestep, mirror, slots, digest, acknowledgement, math, inline. |
| `server_utils/src/*` | Rewind, relevance, aggregation, delta baselines, seats, rates, inline. |
| `examples/horde_playground/src/sim/*` | The many-entity case, inline: warm-arena joins, digest rebuilds, lossy corpse recovery, input windows, playout. Named after the historical failures rather than the functions. |
| `examples/blackhole_playground/src/sim/*` | The field case, inline: aggregation against both baselines, prediction of a forced entity. |


## Conventions

- `parking_lot` for `Mutex`/`RwLock`, never `std::sync` (workspace-wide excsn rule).
- All channels are `fibre`, not `tokio::sync`. Sync call sites use `try_send`; fibre's `send` is a future, so `let _ = tx.send(x);` silently does nothing.
- A session's two notification streams (inbound messages and presence), are single-consumer: they are *taken*, not subscribed to. Taking one twice panics.
- Joins and leaves share one ordered `PresenceEvent` stream on purpose. Splitting them lets a leave overtake a join under `select!`, which broke reconnection.
- Nothing in `core` spawns a task except `TickDriver` and the caller's own `controller.run()`.
