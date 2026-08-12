# Usage Guide: plaza

How to build a shared-state application on `plaza`: writing the rules, standing up a controller, sending a joiner what it should see, driving time, authorizing ops, asking a running controller a question, and shutting it down.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [A Complete Program](#a-complete-program)
    *   [The Four Type Parameters](#the-four-type-parameters)
*   [Writing the Rules](#writing-the-rules)
    *   [Handling Ops](#handling-ops)
    *   [Reacting to Joins and Departures](#reacting-to-joins-and-departures)
    *   [Returning Ops to Specific Agents](#returning-ops-to-specific-agents)
*   [Standing Up a Controller](#standing-up-a-controller)
    *   [Building It](#building-it)
    *   [Running It](#running-it)
*   [Sending a Joiner What It Should See](#sending-a-joiner-what-it-should-see)
    *   [A View per Recipient](#a-view-per-recipient)
    *   [One View for Everyone](#one-view-for-everyone)
    *   [Declining to Send Anything](#declining-to-send-anything)
    *   [Pushing Fresh Views](#pushing-fresh-views)
*   [Driving Time](#driving-time)
    *   [Measured Time](#measured-time)
    *   [Fixed Steps](#fixed-steps)
    *   [Virtual Time for Tests](#virtual-time-for-tests)
*   [Authorizing Ops](#authorizing-ops)
    *   [A Guard as a Function](#a-guard-as-a-function)
    *   [A Guard With State](#a-guard-with-state)
*   [Asking a Running Controller a Question](#asking-a-running-controller-a-question)
*   [Shutting Down](#shutting-down)
*   [Choosing a Transport](#choosing-a-transport)
*   [Optional Modules](#optional-modules)
*   [Error Handling](#error-handling)

## Core Concepts

*   **`StateType`**: your shared state. One instance, owned by the controller, mutated from nowhere else.
*   **`Op`**: your operations. What a client submits and what the server sends back, in one enum.
*   **`Agent<ID>`**: who acted, as `Human`, `Bot` or `System`. `ID` is your own identifier type.
*   **`StateLogic`**: the rules. The only place state changes, and the only thing you must write.
*   **`LogicInput`**: what reaches the rules: `AgentOps`, `AgentJoined`, `AgentLeft`, `TimeStep`.
*   **`LogicOutput`**: what the rules return: ops to send, and optionally a request for fresh snapshots.
*   **`TargetedOp`**: one or more ops plus who they go to. `new_system_all`, `new_system_to`, and the rest.
*   **`SnapshotProvider`**: what a joining or refreshing agent is sent. Called once per recipient.
*   **`OpGuard`**: may this agent do this at all, judged before the rules run, with the state read-only.
*   **`StateController`**: owns the state and processes one input at a time on its own task. No locks anywhere in your logic.
*   **`CommandSender`**: the handle everything else holds. Submits ops, drives time, asks questions, orders a shutdown.
*   **`Session`**: the transport. `InProcessSession` ships here; real sockets live in `plaza_session`.
*   **`TickDriver`**: what sends the controller a time step, at a rate you choose.

## Quick Start

### A Complete Program

A counter, over the in-process transport. `examples/shared_counter` is this program, runnable with `cargo run -p plaza-example-shared_counter`.

```rust,ignore
use plaza::{
  agent::Agent,
  controller::{query_state, StateControllerBuilder},
  session::{InProcessSession, SessionMessage, TargetedOp},
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
  state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type UserId = u64;

#[derive(Clone, Debug, Default)]
struct CounterState { value: i64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterOp { Increment(i64), Changed(i64), Snapshot(i64) }

// The rules. The only place state is mutated.
#[derive(Debug, Default)]
struct CounterLogic;

#[async_trait]
impl StateLogic<CounterOp, UserId, CounterState> for CounterLogic {
  async fn process_input(
    &self,
    state: &mut CounterState,
    input: LogicInput<CounterOp, UserId>,
  ) -> Result<LogicOutput<CounterOp, UserId>, StateLogicError> {
    let mut ops = Vec::new();

    if let LogicInput::AgentOps { ops: incoming, .. } = input {
      for op in incoming {
        if let CounterOp::Increment(by) = op {
          state.value += by;
          ops.push(TargetedOp::new_system_all(vec![CounterOp::Changed(state.value)]));
        }
      }
    }

    Ok(ops.into())
  }
}

// What a joining client is sent.
#[derive(Debug, Default)]
struct CounterSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, CounterState, CounterOp> for CounterSnapshotter {
  async fn create_snapshot(
    &self,
    state: &CounterState,
    _target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
    Ok(Some(CounterOp::Snapshot(state.value)))
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let session = InProcessSession::<CounterOp, UserId>::new();

  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(controller.run());

  // Connecting yields an inbox; the join snapshot arrives on it.
  let alice = Agent::new_human(1u64);
  let (_conn_id, inbox) = session.connect(alice.clone()).await?;

  session.client_send(alice, vec![CounterOp::Increment(5)]).await;

  while let Ok(msg) = inbox.recv().await {
    for op in &msg.ops {
      println!("op: {op:?}");
    }
    if query_state(&tx).await?.value == 5 {
      break;
    }
  }

  Ok(())
}
```

### The Four Type Parameters

Nearly every type here is generic over the same few.

| Parameter | What it is | Bounds |
|---|---|---|
| `StateType` | Your shared state | `Debug + Send + Sync + 'static`, plus `Clone` only to call `query_state` |
| `Op` | Your operations | `Clone + Debug + Send + Sync + 'static`, plus serde to cross a network |
| `ID` | Your identifier | anything satisfying `AgentId` |

`AgentId` is blanket-implemented for every `Clone + Debug + Eq + Hash + Send + Sync + Serialize + Deserialize + 'static` type, so `Uuid` and `u64` qualify with no work.

`Agent<ID>`, `AgentId` and `SessionMessage` are defined in the runtime-free [`plaza_wire`](../wire/) and re-exported here, so a browser client that cannot depend on core names the same envelope types the server does. The `plaza::` paths work regardless.

## Writing the Rules

### Handling Ops

```rust,ignore
async fn process_input(
  &self,
  state: &mut GameState,
  input: LogicInput<GameOp, PlayerId>,
) -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError> {
  let LogicInput::AgentOps { source, ops } = input else { return Ok(LogicOutput::default()) };
  let Some(player) = source.id_cloned() else {
    return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
  };

  let mut out = Vec::new();
  for op in ops {
    match op {
      GameOp::Move { to } => {
        state.place(player, to);
        out.push(TargetedOp::new_system_all(vec![GameOp::Moved { player, to }]));
      }
      _ => {}
    }
  }
  Ok(out.into())
}
```

### Reacting to Joins and Departures

```rust,ignore
match input {
  LogicInput::AgentJoined { agent } => {
    let seat = state.seat(agent.id_cloned().unwrap());
    Ok(vec![TargetedOp::new_system_all(vec![GameOp::Seated { seat }])].into())
  }
  LogicInput::AgentLeft { agent_id } => {
    state.free_seat(&agent_id);
    Ok(LogicOutput::default())
  }
  LogicInput::TimeStep { delta_time } => {
    state.advance(delta_time);
    Ok(LogicOutput::default())
  }
  _ => Ok(LogicOutput::default()),
}
```

An op that arrives from an agent whose seat has already gone is normal, not a fault: a packet crossed a departure. Drop it rather than erroring, or a race the network guarantees kills connections.

### Returning Ops to Specific Agents

```rust,ignore
TargetedOp::new_system_all(ops)              // everyone
TargetedOp::new_system_to(player, ops)       // one agent
TargetedOp::new_system_to_many(players, ops) // several, delivered once each
```

## Standing Up a Controller

### Building It

```rust,ignore
let (tx, controller) = StateControllerBuilder::new(
  Arc::new(MyLogic),
  session.clone(),
  Arc::new(MySnapshotter),
  MyState::default(),
)
.command_buffer(256)
.guard(Arc::new(GuardFn(screen)))
.build();
```

If nothing you build ever needs catch-up on join, say so once and write no `create_snapshot` at all:

```rust,ignore
let (tx, controller) = StateControllerBuilder::without_snapshots(logic, session, state).build();
```

### Running It

```rust,ignore
let handle = tokio::spawn(controller.run());
```

The controller owns the state and processes one input at a time on its own task, so your logic needs no locking. Nothing in this crate spawns a task except `TickDriver` and this call.

## Sending a Joiner What It Should See

### A View per Recipient

`create_snapshot` receives the agent the snapshot is *for*, and the controller calls it once per recipient. A different payload per agent is the normal path, not a special case.

```rust,ignore
let me = target.and_then(|a| a.id());
Ok(Some(GameOp::Snapshot(Box::new(GameView {
  my_hand: me.and_then(|id| state.hands.get(id)).cloned().unwrap_or_default(),
  opponent_hand_sizes: state.hands.iter()
    .filter(|(id, _)| Some(*id) != me)
    .map(|(id, h)| (id.clone(), h.len()))
    .collect(),
}))))
```

When the provider is a pure function of the state and the recipient, which most are, skip the `async fn` and the `Ok(..)` ceremony:

```rust,ignore
.snapshot_provider(Arc::new(SnapshotFn(view)))
```

### One View for Everyone

When the view does not depend on who is asking, a world broadcast in a state-sync game, request it uniform. The provider runs once with `target_agent: None` and every recipient receives that one payload, so the pass costs one build and one encode instead of N.

```rust,ignore
SnapshotRequest::uniform(everyone)
```

The `None` view goes to all of them, so it must be one anyone may see.

### Declining to Send Anything

```rust,ignore
Ok(None)
```

### Pushing Fresh Views

When a change alters what players may see, logic can push rather than wait to be asked:

```rust,ignore
Ok(LogicOutput::ops(ops).and_snapshot(SnapshotRequest::to(state.seated_players())))
```

## Driving Time

The controller does not advance time on its own: something has to send it `ProcessTimeStep`.

### Measured Time

```rust,ignore
tokio::spawn(TickDriver::from_hz(60).run(tx.clone()));
```

`run` passes the measured elapsed time, so logic that integrates over it stays correct when a tick runs late. Right for a physics step, a decay, a cooldown.

### Fixed Steps

```rust,ignore
tokio::spawn(TickDriver::from_hz(120).run_fixed(tx.clone(), Duration::from_millis(16)));
```

**Use this whenever anything predicts, replays or rolls back this logic.** Measured time means the step size is whatever the host's scheduler delivered: 16 ms, then 17, then 16. A simulation advanced by that is a function of the scheduler as well as of its inputs, so no client can reproduce it. `run_fixed` accumulates elapsed time and spends it as whole steps of exactly the size you asked for, carrying the remainder; after a long stall the world falls behind rather than repaying the debt as a burst.

### Virtual Time for Tests

```rust,ignore
TickDriver::new(Duration::from_millis(16)).run_for(tx, 100).await;   // bounded
TickDriver::run_virtual(&tx, Duration::from_secs(1), 5).await;       // 5s of game time, at once
```

## Authorizing Ops

"May this agent do this at all" is authorization, not rules. Mixing it into `StateLogic` smears security checks through the handlers; an `OpGuard` is the one auditable place for it. The controller runs it per op, ahead of `process_input`, with the state read-only, and a refused op never reaches the rules.

### A Guard as a Function

```rust,ignore
use plaza::op_guard::{GuardFn, OpClearance};

fn screen(state: &Game, source: &Agent<PlayerId>, op: &GameOp) -> OpClearance<GameOp> {
  match op {
    GameOp::Play(_) if !state.seated(source) => OpClearance::Refused {
      reply: Some(GameOp::Refused(Refusal::Spectating)),
    },
    _ => OpClearance::Cleared,
  }
}

let (tx, controller) = StateControllerBuilder::new(logic, session, snapshotter, state)
  .guard(Arc::new(GuardFn(screen)))
  .build();
```

The reply, if any, goes back to the source as a system op, so a client can say what happened instead of appearing to freeze. `ControllerStats::ops_refused` counts every refusal.

System submissions and time steps are never screened. The guard judges the actor's standing rather than the act's content: whether this player may vote in this phase is the guard's, whether their target exists stays in the rules. It is sync on purpose, since it runs per op on the controller's task, so a permission that lives in a database belongs loaded into state rather than fetched mid-stream.

### A Guard With State

Anything stateful implements `OpGuard` directly, as `examples/night_watch`'s `VillageGuard` does for seat, liveness, phase and role.

```rust,ignore
impl OpGuard<GameOp, PlayerId, Game> for VillageGuard {
  fn clear(&self, state: &Game, source: &Agent<PlayerId>, op: &GameOp) -> OpClearance<GameOp> {
    // ...
  }
}
```

The default is `NoGuard`, which admits everything.

## Asking a Running Controller a Question

```rust,ignore
let whole = query_state(&tx).await?;   // the only thing needing StateType: Clone
```

When a field is what you want, ask for the field. The closure runs on the controller's task with the state borrowed, so nothing is copied.

```rust,ignore
let seated = query_with(&tx, |state| state.seated_players().len()).await?;
```

## Shutting Down

`run` returns the final state, and commands already queued when `Shutdown` arrives are processed first, so a closing broadcast submitted beforehand is guaranteed to go out.

```rust,ignore
tx.send(ControllerCommand::SubmitSystemOps { /* "server closing" */ }).await?;
tx.send(ControllerCommand::Shutdown).await?;
let final_state = handle.await??;
```

## Choosing a Transport

`InProcessSession` ships here for tests and local play: each client gets its own inbox, and message targeting is resolved server-side exactly as a real transport would.

```rust,ignore
let session = InProcessSession::<Op, PlayerId>::new();
let (conn_id, inbox) = session.connect(agent).await?;
session.client_send(agent, vec![op]).await;
```

For WebSockets or TCP, add [`plaza_session`](../session/). Implementing `Session` yourself is four async methods and two stream accessors.

Presence is one ordered stream (`PresenceEvent::{Joined, Left}`) deliberately: separate channels let a leave overtake a join, which breaks reconnection.

## Optional Modules

None of this is required; take what fits. Each is a trait plus at most a ready-made implementation, so anything provided can be swapped.

| Module | What it holds |
|---|---|
| `common::scheduler` | Events or callbacks on a tick (`u64`) or game-time (`Duration`) axis |
| `common::reconnect` | `ReconnectTracker`: disconnect grace bookkeeping, no timers, expiry means what you say |
| `common::closure` | `ClosureLog`: the closes this host ordered, so an ordered close is told apart from a netdrop |
| `common::fsm` | `StateMachine`, with `OpsQueue` as the minimal context |
| `common::participants` | `ParticipantTracker` |
| `common::math` | Plain `Vec2`/`Vec3`/`Quat` for op payloads |
| `game_common::reconciliation` | The server half of client-side prediction: sequence tracking, delayed input buffers, a rewind buffer |
| `game_common::flow_control` | Turns, rounds, phases, and deferred work belonging to a phase |
| `game_common::scorekeeping` | `Scorekeeper` and a `HashMap` implementation |
| `app_common` | Op payload shapes for collaborative apps: locking, presence, ordered collections, object CRUD |

## Error Handling

`PlazaError<ID>` is the top of the tree; the rest nest under it and each carries the id it concerns.

```rust,ignore
match query_state(&tx).await {
  Ok(state) => state,
  Err(QueryError::ControllerGone) => return,
  Err(e) => return eprintln!("{e}"),
}
```

*   **`StateLogicError`**: what your rules return. `InvalidOperation` for an op that cannot be honoured, and the variants around it.
*   **`SnapshotError<ID>`**: what a provider returns when it cannot build a view for an agent.
*   **`SessionError<ID>`**: transport-level failure, including whatever a real transport wraps.
*   **`QueryError`**: `query_state` and `query_with` when the controller has gone or the reply was dropped.

Returning `Err` from `process_input` is logged and does not stop the controller: one bad op must not take the room down with it. Reserve it for an op that genuinely cannot be honoured, and prefer answering the offender with an op of your own.
