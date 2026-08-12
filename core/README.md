# `plaza`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The controller loop and the traits you implement around it. This is the crate you start with; [`plaza_session`](../session/), [`plaza_lobby`](../lobby/), and [`plaza_client_utils`](../client_utils/) build on top. `plaza` itself depends on no other crate in the workspace.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md). For the concepts and why they are shaped this way, see the [workspace README](../README.md).

## Install

```toml
[dependencies]
plaza = "0.7"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
```

`plaza` has no feature flags. It needs a Tokio runtime, `async-trait` for the traits you implement, and `serde` on any type that crosses a network.

## What it gives you

| Problem | Piece |
|---|---|
| Several agents acting on one shared state, with no locks in your code | `StateController` / `StateControllerBuilder` |
| The rules, in the one place state is allowed to change | `StateLogic`, `LogicInput`, `LogicOutput` |
| Naming who acted: a person, a bot, the server | `Agent`, `AgentId` |
| Sending a joiner what it should see, per recipient or once for all | `SnapshotProvider`, `SnapshotRequest`, `SnapshotFn` |
| Refusing an op before the rules ever see it | `OpGuard`, `OpClearance`, `GuardFn` |
| Feeding time to the loop, reproducibly | `TickDriver` and `run_fixed` |
| Asking a running controller a question without copying the world | `query_with`, `query_state` |
| Watching the loop's health | `ControllerStats` |
| The whole loop with no sockets, for tests and local play | `InProcessSession` |
| Disconnect grace, timers and meaning left to you | `common::reconnect::ReconnectTracker` |
| Turns, rounds and phases | `game_common::flow_control` |
| The server half of client-side prediction | `game_common::reconciliation` |
| Real sockets | [`plaza_session`](../session/) |

## Status

Experimental. The API changes.
