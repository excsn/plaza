# 90. The parts bin

Every block, one line each, grouped by the problem it solves. Links go to the crate that owns the block; its README explains the design and its API_REFERENCE documents the full API. Chapters in parentheses say where each block is explained.

## The loop and who is in it (chapters 01, 12)

| Block | Reach for this when | Lives in |
|---|---|---|
| `StateController` / `StateControllerBuilder` | several agents act on one shared state and you want no locks | [core](../../core/API_REFERENCE.md) |
| `StateLogic`, `LogicInput`/`LogicOutput` | writing the rules; the only place state changes | [core](../../core/API_REFERENCE.md) |
| `Agent`, `AgentId` | naming who acted (human, bot, system), identically on wasm | [core](../../core/API_REFERENCE.md) |
| `TickDriver` | feeding time to the loop; `run_fixed` whenever anything predicts or replays | [core](../../core/API_REFERENCE.md) |
| `InProcessSession` | the whole loop with no sockets: tests, demos, local play | [core](../../core/API_REFERENCE.md) |
| `query_with` / `ControllerStats` | asking a running controller a question / watching its health without touching its queue | [core](../../core/API_REFERENCE.md) |
| `ReconnectTracker` | disconnect grace, driven from your tick; what expiry means is up to you | [core](../../core/API_REFERENCE.md) |
| `SeatTable` / `Seating` | bounded seats where a fresh occupant must not inherit the last one's state | [server_utils](../../server_utils/API_REFERENCE.md) |
| `Roster` | the same, when a seat number is what your ops and your wire actually carry | [server_utils](../../server_utils/API_REFERENCE.md) |
| `Crew` | bots in the roster: real seats through the same admission as a person, with no connection | [server_utils](../../server_utils/API_REFERENCE.md) |
| `ClosureLog` | telling a close you ordered apart from a netdrop, since both arrive as the same `AgentLeft` | [core](../../core/API_REFERENCE.md) |

## Showing the world (chapters 10, 11)

| Block | Reach for this when | Lives in |
|---|---|---|
| `SnapshotProvider` / `SnapshotRequest` | what a joiner or a refresh is sent; per-recipient secrecy or the uniform fast path | [core](../../core/API_REFERENCE.md) |
| `morton`, `GridQuantizer`, `SpatialGrid` | gathering nearby ids without scanning the world | [server_utils](../../server_utils/API_REFERENCE.md) |
| `Subscriptions` / `Audience` | relevance a distance query cannot answer: a party, a guild, a watchlist, wherever its members are | [server_utils](../../server_utils/API_REFERENCE.md) |
| `VisibilitySet` | a per-client visible set with a fast entered/left diff for spawn and despawn streams | [server_utils](../../server_utils/API_REFERENCE.md) |
| `TierBoundary` | any wire-affecting threshold that would flap on an edge-loiterer | [server_utils](../../server_utils/API_REFERENCE.md) |
| `AggregateTree` | distant entities the client computes with, not just draws | [server_utils](../../server_utils/API_REFERENCE.md) |
| `DeltaBaseline` / `DeltaPlan` | streaming a changing set over a lossy link, diffed against what was acknowledged | [server_utils](../../server_utils/API_REFERENCE.md) |
| `DeltaMirror`, `SetDigest`, `SlotKey`, `SlotAllocator` | the client half of that stream and the check that both ends still agree | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PriorityAccumulator` | choosing which relevant entities fit *this* packet, without starving the rest | [server_utils](../../server_utils/API_REFERENCE.md) |
| `RestDetector` | knowing which entities have stopped, so a packet can stop paying for them | [server_utils](../../server_utils/API_REFERENCE.md) |
| `Told` | what each viewer has already been told, so a world that is not changing is not re-sent | [server_utils](../../server_utils/API_REFERENCE.md) |
| `Silence` | the client acting on an entity the server has stopped mentioning, with a grace period you choose | [client_utils](../../client_utils/API_REFERENCE.md) |
| `RateMeter` | live rates, means and shares on a HUD (re-exported by server_utils) | [client_utils](../../client_utils/API_REFERENCE.md) |

## Your own character (chapter 20)

| Block | Reach for this when | Lives in |
|---|---|---|
| `PredictedPlayer` | the wired bundle for a server that consumes one input per step | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HeldInputPredictor` | the wired bundle for a server that integrates held inputs | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PredictedEntity` + `ClientInputBuffer` | the primitives under both, for wiring your own | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ErrorSmoother` / `CorrectionMonitor` | easing what you draw after a correction / knowing whether that correction was normal | [client_utils](../../client_utils/API_REFERENCE.md) |
| `AdaptiveDecay` | clearing a large correction *sooner* than a small one, rather than in the same fixed time | [client_utils](../../client_utils/API_REFERENCE.md) |
| `InputCoalescer` | send-on-change plus keepalive, paired with held-input servers only | [client_utils](../../client_utils/API_REFERENCE.md) |
| `RoutePredictor` | a client that runs the same deterministic rule as the server (a pathfinder), so one op covers a whole walk | [client_utils](../../client_utils/API_REFERENCE.md) |
| reconciliation module (server half) | tracking which inputs each client has been credited for | [core](../../core/API_REFERENCE.md) |

