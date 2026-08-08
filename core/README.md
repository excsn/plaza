# `plaza`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The controller loop and the traits you implement around it. This is the crate you start with; [`plaza_session`](../session/), [`plaza_lobby`](../lobby/), and [`plaza_client_utils`](../client_utils/) build on top. `plaza` itself depends on no other crate in the workspace.

For the concepts and why they are shaped this way, see the [workspace README](../README.md). For the full public surface, see [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
```

`plaza` has no feature flags. It needs a Tokio runtime, `async-trait` for the traits you implement, and `serde` on any type that crosses a network.

## The four type parameters

Nearly every type here is generic over the same four, so it is worth naming them once:

| Parameter | What it is | Bounds |
|---|---|---|
| `StateType` | Your shared state | `Debug + Send + Sync + 'static`, plus `Clone` only to call `query_state` |
| `Op` | Your operations | `Clone + Debug + Send + Sync + 'static`, plus serde to cross a network |
| `ID` | Your identifier | anything satisfying `AgentId` |

`AgentId` is blanket-implemented for every `Clone + Debug + Eq + Hash + Send + Sync + Serialize + Deserialize + 'static` type, so `Uuid` and `u64` qualify with no work. `Agent<ID>` wraps an ID and distinguishes `Human`, `Bot`, and `System`. These, along with `SessionMessage`, are defined in the runtime-free [`plaza_wire`](../wire/) and re-exported here, so a browser client (which cannot depend on core) names the same envelope types the server does; the `plaza::` paths below work regardless.

## A complete program

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
enum CounterOp { Increment(i64), Changed(i64) }

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
impl SnapshotProvider<UserId, CounterState, i64> for CounterSnapshotter {
  async fn create_snapshot(
    &self,
    state: &CounterState,
    _target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
    // A snapshot is an op. Box the variant when it carries a whole state view.
    Ok(Some(CounterOp::Snapshot(state.value)))
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let session = InProcessSession::<CounterOp, UserId, i64>::new();

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
    // One message kind: a snapshot arrives as an op like any other.
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

`examples/shared_counter` is this program, runnable:

```sh
cargo run -p plaza-example-shared_counter
```

## Driving time

The controller does not advance time on its own: something has to send it `ProcessTimeStep`. For a fixed rate, that is `TickDriver`:

```rust,ignore
tokio::spawn(TickDriver::from_hz(60).run(tx.clone()));               // a live server
TickDriver::new(Duration::from_millis(16)).run_for(tx, 100).await;   // bounded, for tests
TickDriver::run_virtual(&tx, Duration::from_secs(1), 5).await;       // 5s of game time, at once

// Logic a client predicts: every step is exactly 16 ms, whatever the scheduler did.
tokio::spawn(TickDriver::from_hz(120).run_fixed(tx.clone(), Duration::from_millis(16)));
```

`run` passes the measured elapsed time, so logic that integrates over it stays correct when a tick runs late. That is right for a physics step, a decay, a cooldown.

**`run_fixed` is not an optimisation, and choosing wrong is a real bug.** Measured time means the step size is whatever the host's scheduler delivered: 16 ms, then 17, then 16. A simulation advanced by that is a function of the scheduler as well as of its inputs, so **no client can reproduce it**, and prediction, replay and rollback all rest on reproducing it. `run_fixed` accumulates the elapsed time and spends it as whole steps of exactly the size you asked for, carrying the remainder; after a long stall the world falls behind rather than repaying the debt as a burst. Use it whenever anything predicts this logic.

## Per-recipient views

`create_snapshot` receives the agent a snapshot is *for*, and the controller calls it once per recipient. Returning a different payload per agent is the normal path, not a special case:

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

Return `Ok(None)` to send a recipient nothing, which is how an application declines a particular agent. If nothing you build ever needs catch-up on join, say so once instead: `StateControllerBuilder::without_snapshots(logic, session, state)` supplies the shipped `NoSnapshots` provider and you write no `create_snapshot` at all. And when your provider is a pure function of the state and the recipient, which most are, `SnapshotFn(view)` wraps it without the `async fn` and the `Ok(..)` ceremony.

When the view does not depend on who is asking, a world broadcast in a state-sync game, request it `SnapshotRequest::uniform(everyone)` instead: the provider runs once with `target_agent: None` and every recipient receives that one payload, so the pass costs one build and one encode instead of N. The `None` view goes to all of them, so it must be one anyone may see.

### Asking a running controller a question

`query_state(&tx)` copies the whole state back to you, and is the only thing in the crate that needs `StateType: Clone`. When a field is what you want, ask for the field: the closure runs on the controller's task with the state borrowed, so nothing is copied.

```rust,ignore
let seated = query_with(&tx, |state| state.seated_players().len()).await?;
```

When a change alters what players may see, logic can push fresh views rather than waiting to be asked:

```rust,ignore
Ok(LogicOutput::ops(ops).and_snapshot(SnapshotRequest::to(state.seated_players())))
```

## Shutting down

`run` returns the final state, and commands already queued when `Shutdown` arrives are processed first, so a closing broadcast submitted beforehand is guaranteed to go out:

```rust,ignore
tx.send(ControllerCommand::SubmitSystemOps { /* "server closing" */ }).await?;
tx.send(ControllerCommand::Shutdown).await?;
let final_state = handle.await??;   // persist it, or don't
```

## Optional modules

None of this is required; take what fits. Each is a trait plus at most a ready-made implementation, so anything provided can be swapped.

- **`common::scheduler`**: fires events or runs callbacks on a tick (`u64`) or game-time (`Duration`) axis. `TickEventScheduler`, `TimeEventScheduler`, and the callback equivalents.
- **`common::reconnect`**: `ReconnectTracker`, bookkeeping for disconnect grace periods. Holds no timers; you drive it and decide what expiry means.
- **`common::fsm`**: `StateMachine`, with `OpsQueue` as the minimal context.
- **`common::participants`**: `ParticipantTracker`.
- **`common::math`**: plain `Vec2`/`Vec3`/`Quat` for op payloads.
- **`game_common::reconciliation`**: the server half of client-side prediction. Input sequence tracking, delayed input buffers, and a rewind buffer for lag compensation.
- **`game_common::flow_control`**: turns, rounds, phases, and deferred work that belongs to a phase, with `RoundRobinTurnManager`, `SequentialRoundManager`, `Phased`/`Epoch`, and `PhasedScheduler`.
- **`game_common::scorekeeping`**: `Scorekeeper` and a `HashMap` implementation.
- **`app_common`**: op payload shapes for collaborative apps: locking, presence, ordered collections, object/property CRUD.

## Transports

`InProcessSession` ships here for tests and local play: each client gets its own inbox, and message targeting is resolved server-side exactly as a real transport would. For WebSockets or TCP, add [`plaza_session`](../session/).

Implementing `Session` yourself is four async methods and two stream accessors. Presence is one ordered stream (`PresenceEvent::{Joined, Left}`), deliberately: separate channels let a leave overtake a join, which breaks reconnection.

## Status

Experimental. The API changes.
