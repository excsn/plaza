# API Reference: `plaza`

## 1. Introduction & Core Concepts

`plaza` is the core crate of the Plaza workspace: a server-authoritative loop for applications where several agents act on one shared state.

An application supplies four things; `plaza` runs the loop around them.

*   **`StateType`**: the shared state. Any `Clone + Debug + Send + Sync + 'static` type.
*   **`Op`**: the discrete actions that change it. `Clone + Debug + Send + Sync + 'static`, and `Serialize`/`Deserialize` if it crosses a network.
*   **[`StateLogic`](#stateloginc)**: the rules. The only place state is mutated.
*   **[`SnapshotProvider`](#snapshotprovider)**: what a client is sent, built per recipient.

**Single-actor model.** A [`StateController`](#statecontroller) owns the state and mutates it only from its own task, processing one input at a time. Application logic therefore needs no locking. Nothing in this crate spawns a task except [`TickDriver`](#tickdriver) and the caller's own `controller.run()`.

**Identity.** `ID` is the application's identifier type. Anything satisfying [`AgentId`](#agentid) qualifies through a blanket impl, so it is rarely implemented by hand. [`Agent<ID>`](#agent) wraps it and distinguishes humans, bots, and the system.

**Transport is a trait.** [`Session`](#session) abstracts the network. [`InProcessSession`](#inprocesssession) ships here for tests and local play; the `plaza_session` crate provides WebSocket and TCP implementations.

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

`ControllerGone`: the only way [`query_state`](#query_state) can fail.

## 3. Agents

### Trait `AgentId`

```rust,ignore
pub trait AgentId: Clone + Debug + Eq + Hash + Send + Sync
  + Serialize + for<'de> Deserialize<'de> + 'static {}
```

Blanket-implemented for every type meeting the bounds; `Uuid` and `u64` qualify as-is.

### Enum `Agent<ID: AgentId>`

*   **Variants**: `Human { id, name }`, `Bot { id, name }`, `System`.
*   **Constructors**: `new_human(id, name)`, `new_bot(id, name)`, `system()`.
*   **Methods**:
    *   `id(&self) -> Option<&ID>`: `None` for `System`.
    *   `id_cloned(&self) -> Option<ID>`
    *   `label(&self) -> String`: the name, or `"SYSTEM"`.
    *   `is_system(&self) -> bool`

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

#### Struct `LogicOutput<Op, ID: AgentId>`

What processing produced.

*   **Fields**: `ops: Vec<TargetedOp<Op, ID>>`, `snapshots: Vec<SnapshotRequest<ID>>`.
*   **Constructors**: `none()`, `ops(Vec<TargetedOp<..>>)`.
*   **Methods**: `and_snapshot(SnapshotRequest<ID>) -> Self` (builder-style).
*   **Conversions**: `From<Vec<TargetedOp<Op, ID>>>`, so ops-only logic ends with `Ok(ops.into())`.
*   Ops are sent before snapshots, so a client sees the event explaining a change before the state reflecting it.

#### Struct `SnapshotRequest<ID: AgentId>`

*   **Fields**: `recipients: Vec<Agent<ID>>`, `context: Option<SnapshotContext>`.
*   **Constructors**: `to(recipients)`, `with_context(recipients, context)`.

### Trait `SnapshotProvider`

```rust,ignore
#[async_trait]
pub trait SnapshotProvider<ID: AgentId, StateType, SnapshotPayload>: Send + Sync + 'static {
  async fn create_snapshot_data(
    &self,
    full_state: &StateType,
    target_agent: Option<&Agent<ID>>,
    context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<SnapshotPayload>, SnapshotError<ID>>;
}
```

**The seam for hidden information.** `target_agent` is who the snapshot is *for*, and the controller calls this once per recipient, so returning a different payload per agent (each player sees their own hand) is the normal path.

#### Struct `SnapshotData<SnapshotPayload>`

Wrapper with one field, `payload`, so versioning can be added later without changing the trait signature.

#### Enum `SnapshotContext`

Which snapshot is wanted. **Plaza never reads this**: it carries it from caller to provider, both of which are yours.

*   **Variants**: `Full` (default), `DeltaFromVersion(u64)`, `ForPerspective(String)`, `Custom(Arc<dyn Any + Send + Sync>)`.
*   **Methods**: `custom<T>(value) -> Self`, `downcast_ref<T>() -> Option<&T>`.
*   The named variants are conveniences. When your notion of "which snapshot" is a content hash, a vector clock, or a typed enum, use `Custom`. Plaza tracks no versions and runs no acknowledgement protocol.

## 5. The Controller

### Struct `StateControllerBuilder<Op, ID, StateType, SnapshotPayload, SL, Sess, SP>`

*   **`new(op_handler: Arc<SL>, session: Arc<Sess>, snapshot_provider: Arc<SP>, initial_state: StateType) -> Self`** All components are required, which is why `build` is infallible.
*   **`command_buffer(size: usize) -> Self`**: command channel depth. Default [`DEFAULT_COMMAND_BUFFER`](#constants) (32).
*   **`snapshot_context_on_join(context: Option<SnapshotContext>) -> Self`**: context for the snapshot a joining agent receives. Defaults to `Full`.
*   **`build(self) -> (CommandSender<Op, ID, StateType>, StateController<..>)`**

### Struct `StateController<Op, ID, StateType, SnapshotPayload, SL, Sess, SP>`

*   **`async run(self) -> Result<StateType, PlazaError<ID>>`** Runs until `Shutdown` or its channels close, then returns the final state for the caller to persist. Commands already queued when `Shutdown` arrives are processed first; only what is already buffered is drained, so a producer that keeps sending cannot keep the controller alive.

### Enum `ControllerCommand<Op, ID: AgentId, StateType>`

*   `SubmitAgentOps { agent, ops }`
*   `SubmitSystemOps { source_description, ops }`
*   `ProcessTimeStep { delta_time }`
*   `HandleAgentJoined { agent }` / `HandleAgentLeft { agent_id }`: the push-style alternative to the session's own notifications; the lobby uses the latter.
*   `QueryCurrentState { response_tx: oneshot::Sender<StateType> }`
*   `SendSnapshots { recipients: Vec<Agent<ID>>, context: Option<SnapshotContext> }`: re-sends state, building a snapshot per recipient. Recipients are explicit because the roster lives in your state, not the controller.
*   `Shutdown`

### Type Alias `CommandSender<Op, ID, StateType>`

`fibre::mpsc::BoundedAsyncSender<ControllerCommand<..>>`. Cloneable, a tick driver, a lobby, and request handlers can all hold one.

### Function `query_state`

```rust,ignore
pub async fn query_state<Op, ID, StateType>(
  tx: &CommandSender<Op, ID, StateType>,
) -> Result<StateType, QueryError>
```

Wraps the request/response channel dance for `QueryCurrentState`.

### Constants

*   `DEFAULT_COMMAND_BUFFER: usize = 32`

## 6. Sessions (Transport)

### Trait `Session`

```rust,ignore
#[async_trait]
pub trait Session<Op: Send + 'static, ID: AgentId, SnapshotPayload: Send + 'static>:
  Send + Sync + 'static
{
  async fn agent_join(&self, agent_info: Agent<ID>) -> Result<ConnectionId, PlazaError<ID>>;
  async fn agent_leave(&self, agent_id: &ID, conn_id: ConnectionId) -> Result<(), PlazaError<ID>>;
  async fn send_message(&self, target: MessageTarget<ID>, msg: SessionMessage<..>) -> Result<(), PlazaError<ID>>;
  fn subscribe_to_incoming_messages(&self) -> SessionReceiver<SessionMessage<..>>;
  fn on_presence_change(&self) -> SessionReceiver<PresenceEvent<ID>>;
}
```

*   **One consumer.** The two streams deliver each item to exactly one receiver: they are *taken*, not subscribed to, and taking one twice panics. A session feeds exactly one controller, which is already the architecture.
*   **`agent_join`** returns `NotImplemented` on networked transports, where a client joins by connecting.

### Enum `PresenceEvent<ID: AgentId>`

`Joined(Agent<ID>)` | `Left(ID)`.

One stream rather than two **because order between them matters**: separate channels let a leave overtake the join that preceded it, which breaks reconnection.

### Enum `MessageTarget<ID: AgentId>`

`All`, `Agent(ID)`, `Agents(Vec<ID>)`, `AllExcept(ID)`, `AllExceptThese(Vec<ID>)`.

### Enum `SessionMessage<Op, ID: AgentId, SnapshotPayload>`

`Ops { from: Agent<ID>, ops: Vec<Op> }` | `StateData { from, data: SnapshotData<..> }`. `Serialize`/`Deserialize` when its parameters are.

### Struct `TargetedOp<Op, ID: AgentId>`

*   **Fields**: `from_agent`, `target`, `ops`.
*   **Constructors**: `new(from_agent, target, ops)`, `new_system_all(ops)`, `new_system_to(agent_id, ops)`.

### Struct `InProcessSession<Op, ID: AgentId, SnapshotPayload>`

Loopback transport for tests, demos, and local play. Each client gets its own inbox and `MessageTarget` is resolved server-side, exactly as a real transport would.

*   **`new() -> Arc<Self>`**, **`with_capacity(session_capacity, client_capacity) -> Arc<Self>`**
*   **`async connect(&self, agent) -> Result<(ConnectionId, ClientInbox<..>), PlazaError<ID>>`** Registers a client and announces the join: read the inbox for the snapshot that follows.
*   **`async client_send(&self, from: Agent<ID>, ops: Vec<Op>)`**: as if a client sent them.
*   **`connected_agents(&self) -> Vec<Agent<ID>>`**

### Type Aliases

*   `ConnectionId = u64`
*   `SessionReceiver<T>` / `SessionSender<T>`: `fibre::mpsc` bounded async handles.
*   `ClientInbox<Op, ID, SnapshotPayload>`: the receiving end of a simulated client.

### Constants

*   `DEFAULT_SESSION_CAPACITY: usize = 256`
*   `DEFAULT_CLIENT_CAPACITY: usize = 64`

## 7. Time

### Struct `TickDriver`

Sends `ProcessTimeStep` at a fixed rate; the controller does not advance time on its own.

*   **`new(interval: Duration) -> Self`** (panics on zero), **`from_hz(hz: u32) -> Self`**
*   **`async run<..>(self, tx: CommandSender<..>)`**: until the channel closes. `delta_time` is measured elapsed time, so logic integrating over it stays correct when a tick runs late.
*   **`async run_for<..>(self, tx, max_ticks: u64)`**: bounded, for demos and tests.
*   **`async run_virtual<..>(tx: &CommandSender<..>, delta_time: Duration, steps: u64) -> u64`** Advances game time without waiting on real time: fast-forwarding past a timeout in a test. Returns steps delivered.

## 8. Module `common`: reusable infrastructure

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
*   **Struct `ParticipantInfo<ID, Data>`**: `agent`, `app_data`.
*   **Struct `ParticipantTracker<ID, Data>`**: `add_participant`, `remove_participant`, `get_participant[_mut]`, `get_participant_app_data[_mut]`, `contains_participant`, `iter[_mut]`, `all_agent_ids`, `count`, `is_empty`.

### `common::math`

Serde-friendly PODs for op payloads: `Vec2`, `Vec3`, `Quat` (with `Quat::IDENTITY`, and `Default` returning identity rather than zeroes). Applications with real math needs should use their own types: every payload is generic over them.

## 9. Module `game_common`: game patterns

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

*   **Trait `TurnManager<Op, AppID, TurnActorId>`**: `current_turn_actor`, `end_current_turn_and_advance(context)`.
*   **Struct `RoundRobinTurnManager<Op, AppID, TurnActorId>`**: `new(actors, notice_fn)`, `with_time_limit`, `begin(context)`, `restart(context)`, `add_actor`, `remove_actor`, `actors`, `turn_number`. Removing the actor whose turn it is passes play to whoever fills that slot rather than stalling. `begin` declines to interrupt an active turn; `restart` seats the first actor again and resets `turn_number` to 1, which is what a game whose order restarts each round needs. `turn_number` counts turns since the order began, so round-scoped or match-scoped follows from how long you keep one manager.
*   **Trait `RoundManager<Op, AppID>`**: `current_round`, `max_rounds`, `start_next_round(context)`, `end_current_round(context, reason)`.
*   **Struct `SequentialRoundManager<Op, AppID, Summary>`**: `new(max_rounds, started_fn, ended_fn)`, `end_round_with(context, reason, summary)`, `round_in_progress`, `is_finished`. Starting a round while one is running is an error, so a game cannot silently skip scoring.
*   **Struct `Phased<P>`**: `new(initial)`, `current() -> &P`, `epoch() -> Epoch`, `is_current(epoch) -> bool`, `transition_to(next, context, notice) -> bool`, `transition_with(next, context, notice, reason, duration_hint) -> bool`. Transitioning to the phase already in effect is a no-op that emits nothing and does not bump the epoch, so a check running at several call sites cannot spam duplicate notices.
*   **Struct `Epoch`**: an opaque token for one occupancy of a phase, `Copy`. Capture it when scheduling deferred work, compare it on resume to learn whether the phase moved underneath. A stale token means only that; what follows is application policy.
*   **Payloads**: `TurnChangedNoticePayload`, `EndTurnRequestPayload`, `RoundStartedNoticePayload`, `RoundEndedNoticePayload`, `PhaseChangedNoticePayload`, `RequestPhaseTransitionPayload`, `CountdownTickNoticePayload`.

### `game_common::scorekeeping`

*   **Trait `Scorekeeper<ID, ScoreType>`**, **Trait `ScoreValue`** (blanket bounds).
*   **Struct `HashMapScorekeeper<ID, ScoreType>`**.
*   **Payloads**: `SetScorePayload`, `IncrementScorePayload`, `ScoreUpdatedNoticePayload`.

### `game_common::input_intent`

*   **Struct `PlayerIntent<ID, Intent>`**: `new`.

## 10. Module `app_common`: collaboration payloads

Op shapes for non-game applications. These are payload definitions, not engines; the application still writes the logic.

*   **`locking`**: `LockManager<R, ID>` (`try_acquire_lock`, `release_lock`, `force_release_lock`, `get_lock_owner`), `LockInfo`, and the request/release/notice payloads.
*   **`presence`**: `UpdatePresencePayload`, `PresenceChangedNoticePayload`, and fragments `CursorPositionPayload`, `SelectionPayload`, `ActivityStatusPayload`.
*   **`ordered_collection_ops`**: `InsertListItemPayload`, `RemoveListItemPayload`, `MoveListItemPayload`, `UpdateListItemPayload`.
*   **`object_property_ops`**: `CreateObjectPayload`, `DeleteObjectPayload`, `SetObjectPropertyPayload`, `DeleteObjectPropertyPayload`.

## 11. Crate Re-exports

For convenience, the crate root re-exports: `Agent`, `AgentId`, `CommandSender`, `ControllerCommand`, `StateController`, `StateControllerBuilder`, `query_state`, `PlazaError`, `InProcessSession`, `MessageTarget`, `Session`, `SessionMessage`, `TargetedOp`, `SnapshotData`, `SnapshotProvider`, `LogicInput`, `StateLogic`, `TickDriver`.