## Everyone else and fairness (chapter 21)

| Block | Reach for this when | Lives in |
|---|---|---|
| `RemoteView` | an entity you do not control: push snapshots, ask for a render state | [client_utils](../../client_utils/API_REFERENCE.md) |
| `SnapshotBuffer` + `InterpolationClock` | rendering remotes a declared beat in the past | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HermiteView` | a send rate low enough that a straight line between samples visibly corners | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ExtrapolationBase` / `TrajectoryPredictor` | coasting through a gap, capped / the sub-10Hz special case | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ArrivalMonitor` | measuring what render delay your stream actually needs | [client_utils](../../client_utils/API_REFERENCE.md) |
| `HistoricalStateBuffer` | judging a shot at the time the shooter saw rather than the current time | [server_utils](../../server_utils/API_REFERENCE.md) |
| `render_error_at` | how wrong a client's screen was, asked at the instant it drew | [server_utils](../../server_utils/API_REFERENCE.md) |
| `InputSchedule` / `InputWindow` | executing inputs on the tick the client named, rejecting backdates | [server_utils](../../server_utils/API_REFERENCE.md) |
| `StateHistory`, `InputTimeline`, `RollbackSession` | peer-to-peer deterministic rollback, no server at all | [client_utils](../../client_utils/API_REFERENCE.md) |

## Clocks and time (chapters 20, 31)

| Block | Reach for this when | Lives in |
|---|---|---|
| `FixedTimestep` / `Periodic` | a variable frame driving a fixed-quantum sim / "is it time yet"; `FixedTimestep::advance` yields each step as a `Duration` | [client_utils](../../client_utils/API_REFERENCE.md) |
| `RttEstimator` | smoothed round trip, jitter and minimum from the probe plane | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ClockSyncEstimator` | server-clock offset and drift rate by least squares, for long sessions | [client_utils](../../client_utils/API_REFERENCE.md) |
| `Timeline` / `Probe` | keeping probe samples valid across reconnects and tab resumes | [client_utils](../../client_utils/API_REFERENCE.md) |
| `ScalarKalman` | optimally smoothing one noisy scalar | [client_utils](../../client_utils/API_REFERENCE.md) |

## The wire (chapter 30)

| Block | Reach for this when | Lives in |
|---|---|---|
| `frame` (kinds, split, begin) | the `[kind][body]` layout and the skip-unknown rule | [wire](../../wire/API_REFERENCE.md) |
| `framing`, `LengthDelimited` | the 4-byte length prefix a byte stream carries each frame behind, with a decoder for your own I/O | [wire](../../wire/API_REFERENCE.md) |
| `JsonCodec` / `MsgPackCodec` / `MsgPackNamedCodec` / `WireCodec` | JSON by default, compact MessagePack when measurement shows it saves enough, named MessagePack when the other end cannot be built from your struct definitions and your own codec when none fits | [wire](../../wire/API_REFERENCE.md) and [session](../../session/API_REFERENCE.md) |
| `build::Wire` / `build::emit` / `ProtocolVersion` | a wire version generated from your protocol types, so nobody bumps it by hand; `Wire::dart_types` also generates Dart types | [wire](../../wire/API_REFERENCE.md) |
| `answer_ping` | answering probes from a hand-written read loop | [wire](../../wire/API_REFERENCE.md) |
| `AckWindow` | telling the other side what arrived, in sixteen bytes | [client_utils](../../client_utils/API_REFERENCE.md) |
| `oneshot::Pending` | resending a one-shot op (a welcome, a refusal) until the other end shows it arrived, on a link that can lose it | [server_utils](../../server_utils/API_REFERENCE.md) |
| `bits` (`BitWriter`/`BitReader`, `quantize`, `smallest_three`, varints) | packing the hot array, where a byte-aligned codec cannot use a value's bounds and the bounds are where the saving comes from | [wire](../../wire/API_REFERENCE.md) |
| `BitCodec` | the same idea with no layout written by hand: saves 1.4x, which is as far as a derive can go | [wire](../../wire/API_REFERENCE.md) |
| `Payload` | carrying packed bytes in a field, without a codec re-encoding every byte as an integer | [wire](../../wire/API_REFERENCE.md) |
| payload types (`SequencedClientInput` and friends) | the shared netcode vocabulary, generic over your types | [wire](../../wire/API_REFERENCE.md) |

## Sockets and sessions (chapters 31, 32, 33)

