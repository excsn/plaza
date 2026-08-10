# 90. The parts bin

Every block, one line each, sorted by the itch it scratches. Links go to the crate that owns the block; its README argues the design and its API_REFERENCE has the full surface. Chapters in parentheses tell you where the story is.

## The loop and who is in it (chapters 01, 12)

| Block | Reach for this when | Lives in |
|---|---|---|
| `StateController` / `StateControllerBuilder` | several agents act on one shared state, and you want no locks | [core](../../core/API_REFERENCE.md) |
| `StateLogic`, `LogicInput`/`LogicOutput` | writing the rules; the only place state changes | [core](../../core/API_REFERENCE.md) |
| `Agent`, `AgentId` | naming who acted (human, bot, system), identically on wasm | [core](../../core/API_REFERENCE.md) |
| `TickDriver` | feeding time to the loop; `run_fixed` whenever anything predicts or replays | [core](../../core/API_REFERENCE.md) |
| `InProcessSession` | the whole loop with no sockets: tests, demos, local play | [core](../../core/API_REFERENCE.md) |
| `query_with` / `ControllerStats` | asking a running controller a question / watching its health without touching its queue | [core](../../core/API_REFERENCE.md) |
| `ReconnectTracker` | disconnect grace, driven from your tick, meaning of expiry stays yours | [core](../../core/API_REFERENCE.md) |
| `SeatTable` / `Seating` | bounded seats where a fresh occupant must not inherit the last one's state | [server_utils](../../server_utils/API_REFERENCE.md) |

## Showing the world (chapters 10, 11)

| Block | Reach for this when | Lives in |
|---|---|---|
| `SnapshotProvider` / `SnapshotRequest` | what a joiner or a refresh is sent; per-recipient secrecy or the uniform fast path | [core](../../core/API_REFERENCE.md) |
| `morton`, `GridQuantizer`, `SpatialGrid` | gathering nearby ids without scanning the world | [server_utils](../../server_utils/API_REFERENCE.md) |
| `VisibilitySet` | a per-client visible set with a fast entered/left diff for spawn and despawn streams | [server_utils](../../server_utils/API_REFERENCE.md) |
| `TierBoundary` | any wire-affecting threshold that would flap on an edge-loiterer | [server_utils](../../server_utils/API_REFERENCE.md) |
| `AggregateTree` | distant entities the client computes with, not just draws | [server_utils](../../server_utils/API_REFERENCE.md) |
| `DeltaBaseline` / `DeltaPlan` | streaming a changing set over a lossy link, diffed against what was acknowledged | [server_utils](../../server_utils/API_REFERENCE.md) |
| `DeltaMirror`, `SetDigest`, `SlotKey`, `SlotAllocator` | the client half of that stream, and the proof both ends still agree | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PriorityAccumulator` | relevance says who; this says which of them fit *this* packet, without starving the rest | [server_utils](../../server_utils/API_REFERENCE.md) |
| `RestDetector` | knowing which entities have stopped, so a packet can stop paying for them | [server_utils](../../server_utils/API_REFERENCE.md) |
| `RateMeter` | live rates, means, and shares on a HUD instead of claims in a README | [server_utils](../../server_utils/API_REFERENCE.md) |

## Your own character (chapter 20)

| Block | Reach for this when | Lives in |
|---|---|---|
| `PredictedPlayer` | the wired bundle for a server that consumes one input per step | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HeldInputPredictor` | the wired bundle for a server that integrates held inputs | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PredictedEntity` + `ClientInputBuffer` | the primitives under both, for wiring your own | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ErrorSmoother` / `CorrectionMonitor` | easing what you draw after a correction / knowing whether that correction was normal | [client_utils](../../client_utils/API_REFERENCE.md) |
| `AdaptiveDecay` | clearing a large correction *sooner* than a small one, rather than in the same fixed time | [client_utils](../../client_utils/API_REFERENCE.md) |
| `InputCoalescer` | send-on-change plus keepalive, paired with held-input servers only | [client_utils](../../client_utils/API_REFERENCE.md) |
| reconciliation module (server half) | tracking which inputs each client has been credited for | [core](../../core/API_REFERENCE.md) |

