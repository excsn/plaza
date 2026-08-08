# API Reference: `plaza`

## 1. Introduction & Core Concepts

`plaza` is the core crate of the Plaza workspace: a server-authoritative loop for applications where several agents act on one shared state.

An application supplies four things; `plaza` runs the loop around them.

*   **`StateType`**: the shared state. Any `Debug + Send + Sync + 'static` type; `Clone` is needed only if you call [`query_state`](#function-query_state), which copies the whole of it.
*   **`Op`**: the discrete actions that change it. `Clone + Debug + Send + Sync + 'static`, and `Serialize`/`Deserialize` if it crosses a network.
*   **[`StateLogic`](#trait-statelogic)**: the rules. The only place state is mutated.
*   **[`SnapshotProvider`](#trait-snapshotprovider)**: what a client is sent, built per recipient.

**Single-actor model.** A [`StateController`](#struct-statecontrollerop-id-statetype-sl-sess-sp) owns the state and mutates it only from its own task, processing one input at a time. Application logic therefore needs no locking. Nothing in this crate spawns a task except [`TickDriver`](#struct-tickdriver) and the caller's own `controller.run()`.

**Identity.** `ID` is the application's identifier type. Anything satisfying [`AgentId`](#trait-agentid) qualifies through a blanket impl, so it is rarely implemented by hand. [`Agent<ID>`](#enum-agentid-agentid) wraps it and distinguishes humans, bots, and the system.

**Transport is a trait.** [`Session`](#trait-session) abstracts the network. [`InProcessSession`](#struct-inprocesssessionop-id-agentid) ships here for tests and local play; the `plaza_session` crate provides WebSocket and TCP implementations.

## 2. Error Handling

### Enum `PlazaError<ID: AgentId>`

The crate's umbrella error. Implements `std::error::Error` via `thiserror`.

*   **Variants**: `Session(SessionError<ID>)`, `StateLogic(StateLogicError)`, `Snapshot(SnapshotError<ID>)`, `Serialization { message, source }`, `Deserialization { message, source }`, `Configuration(String)`, `InvalidArgument(String)`, `NotFoundById { id }`, `Io(std::io::Error)`, `Internal(String)`, `NotImplemented(String)`, `Application(Box<dyn Error>)`.
*   **Constructors**: `ser`, `deser` (wrap a source error), `ser_msg`, `deser_msg` (message only).

### Enum `SessionError<ID: AgentId>`

Transport-level failures: `AgentNotFound { id }`, `ConnectionError { id, details }`, `SendError(String)`, `SessionClosed`, `Timeout(String)`, `AuthenticationFailed { id, reason }`, `PermissionDenied { id, action }`, `TransportError(String)`.

### Enum `StateLogicError`

Returned by `StateLogic::process_input` when an input cannot be applied: `InvalidOperation(String)`, `Conflict(String)`, `PreconditionFailed(String)`, `Internal(String)`. The controller logs and continues, so a rejected op does not stop the loop.

### Enum `SnapshotError<ID: AgentId>`

`CreationFailed(String)`, `InvalidContext { id, reason }`, `NotFound { id }`, `Internal(String)`.

### Enum `QueryError`

`ControllerGone`: the only way [`query_state`](#function-query_state) can fail.

## 3. Agents

`AgentId` and `Agent` are **defined in [`plaza_wire`](../wire/) and re-exported here**, so a wasm client (which cannot depend on core) and a server name the same types. The paths below (`plaza::AgentId`, `plaza::Agent`, ...) resolve exactly as documented.

### Trait `AgentId`

```rust,ignore
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync
  + Serialize + for<'de> Deserialize<'de> + 'static {}
```

Blanket-implemented for every type meeting the bounds; `Uuid` and `u64` qualify as-is.

### Enum `Agent<ID: AgentId>`

Identity only. A display name is application data; keep it in your own state or in `ParticipantInfo::app_data`.

*   **Variants**: `Human(ID)`, `Bot(ID)`, `System`.
*   **Constructors**: `new_human(id)`, `new_bot(id)`, `system()`.
*   **Methods**:
    *   `id(&self) -> Option<&ID>`: `None` for `System`.
    *   `id_cloned(&self) -> Option<ID>`
    *   `is_system(&self) -> bool`
*   **Traits**: `Display` writes `human:7` / `bot:7` / `SYSTEM`, allocating nothing. `PartialEq`/`Hash` are by identity alone.

## 4. Application Traits

### Trait `StateLogic`

```rust,ignore
#[async_trait]
pub trait StateLogic<Op, ID: AgentId, StateType>: Send + Sync + 'static {
  async fn process_input(
    &self,
    current_state: &mut StateType,
    input: LogicInput<Op, ID>,
  ) -> Result<LogicOutput<Op, ID>, StateLogicError>;
}
```

The only place state changes. Called one input at a time from the controller's task.

#### Enum `LogicInput<Op, ID: AgentId>`

*   `AgentOps { source: Agent<ID>, ops: Vec<Op> }`
*   `TimeStep { delta_time: Duration }`
*   `AgentJoined { agent: Agent<ID> }`: the controller sends the joiner a snapshot immediately after this returns.
*   `AgentLeft { agent_id: ID }`

**Methods**: `kind(&self) -> &'static str`, the variant name, for grouping in logs and metrics. **Traits**: `Display` describes the input in full (`AgentOps(human:7, 3 ops)`) without allocating, so a switched-off `debug!` costs nothing on the tick path; use `kind()` where the value is captured eagerly, as span fields are.

#### Struct `LogicOutput<Op, ID: AgentId>`

What processing produced.

*   **Fields**: `ops: Vec<TargetedOp<Op, ID>>`, `snapshots: Vec<SnapshotRequest<ID>>`.
*   **Constructors**: `none()`, `ops(Vec<TargetedOp<..>>)`.
*   **Methods**: `and_snapshot(SnapshotRequest<ID>) -> Self` (builder-style); `coalesce(&mut self)`, which merges neighbouring ops sharing a target into one entry, so a tick's events travel in one envelope instead of one each. The controller calls it before sending, so logic is free to push an entry per event. Neighbours only: merging across a gap would reorder those ops against whatever sat between them, for any recipient that receives both. **Target alone, not sender**: a frame is a kind byte and the ops, so two entries with the same recipients are indistinguishable to that recipient however they were caused, and splitting a run on `from_agent` spent an envelope preserving nothing.
*   **Conversions**: `From<Vec<TargetedOp<Op, ID>>>`, so ops-only logic ends with `Ok(ops.into())`.
*   Ops are sent before snapshots, so a client sees the event explaining a change before the state reflecting it.

#### Struct `SnapshotRequest<ID: AgentId>`

*   **Fields**: `recipients: Vec<Agent<ID>>`, `context: Option<SnapshotContext>`, `uniform: bool`.
*   **Constructors**: `to(recipients)`, `with_context(recipients, context)` (per-recipient); `uniform(recipients)`, `uniform_with_context(recipients, context)` (one shared payload).
*   A uniform request runs the provider once with `target_agent: None` and sends the result to every recipient over `MessageTarget::Agents`, so a pass to N agents costs one snapshot build and one encode instead of N of each (145x at 40 KiB × 256 recipients, `session/benches/snapshot_fanout.rs`). The `None` view goes to everyone in the request, so it must contain nothing any recipient may not see: a game with hidden information keeps its per-player passes non-uniform.

### Trait `SnapshotProvider`

```rust,ignore
#[async_trait]
pub trait SnapshotProvider<ID: AgentId, StateType, Op>: Send + Sync + 'static {
  async fn create_snapshot(
    &self,
    full_state: &StateType,
    target_agent: Option<&Agent<ID>>,
    context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<ID>>;
}
```

**The seam for hidden information.** `target_agent` is who the snapshot is *for*, and the controller calls this once per recipient, so returning a different payload per agent (each player sees their own hand) is the normal path. A [uniform request](#struct-snapshotrequestid-agentid) instead calls it once with `target_agent: None`, and that view reaches every recipient in the request.

**Every call in a pass is started before any is awaited**, so a provider that reads a database or a cache overlaps its waits rather than serialising them. That matters because the controller is one task: a pass awaiting per agent stalls ticks and ops behind it, and against a provider suspending ~1.3ms, 64 recipients cost 85ms in sequence and 1.4ms overlapped (`core/benches/snapshots.rs`). A provider that never awaits is unaffected. The consequence is that **calls interleave**: one that relies on finishing before the next begins cannot assume it, though taking `&self` already ruled out holding state across a call.

**A snapshot is an `Op`.** The envelope has one message kind, so "replace everything" is a variant of your own op type rather than a wire concept. **Box it** if it carries a whole state view: unboxed, every op in every batch is sized to it. Return `Ok(None)` to send a recipient nothing, which is how an application declines a particular agent.

### Struct `NoSnapshots`

The shipped provider for an application with no snapshot concept at all: a chat relay, an event log, a client that rebuilds from the op stream. It answers `Ok(None)` for every recipient, so joining carries no catch-up. Implemented for every `ID`/`StateType`/`Op`, and `StateControllerBuilder::without_snapshots(logic, session, state)` takes it for you rather than making you write the `Ok(None)` yourself.

### Struct `SnapshotFn<F>`

A provider that is just a view function. Most providers are a pure function of the state and the recipient with an `async fn` and an `Ok(..)` wrapped around it; this is that wrapper, written once:

```rust,ignore
fn view(state: &Game, target: Option<&Agent<PlayerId>>) -> Option<GameOp> {
  Some(GameOp::Snapshot(Box::new(state.as_seen_by(target))))
}
let provider = Arc::new(SnapshotFn(view));
```

Return `None` to send a recipient nothing. A named function coerces cleanly; a closure usually needs its argument types written out. Anything fallible, or anything that must await, still implements [`SnapshotProvider`](#trait-snapshotprovider) directly.

#### Enum `SnapshotContext`

Which snapshot is wanted. **Plaza never reads this**: it carries it from caller to provider, both of which are yours.

*   **Variants**: `Full` (default), `DeltaFromVersion(u64)`, `ForPerspective(String)`, `Custom(Arc<dyn Any + Send + Sync>)`.
*   **Methods**: `custom<T>(value) -> Self`, `downcast_ref<T>() -> Option<&T>`.
*   The named variants are conveniences. When your notion of "which snapshot" is a content hash, a vector clock, or a typed enum, use `Custom`. Plaza tracks no versions and runs no acknowledgement protocol.

## 5. The Controller

### Struct `StateControllerBuilder<Op, ID, StateType, SL, Sess, SP>`

*   **`new(op_handler: Arc<SL>, session: Arc<Sess>, snapshot_provider: Arc<SP>, initial_state: StateType) -> Self`** All components are required, which is why `build` is infallible.
*   **`without_snapshots(op_handler: Arc<SL>, session: Arc<Sess>, initial_state: StateType) -> Self`**: the same for an application where joining carries no catch-up. Supplies [`NoSnapshots`](#struct-nosnapshots), so `SP` is fixed to it. Everything else is unchanged, including `SendSnapshots`, which becomes a request that sends nothing.
*   **`command_buffer(size: usize) -> Self`**: command channel depth. Default [`DEFAULT_COMMAND_BUFFER`](#constants) (32).
*   **`snapshot_context_on_join(context: Option<SnapshotContext>) -> Self`**: context for the snapshot a joining agent receives. Defaults to `Full`.
*   **`build(self) -> (CommandSender<Op, ID, StateType>, StateController<..>)`**

### Struct `StateController<Op, ID, StateType, SL, Sess, SP>`

*   **`async run(self) -> Result<StateType, PlazaError<ID>>`** Runs until `Shutdown` or its channels close, then returns the final state for the caller to persist. Commands already queued when `Shutdown` arrives are processed first; only what is already buffered is drained, so a producer that keeps sending cannot keep the controller alive.

### Enum `ControllerCommand<Op, ID: AgentId, StateType>`

*   `SubmitAgentOps { agent, ops }`
*   `SubmitSystemOps { source_description, ops }`
*   `ProcessTimeStep { delta_time }`
*   `HandleAgentJoined { agent }` / `HandleAgentLeft { agent_id }`: the push-style alternative to the session's own notifications; the lobby uses the latter.
*   `QueryCurrentState { response_tx: oneshot::ExclusiveSender<StateType> }`: the single-sender oneshot, since the sender is moved into the command and never cloned.
*   `SendSnapshots { recipients: Vec<Agent<ID>>, context: Option<SnapshotContext> }`: re-sends state, building a snapshot per recipient, with every provider call started before any is awaited. Recipients are explicit because the roster lives in your state, not the controller. Always per-recipient: a uniform pass is asked for from logic output, via `SnapshotRequest::uniform`.
*   `Shutdown`

### Type Alias `CommandSender<Op, ID, StateType>`

`fibre::mpsc::BoundedAsyncSender<ControllerCommand<..>>`. Cloneable, a tick driver, a lobby, and request handlers can all hold one.

### Function `query_with`

```rust,ignore
pub async fn query_with<Op, ID, StateType, T>(
  tx: &CommandSender<Op, ID, StateType>,
  read: impl FnOnce(&StateType) -> T + Send + 'static,
) -> Result<T, QueryError>
```

Asks the controller to compute something from its state and send that back. The closure runs **on the controller's task**, with the state borrowed rather than handed over, so it must not block or await: take what you need and get out.

This is the reason `StateType` need not be `Clone`. Nothing is copied unless the closure copies it, so `query_with(&tx, |s| s.players.len())` costs one `usize` where a copy of the world used to be the only option.

### Function `query_state`

```rust,ignore
pub async fn query_state<Op, ID, StateType>(
  tx: &CommandSender<Op, ID, StateType>,
) -> Result<StateType, QueryError>
```

The whole-state case of `query_with`, and the **only** place `StateType: Clone` is required. On a large world it copies all of it on the controller's task while the tick waits; prefer `query_with` when a field or a count is what you want.

### Struct `StateReader<StateType>`

What `QueryCurrentState` carries: a boxed `FnOnce(&StateType) + Send`, with `StateReader::new` to build one. Reach for it only when constructing the command by hand; `query_with` builds both it and the reply channel.

### Constants

*   `DEFAULT_COMMAND_BUFFER: usize = 32`

## 6. Sessions (Transport)

### Trait `Session`

```rust,ignore
#[async_trait]
pub trait Session<Op: Send + 'static, ID: AgentId>:
  Send + Sync + 'static
{
  async fn send_message(&self, target: MessageTarget<ID>, msg: SessionMessage<..>) -> Result<(), PlazaError<ID>>;
  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<..>>;
  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>>;
}
```

*   **One consumer.** The two streams deliver each item to exactly one receiver: they are *taken*, not subscribed to, and taking one twice panics. A session feeds exactly one controller, which is already the architecture.
*   **Membership is not on the trait.** Joins and leaves are transport-level facts the controller learns from the presence stream: a client joins by connecting, and a server-side disconnect is an inherent method on the transport that owns the connection (`InProcessSession::disconnect`, `ConnectionManager::close_connection`).

### Enum `PresenceEvent<ID: AgentId>`

`Joined { agent: Agent<ID>, conn_id: ConnectionId }` | `Left { agent_id: ID, conn_id: ConnectionId }`.

One stream rather than two **because order between them matters**: separate channels let a leave overtake the join that preceded it, which breaks reconnection.

The connection rides along with the agent because an agent may hold several at once, and acting on one (a close, a per-connection reader, a duplicate-login rule) needs the id of the connection the event was about. Nothing downstream can recover it: without this field an application cannot build an agent-to-connection index from public events at all.

### Enum `MessageTarget<ID: AgentId>`

`All`, `Agent(ID)`, `Agents(Vec<ID>)`, `AllExcept(ID)`, `AllExceptThese(Vec<ID>)`.

### Struct `SessionMessage<Op, ID: AgentId>`

`{ from: Agent<ID>, ops: Vec<Op> }`, with `new(from, ops)` and `system(ops)`. **Server-side only and deliberately not `Serialize`**: the wire carries `[kind byte][encoded ops]` and nothing else, so `from` is bookkeeping the transport attaches inbound and never sends outbound. It sits beside `MessageTarget`, `PresenceEvent` and `TargetedOp` for the same reason they do.

### Struct `TargetedOp<Op, ID: AgentId>`

*   **Fields**: `from_agent`, `target`, `ops`.
*   **Constructors**: `new(from_agent, target, ops)`, `new_system_all(ops)`, `new_system_to(agent_id, ops)`.

### Struct `InProcessSession<Op, ID: AgentId>`

Loopback transport for tests, demos, and local play. Each client gets its own inbox and `MessageTarget` is resolved server-side, exactly as a real transport would.

*   **`new() -> Arc<Self>`**, **`with_capacity(session_capacity, client_capacity) -> Arc<Self>`**
*   **`async connect(&self, agent) -> Result<(ConnectionId, ClientInbox<..>), PlazaError<ID>>`** Registers a client and announces the join: read the inbox for the snapshot that follows.
*   **`async disconnect(&self, agent_id: &ID, conn_id: ConnectionId)`**: removes the client and announces the leave, the in-process equivalent of the socket closing.
*   **`async client_send(&self, from: Agent<ID>, ops: Vec<Op>)`**: as if a client sent them.
*   **`connected_agents(&self) -> Vec<Agent<ID>>`**

### Type Aliases

*   `ConnectionId = u64`
*   `SessionReceiver<T>` / `SessionSender<T>`: bounded async MPSC handles. The concrete channel is `fibre`'s, which is part of plaza's contract; build pairs with `session_channel` rather than naming the crate.
*   **`session_channel<T: Send>(capacity) -> (SessionSender<T>, SessionReceiver<T>)`**: the constructor behind every `Session` stream, so a transport outside this workspace produces the exact type the trait returns without a fibre dependency of its own. Panics if `capacity` is zero.
*   `ClientInbox<Op, ID>`: the receiving end of a simulated client.

### Constants

*   `DEFAULT_SESSION_CAPACITY: usize = 256`
*   `DEFAULT_CLIENT_CAPACITY: usize = 64`

## 7. Time

### Struct `TickDriver`

Sends `ProcessTimeStep` at a fixed rate; the controller does not advance time on its own.

*   **`new(interval: Duration) -> Self`** (panics on zero), **`from_hz(hz: u32) -> Self`**
*   **`async run<..>(self, tx: CommandSender<..>)`**: until the channel closes. `delta_time` is measured elapsed time, so logic integrating over it stays correct when a tick runs late.
*   **`async run_for<..>(self, tx, max_ticks: u64)`**: bounded, for demos and tests.
*   **`async run_fixed<..>(self, tx, step: Duration)`** / **`async run_fixed_for<..>(self, tx, step, max_steps: u64)`**: the same cadence, but `delta_time` is **always exactly `step`**. The driver wakes on its interval, accumulates the measured elapsed time, and spends it as whole steps, carrying the remainder. Bounded by *steps* rather than wakes, because with a fixed step the steps are the amount of simulated time a caller is counting.
*   **`async run_virtual<..>(tx: &CommandSender<..>, delta_time: Duration, steps: u64) -> u64`** Advances game time without waiting on real time: fast-forwarding past a timeout in a test. Returns steps delivered.
*   **`MAX_STEPS_PER_WAKE`** (module constant, 8): the most fixed steps one wake will spend. After a long stall the world falls behind rather than fast-forwarding, because repaying a five second stall as three hundred back-to-back steps is a freeze, and it lands exactly when the machine is already struggling.

**Which one to use, and this is not a style preference.** Use `run_fixed` whenever anything **predicts, replays, or rolls back** this logic. Measured elapsed time means the step size is whatever the host's scheduler delivered: 16 ms, then 17, then 16. A simulation advanced by that is a function of the scheduler as well as of its inputs, and no client can reproduce it, because a client stepping in fixed ticks and a server stepping in measured ones accumulate the same motion at different rates. In a continuous game that presents as a permanent small correction; in a discrete one the two sides cross each boundary a step apart and every crossing is a visible jump. `run` is right for logic that only integrates: a physics step, a decay, a cooldown.

The interval and the step are separate on purpose. Waking more often than you step keeps the phase error small, because a step is spent nearer the moment it was earned; waking less often batches them. Setting both the same is the ordinary choice.

**A predicted simulation should still own its own quantum.** `run_fixed` fixes the driver, not the contract: a different driver, a test harness, or a hand-rolled loop can still hand the logic an arbitrary delta. Accumulating inside the simulation as well costs four lines and means the guarantee cannot be broken from outside; `examples/bomb_grid` does both, and `plaza_client_utils::FixedTimestep` is the same pattern for the client side.

## 8. Observability (module `stats`)

### Struct `ControllerStats`

Live counters for one running controller, obtained from
[`StateControllerBuilder::stats`](#struct-statecontrollerbuilderop-id-statetype-sl-sess-sp) before `build`
(or `with_stats` to supply one you already hold), and from `StateController::stats` after.

*   **`ticks()`**, **`commands()`**, **`ops()`**, **`joins()`**, **`leaves()`**, **`snapshots()`**.
*   **`mean_tick()`** / **`worst_tick()`**: how long a `ProcessTimeStep` took, against which your tick interval says whether the simulation keeps up with itself. Both are kept because one slow tick in a thousand is invisible in a mean and is exactly the hitch a player notices.
*   **`busy()`**: total time handling commands, against wall time the non-idle fraction.
*   **`queue_depth()`** / **`deepest_queue()`**: how many commands were waiting when the last was taken. A depth sitting near the buffer size is a producer outrunning the loop, which is the state before commands are dropped.

**Shared memory rather than a command, and that is the whole design.** The obvious alternative is a `ControllerCommand::QueryStats`, which travels the same queue it reports on: answered slowly by a busy controller and not at all by a wedged one, so the reading goes blank exactly when it becomes interesting. **You cannot ask a stalled thing how stalled it is.** Reads are relaxed atomics, so they never wait on the controller and can happen mid-tick from another thread. The same reasoning rules out a callback, which would run application code inside the loop, the deadlock this crate already refuses in `StateLogic`.

**Not a metrics framework**: no registry, labels, histograms or exporter, because shipping one picks a scheme every application then works around. And it holds only what nothing else can reach, so connection counts stay with the transport and how long *your* logic took stays measurable inside your own `StateLogic`. Two numbers for one fact eventually disagree.

A reading is a **sample**, not a transaction: two fields read in succession may straddle a tick boundary, which matters if you compute a ratio from them.

## 9. Module `common`: reusable infrastructure

### `common::scheduler`

Two shapes, each generic over a time axis.

*   **Trait `SchedulerInstant`**: implemented for `u64` (ticks) and `Duration` (game time). `add_interval` saturates; `interval_is_zero` guards repeats.
*   **Struct `EventScheduler<T, E>`**: returns due event payloads. `schedule_at`, `schedule_after`, `schedule_repeating_at`, `schedule_repeating_after`, `tick(now) -> Vec<E>`, `cancel(id)`, `cancel_matching(predicate) -> usize`, `any_pending(predicate)`, `next_trigger()`, `len`, `is_empty`, `clear`.
*   **Struct `CallbackScheduler<T, StateType, Op, ID>`**: runs closures against state instead. Same scheduling methods; `tick(now, &mut state, &mut ops_to_broadcast)`.
*   **Type `ScheduledAction<StateType, Op, ID>`**: `Box<dyn FnMut(&mut StateType, &mut Vec<TargetedOp<..>>) + Send>`.
*   **Struct `ScheduledEventId`**: returned by every `schedule_*`, accepted by `cancel`.
*   **Aliases**: `TickEventScheduler<E>`, `TimeEventScheduler<E>`, `TickCallbackScheduler<..>`, `TimeCallbackScheduler<..>`.

Repeating schedules skip ahead past `now` after a stall rather than replaying every missed interval.

### `common::reconnect`

*   **Struct `ReconnectTracker<ID: AgentId, T: SchedulerInstant>`**: bookkeeping for disconnect grace periods. Holds no timers and spawns nothing; keep one in your `StateType` and drive it.
    *   `new(grace: T)`
    *   `on_disconnect(agent_id, now)`: call on `AgentLeft`; restarts the clock if already pending.
    *   `on_reconnect(&agent_id) -> bool`: call on `AgentJoined`. `true` means a genuine return within the window, `false` a first join.
    *   `expired(now) -> Vec<ID>`: drive from `TimeStep`. Returns who ran out so the consequence stays yours.
    *   `is_awaiting_reconnect`, `deadline_for`, `awaiting`, `count`, `is_empty`, `forget` (a deliberate quit), `clear`.
*   A returning client must present the *same* agent ID: derive it from an auth token in your route handler, not per connection.

### `common::fsm`

*   **Trait `FsmContext<Op, AppID>`**: `ops_q()` and `as_any_mut()`.
*   **Struct `OpsQueue<Op, AppID>`**: the minimal context: an op queue and nothing else. `new`, `from_ops`, `into_ops`, `len`, `is_empty`.
*   **Trait `State<Op, AppID, StateIdEnum>`**: `id`, `on_enter`, `on_update`, `on_exit`.
*   **Struct `StateMachine<Op, AppID, StateIdEnum>`**: `new`, `add_state`, `set_initial_state`, `current_state_id`, `update`, `transition`.

### `common::participants`

*   **Trait `ParticipantAppSpecificData`**: blanket marker for per-participant data.
*   **Struct `ParticipantInfo<ID, Data>`**: `agent`, `app_data`. A display name goes in `app_data`: `Agent` is identity, and this is the tracker's slot for everything else.
*   **Struct `ParticipantTracker<ID, Data>`**: `add_participant`, `remove_participant`, `get_participant[_mut]`, `get_participant_app_data[_mut]`, `contains_participant`, `iter[_mut]`, `all_agent_ids`, `agents` (every tracked agent cloned, the shape `SnapshotRequest::to` wants), `agents_except` (the usual recipient list for reacting to something one agent just did), `count`, `is_empty`.

### `common::math`

Serde-friendly PODs for op payloads: `Vec2`, `Vec3`, `Quat` (with `Quat::IDENTITY`, and `Default` returning identity rather than zeroes). Applications with real math needs should use their own types: every payload is generic over them.

## 10. Module `game_common`: game patterns

### `game_common::reconciliation`

The server half of client-side prediction; the client half is `plaza_client_utils`.

*   **Struct `ClientInputTracker<ID>`**: last processed input sequence per client. `record_processed_input`, `get_last_processed_input_seq`, `on_client_disconnect`, `clear_all`.
*   **Struct `ServerInputBuffer<ID, InputData, ServerTime>`**: buffers inputs a fixed delay before processing, for fairness across latencies. `add_input`, `drain_delayed_inputs(now, delay)`, `clear_inputs_for_client`, `clear_all`.
*   **Struct `BufferedInput<InputData, ServerTime>`**: `client_input`, `server_received_time`.
*   **Struct `HistoricalStateBuffer<EntityId, Snapshot, ServerTime>`**: rewind buffer for lag compensation. `record_state`, `get_state_at_or_before`, `remove_entity_history`, `clear_all_history`. Queries outside the retained range clamp to the nearest snapshot.
*   **Trait `Interpolatable<TimePoint>`**: `interpolate(other, t, time_a, time_b)`.
*   **Struct `TimedState<ServerTime, State>`**: `time`, `state`.
*   **Payloads**: `SequencedClientInput`, `AuthoritativeStateUpdate`, `TimestampedClientAction`, `RemoteEntitySnapshot`. Re-exports `Vec2`/`Vec3`/`Quat`.

### `game_common::flow_control`

Turns and rounds are a trait plus a ready-made implementation, so anything provided can be swapped. Both emit notice ops through an `FsmContext`; because a manager cannot know your `Op` type, you supply the constructor wrapping each notice payload into it, as a plain `fn` pointer.

Phases get no trait. *When* a phase changes varies too much between games for one shape to fit, so plaza takes no position on it and the transition rules stay yours. *That a change reaches clients* does not vary, so `Phased` owns the field and makes changing it silently inexpressible. See the `phases` module docs for where that line falls.

All three types are `Clone` and hold no timers, channels, or boxed closures, so a game that searches ahead can clone its state and re-run flow control in simulation.

*   **Trait `TurnManager<Op, AppID, TurnActorId>`**: `current_turn_actor`, `begin(context)`, `restart(context)`, `add_actor`, `remove_actor`, `end_current_turn_and_advance(context)`.
*   **It is a conformance target, not a dispatch mechanism.** Nothing holds a `dyn TurnManager` and probably nothing will, since a game knows which order it plays in. What the trait answers is "I am writing an order of my own, what must it provide?", and it held only the first and last of those methods until [`draft_board`](../examples/draft_board/) wrote a second implementation and found that every consumer was calling five. A manager an application cannot seat or change the roster of is not usable, so seating and roster are on the trait rather than left to each implementation to remember.
*   **Enum `Advanced<TurnActorId>`**: `Next(actor)` or `PassClosed(actor)`, with `actor()`, `into_actor()` and `pass_closed()`. Returned by `end_current_turn_and_advance`, because whether a pass over the roster just closed is knowable only inside the manager: *how* a pass ends is the thing implementations differ about. Round-robin wraps, a snake reverses onto the **same actor**, so a caller comparing the new turn against the old reads the boundary backwards exactly where it matters.
*   **The next actor may be the same actor.** The method promises the next turn, not a different holder of it.
*   **Struct `RoundRobinTurnManager<Op, AppID, TurnActorId>`**: the trait methods plus `new(actors, notice_fn)`, `with_time_limit`, `actors`, `turn_number`. Removing the actor whose turn it is passes play to whoever fills that slot rather than stalling. `begin` declines to interrupt an active turn; `restart` seats the first actor again and resets `turn_number` to 1, which is what a game whose order restarts each round needs. `turn_number` counts turns since the order began, so round-scoped or match-scoped follows from how long you keep one manager.
*   **Trait `RoundManager<Op, AppID>`**: `current_round`, `max_rounds`, `start_next_round(context)`, `end_current_round(context, reason)`.
*   **Struct `SequentialRoundManager<Op, AppID, Summary>`**: `new(max_rounds, started_fn, ended_fn)`, `end_round_with(context, reason, summary)`, `round_in_progress`, `is_finished`. Starting a round while one is running is an error, so a game cannot silently skip scoring.
*   **`RoundManager::reset()`** puts the count back to zero for a fresh match, keeping the limit and the notice constructors. A manager counts up and had no other way back, so both card examples were replacing the whole thing to play again, restating configuration at a call site with no business knowing it.
*   **Struct `Phased<P>`**: `new(initial)`, `current() -> &P`, `epoch() -> Epoch`, `is_current(epoch) -> bool`, `transition_to(next, context, notice) -> bool`, `transition_with(next, context, notice, reason, duration_hint) -> bool`. Transitioning to the phase already in effect is a no-op that emits nothing and does not bump the epoch, so a check running at several call sites cannot spam duplicate notices.
*   **Struct `Epoch`**: an opaque token for one occupancy of a phase, `Copy`. Capture it when scheduling deferred work, compare it on resume to learn whether the phase moved underneath. A stale token means only that; what follows is application policy.
*   **Struct `PhasedScheduler<E>`**: a tick scheduler whose every event belongs to one phase occupancy. `schedule_after(now, delay, &phased, event)` captures the epoch itself; `due(now, &phased)` yields only events whose occupancy still holds and drops the rest with a debug line. Extracted after the pairing was hand-written nine times across four examples, every copy the same `if !phase.is_current(epoch) continue`. What stays with the application is everything past the epoch: whether the timed-out player is still on turn is the game's check, and under a snake order it is an identity check a generation counter would get wrong. `any_pending(predicate)`, `is_empty`.
*   **Payloads**: `TurnChangedNoticePayload`, `EndTurnRequestPayload`, `RoundStartedNoticePayload`, `RoundEndedNoticePayload`, `PhaseChangedNoticePayload`, `RequestPhaseTransitionPayload`, `CountdownTickNoticePayload`.

### `game_common::scorekeeping`

*   **Trait `Scorekeeper<ID, ScoreType>`**, **Trait `ScoreValue`** (blanket bounds).
*   **Struct `HashMapScorekeeper<ID, ScoreType>`**.
*   **Staying versus leaving are different questions.** `reset_player_score` sets someone to zero and leaves them on the board; `forget_player` takes them off it and returns what it discarded. `reset_all_scores` keeps the roster for a new round; `clear_all_scores` drops it for a new roster. **Call `forget_player` from your own rules, never from a disconnect**: a scorekeeper is never told a socket closed and could not read one correctly if it were, which is `SeatReservations::withdraw`'s lesson from the other end. A room that lives for one match usually keeps departed players so the board does not reshuffle mid-game; a standing room cycling players for hours has to forget them or its leaderboard fills with zeroes for people who left.
*   **Payloads**: `SetScorePayload`, `IncrementScorePayload`, `ScoreUpdatedNoticePayload`.

### `game_common::input_intent`

*   **Struct `PlayerIntent<ID, Intent>`**: `new`.

## 11. Module `app_common`: collaboration payloads

Op shapes for non-game applications. These are payload definitions, not engines; the application still writes the logic.

*   **`locking`**: `LockManager<R, ID>` (`try_acquire_lock`, `release_lock`, `force_release_lock`, `get_lock_owner`), `LockInfo`, and the request/release/notice payloads.
*   **`presence`**: `UpdatePresencePayload`, `PresenceChangedNoticePayload`, and fragments `CursorPositionPayload`, `SelectionPayload`, `ActivityStatusPayload`.
*   **`ordered_collection_ops`**: `InsertListItemPayload`, `RemoveListItemPayload`, `MoveListItemPayload`, `UpdateListItemPayload`.
*   **`object_property_ops`**: `CreateObjectPayload`, `DeleteObjectPayload`, `SetObjectPropertyPayload`, `DeleteObjectPropertyPayload`.

## 12. Crate Re-exports

For convenience, the crate root re-exports: `Agent`, `AgentId`, `CommandSender`, `ControllerCommand`, `StateController`, `StateControllerBuilder`, `query_state`, `query_with`, `StateReader`, `NoSnapshots`, `PlazaError`, `InProcessSession`, `MessageTarget`, `Session`, `SessionMessage`, `TargetedOp`, `SnapshotProvider`, `LogicInput`, `StateLogic`, `TickDriver`.