| Block | Reach for this when | Lives in |
|---|---|---|
| `ActixWsPlazaSession` / `TcpPlazaSession` | the shipped transports | [session](../../session/API_REFERENCE.md) |
| `ConnectionManager` | the registry every transport is built on: register, forward, resolve, measure, close | [session](../../session/API_REFERENCE.md) |
| `LinkDriver`, `Conditioner`, `ProbeState` | the connection loop's parts, assembled or piecemeal, for transports of your own | [session](../../session/API_REFERENCE.md) |
| `LinkProfile` / `DirectionProfile` | impairment: delay, jitter, loss, per connection, per direction, at runtime | [session](../../session/API_REFERENCE.md) |
| `Host` | serving the browser bundle with cache busting, so a stale client only needs a reload | [session](../../session/API_REFERENCE.md) |
| `SessionOptions`, `Workload` | queue depths, limits and overflow policy, sized from a description of your traffic | [session](../../session/API_REFERENCE.md) |
| `Socket` trait, `loopback::pair`, `trim_backlog` | one client socket shape across desktop, wasm and in-process; resumed-tab backlog | [ws_client](../../ws_client/API_REFERENCE.md) |
| `FramePump` | the client half of the framed protocol: hello, probes, version check, credential and the server's goodbye | [ws_client](../../ws_client/API_REFERENCE.md) |
| `ScriptedSocket` | a socket whose arrivals a test feeds, for client code with no network | [ws_client](../../ws_client/API_REFERENCE.md) |
| `TransportStats` | what the transport carried and dropped, readable while it is busy | [session](../../session/API_REFERENCE.md) |
| `OutboundBudget`, `connection_owed` | sending a slow client fewer complete frames rather than losing frames from a full queue | [session](../../session/API_REFERENCE.md) |

## Saying no (chapter 40)

| Block | Reach for this when | Lives in |
|---|---|---|
| fallible `AgentFactory` | turning a TCP socket away by what it shows, before anything is registered for it | [session](../../session/API_REFERENCE.md) |
| `ConnectionAdmitter`, `admit_connection` / `bind_with_admitter` | admitting a client on the credential it presents after its `Hello`, with every identity rule judged before it registers | [session](../../session/API_REFERENCE.md) |
| `Farewell` | the close code and detail every server-initiated close writes last as a `Goodbye` | [session](../../session/API_REFERENCE.md) |
| `connections_of` + `PresenceEvent`'s conn id | resolving an account to a connection handle you can close | [session](../../session/API_REFERENCE.md) |
| `close_connection` / `deregister_agent` / `disconnect_all` | ending one connection, all of one account's or everyone's, with the reason arriving first | [session](../../session/API_REFERENCE.md) |
| `idle_for` / `agent_idle_for` | AFK rules that probe traffic cannot postpone | [session](../../session/API_REFERENCE.md) |
| `connection_inbound` / `agent_inbound` | attributing a flood to the connection sending it | [session](../../session/API_REFERENCE.md) |
| `SessionOptions::rate_limit_inbound`, `Rate` | making that flood cost only the connection sending it | [session](../../session/API_REFERENCE.md) |
| `OpGuard` | refusing an op before the rules see it, in one place rather than across every handler | [core](../../core/API_REFERENCE.md) |
| `set_deadline` | credits, trials and token expiry as one renewable mechanism | [session](../../session/API_REFERENCE.md) |

## Rooms and placement (chapter 41)

| Block | Reach for this when | Lives in |
|---|---|---|
| `RoomFactory` / `RoomHandle` | rooms spawned on demand behind a seam that names none of their types | [lobby](../../lobby/API_REFERENCE.md) |
| `InMemoryLobbyManager` | the assembled directory: create, list, join, reap | [lobby](../../lobby/API_REFERENCE.md) |
| `MatchQueue` | quick match with patience, driven from your tick | [lobby](../../lobby/API_REFERENCE.md) |
| `SeatReservations` | seat reservations that survive the old socket closing during a room hop and lapse if nobody connects | [lobby](../../lobby/API_REFERENCE.md) |
| `TicketStore` | placement handed to a client to present elsewhere (placement only, no authentication), as `MapTicketRegistry` or `CachedTicketRegistry` | [lobby](../../lobby/API_REFERENCE.md) |

## Testing and odds and ends

| Block | Reach for this when | Lives in |
|---|---|---|
| `LatencyLink`, `Rng` (feature `net-sim`) | deterministic latency, jitter and loss for tests, behaving like a real stream | [client_utils](../../client_utils/API_REFERENCE.md) |
| `PlayoutBuffer` / `Admission` | a playout queue that knows when a resumed tab's timeline is lost | [client_utils](../../client_utils/API_REFERENCE.md) |
| `Vec2` / `Vec3` / `Quat` and the `Interpolatable` trait | standalone math; or implement the traits on glam and keep your own | [client_utils](../../client_utils/API_REFERENCE.md) |
| `Fx`, `XorShift`, `ValueNoise`, `mix64` | fixed-point arithmetic and seeded draws that give the same number on both ends in every build | [client_utils](../../client_utils/API_REFERENCE.md) |
| schedulers, fsm, flow control, scorekeeping | optional core modules, take what fits, each a trait with a swappable impl | [core](../../core/API_REFERENCE.md) |