## Everyone else, and fairness (chapter 21)

| Block | Reach for this when | Lives in |
|---|---|---|
| `RemoteView` | an entity you do not control: push snapshots, ask for a render state | [client_utils](../../client_utils/API_REFERENCE.md) |
| `SnapshotBuffer` + `InterpolationClock` | rendering remotes a declared beat in the past | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HermiteView` | a send rate low enough that a straight line between samples visibly corners | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ExtrapolationBase` / `TrajectoryPredictor` | coasting through a gap, capped / the sub-10Hz special case | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ArrivalMonitor` | measuring what render delay your stream actually needs | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HistoricalStateBuffer` | judging a shot at the time the shooter saw, not now | [server_utils](../../server_utils/API_REFERENCE.md) |
| `render_error_at` | how wrong a client's screen was, asked at the instant it drew | [server_utils](../../server_utils/API_REFERENCE.md) |
| `InputSchedule` / `InputWindow` | executing inputs on the tick the client named, rejecting backdates | [server_utils](../../server_utils/API_REFERENCE.md) |
| `StateHistory`, `InputTimeline`, `RollbackSession` | peer-to-peer deterministic rollback, no server at all | [client_utils](../../client_utils/API_REFERENCE.md) |

## Clocks and time (chapters 20, 31)

| Block | Reach for this when | Lives in |
|---|---|---|
| `FixedTimestep` / `Periodic` | a variable frame driving a fixed-quantum sim / "is it time yet" | [client_utils](../../client_utils/API_REFERENCE.md) |
| `RttEstimator` | smoothed round trip, jitter, and minimum from the probe plane | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ClockSyncEstimator` | server-clock offset and drift rate by least squares, for long sessions | [client_utils](../../client_utils/API_REFERENCE.md) |
| `Timeline` / `Probe` | keeping probe samples honest across reconnects and tab resumes | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ScalarKalman` | optimally smoothing one noisy scalar | [client_utils](../../client_utils/API_REFERENCE.md) |

## The wire (chapter 30)

| Block | Reach for this when | Lives in |
|---|---|---|
| `frame` (kinds, split, begin) | the `[kind][body]` layout and the skip-unknown rule | [wire](../../wire/API_REFERENCE.md) |
| `JsonCodec` / `MsgPackCodec` / `MsgPackNamedCodec` / `WireCodec` | readable by default, compact when measured to pay, named when the other end cannot be built from your struct definitions, yours when none fits | [wire](../../wire/API_REFERENCE.md) and [session](../../session/API_REFERENCE.md) |
| `build::emit` / `ProtocolVersion` | a wire version nobody has to remember to bump | [wire](../../wire/API_REFERENCE.md) |
| `answer_ping` | answering probes from a hand-written read loop | [wire](../../wire/API_REFERENCE.md) |
| `AckWindow` | telling the other side what arrived, in twelve bytes | [client_utils](../../client_utils/API_REFERENCE.md) |
| `bits` (`BitWriter`/`BitReader`, `quantize`, `smallest_three`, varints) | the hot array, where a byte-aligned codec cannot express a bound and a bound is the whole saving | [wire](../../wire/API_REFERENCE.md) |
| `BitCodec` | the same idea with no layout written by hand: worth 1.4x, and the ceiling of what a derive can reach | [wire](../../wire/API_REFERENCE.md) |
| `Payload` | carrying packed bytes in a field, without a codec re-encoding every byte as an integer | [wire](../../wire/API_REFERENCE.md) |
| payload types (`SequencedClientInput` and friends) | the shared netcode vocabulary, generic over your types | [wire](../../wire/API_REFERENCE.md) |

## Sockets and sessions (chapters 31, 32, 33)

| Block | Reach for this when | Lives in |
|---|---|---|
| `ActixWsPlazaSession` / `TcpPlazaSession` | the shipped transports | [session](../../session/API_REFERENCE.md) |
| `ConnectionManager` | the registry every transport stands on: register, forward, resolve, measure, close | [session](../../session/API_REFERENCE.md) |
| `LinkDriver`, `Conditioner`, `ProbeState` | the connection loop's parts, assembled or piecemeal, for transports of your own | [session](../../session/API_REFERENCE.md) |
| `LinkProfile` / `DirectionProfile` | impairment: delay, jitter, loss, per connection, per direction, at runtime | [session](../../session/API_REFERENCE.md) |
| `Host` | serving the browser bundle with the cache-busting that makes stale clients a reload | [session](../../session/API_REFERENCE.md) |
| `SessionOptions`, `Workload` | queue depths, limits, and overflow policy, sized from a description of your traffic | [session](../../session/API_REFERENCE.md) |
| `Socket` trait, `loopback::pair`, `trim_backlog` | one client socket shape across desktop, wasm, and in-process; resumed-tab backlog | [ws_client](../../ws_client/) |
| `TransportStats` | what the transport carried and dropped, readable while it is busy | [session](../../session/API_REFERENCE.md) |

## Saying no (chapter 40)

| Block | Reach for this when | Lives in |
|---|---|---|
| fallible `AgentFactory` / `Refusal` | turning a socket away before anything is registered for it | [session](../../session/API_REFERENCE.md) |
| `connections_of` + `PresenceEvent`'s conn id | resolving "this account must go" to a closable handle | [session](../../session/API_REFERENCE.md) |
| `close_connection` / `deregister_agent` / `disconnect_all` | ending sessions with the reason arriving first, one, all-of-one, or everyone | [session](../../session/API_REFERENCE.md) |
| `idle_for` / `agent_idle_for` | AFK rules that probe traffic cannot postpone | [session](../../session/API_REFERENCE.md) |
| `connection_inbound` / `agent_inbound` | attributing a flood to the connection sending it | [session](../../session/API_REFERENCE.md) |
| `set_deadline` | credits, trials, and token expiry as one renewable mechanism | [session](../../session/API_REFERENCE.md) |

## Rooms and placement (chapter 41)

| Block | Reach for this when | Lives in |
|---|---|---|
| `RoomFactory` / `RoomHandle` | rooms spawned on demand behind a seam that names none of their types | [lobby](../../lobby/API_REFERENCE.md) |
| `InMemoryLobbyManager` | the assembled directory: create, list, join, reap | [lobby](../../lobby/API_REFERENCE.md) |
| `MatchQueue` | quick match with patience, driven from your tick | [lobby](../../lobby/API_REFERENCE.md) |
| `SeatReservations` | promises that survive the socket closing behind a room hop, and lapse if nobody dials | [lobby](../../lobby/API_REFERENCE.md) |
| `TicketStore` | placement handed to a client to present elsewhere (placement, not authentication), as `MapTicketRegistry` or `CachedTicketRegistry` | [lobby](../../lobby/API_REFERENCE.md) |

## Testing and odds and ends

| Block | Reach for this when | Lives in |
|---|---|---|
| `LatencyLink`, `Rng` (feature `net-sim`) | deterministic latency, jitter, and loss for tests, faithful to a stream's physics | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PlayoutBuffer` / `Admission` | a playout queue that knows when a resumed tab's timeline is lost | [client_utils](../../client_utils/API_REFERENCE.md) |
| `Vec2` / `Vec3` / `Quat` and the `Interpolatable` trait | standalone math, or implement the traits on glam and keep your own | [client_utils](../../client_utils/API_REFERENCE.md) |
| schedulers, fsm, flow control, scorekeeping | optional core modules, take what fits, each a trait with a swappable impl | [core](../../core/API_REFERENCE.md) |
