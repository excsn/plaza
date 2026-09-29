# 01. One loop, one truth

This chapter covers where your game's state lives and what is allowed to change it.

## The controller loop

Plaza's core is a single loop: the `StateController` owns your state struct and mutates it from its own task, one input at a time. Ops from players, ticks from the clock and joins and leaves from the transport all go into the same queue and are applied in sequence. Because only one task ever touches the state, your game logic needs no locking (no `Arc<Mutex<World>>`) and no reasoning about interleaving. If two bids race, your rules handle them in the order they arrive and neither is ever half-applied.

This is the actor model without the ceremony. Nothing in `plaza` spawns a task except `TickDriver` and the `controller.run()` you spawn yourself.

## The `StateLogic` trait

You implement [`StateLogic`](../../core/API_REFERENCE.md): one method, `process_input`, taking a `LogicInput` and returning a `LogicOutput`.

`LogicInput` is the complete list of things that can happen to your world: `AgentOps` (a player did something), `TimeStep` (time passed), `AgentJoined` and `AgentLeft`. `LogicOutput` is the complete list of consequences: ops to send (each with a target: everyone, one player, everyone except the culprit) and snapshot requests. A returned error is logged and the loop continues, because rejecting an op is a normal event for a server and should not stop the loop.

Ops are sent before snapshots in the same output, so a client always sees the event that explains a change before the state that reflects it. "You were eaten" arrives before the board without you on it.

## Who is acting

An `Agent` is an identity and nothing more: `Human(id)`, `Bot(id)` or `System`. Display names, loadouts and wallets are application data keyed by the ID in your own state. This helps in two ways. First, the same agent types compile to wasm, so a browser client refers to players the same way the server does. Second, bots need no special path: a bot is an agent that submits ops like anyone else. The examples make their bots read the same filtered view a browser receives, because a bot that reads privileged state sees things no player can and stops testing the real game. [Chapter 12](12-players-come-and-go.md) covers letting bots fill empty seats.

## Time as an input

The controller does not advance time on its own; a [`TickDriver`](../../core/API_REFERENCE.md) feeds it `TimeStep` inputs. It has two modes:

- `run` passes measured elapsed time. Use it for physics-free decay, cooldowns and anything else where you only need to know how long it has been.
- `run_fixed` spends accumulated time as exact whole steps. It is required as soon as anything predicts, replays or rolls back, because a simulation advanced by measured deltas depends on the scheduler as well as its inputs and no client can reproduce it.

After the process stalls, `run_fixed` advances a bounded number of steps and lets the world fall behind instead of fast-forwarding in one unplayable burst. The client-side equivalent is `FixedTimestep` in the client crate. [Chapter 20](20-hiding-the-wire.md) shows what happens when the two sides step different quanta: four bugs, three of which looked exactly like network faults.

## Watching it run

The controller exposes live counters through shared atomics instead of a query command. A query that travels the same queue it reports on stops answering when that queue stalls, which is when you most need the answer. Both the mean tick time and the worst tick time are kept, because one slow tick in a thousand is invisible in a mean but a player notices it.

For questions about your state rather than the loop's health, `query_with` runs a closure inside the controller's task and copies nothing.

## The local loop

`InProcessSession` is a complete session that delivers messages in memory, with real per-client inboxes and real targeting. Tests, demos and local play use it to run the same loop as production. It is not a mock: apart from the bytes, everything downstream of the session boundary behaves as it does in production. When [chapter 32](32-serving-your-game.md) swaps in a WebSocket, your `StateLogic` does not change at all.

## Replacing it

The controller is a prescription. The seam under it is the `ControllerCommand` channel: joins, ops, time steps, snapshot requests and shutdown are all commands and `TickDriver` is just a loop that sends one of them on a schedule. You can replace the driver with your own cadence, feed ops from a replay file instead of a session or drive the whole controller from a test harness one command at a time. The lobby crate talks to rooms this way and never links their game types.

## The lab

[shared_counter](../../examples/shared_counter/) covers this chapter in one file: two in-process clients, one shared value, join, snapshot, op and broadcast, with no networking to set up. Then [whack_a_mole](../../examples/whack_a_mole/) and [timed_debuff](../../examples/timed_debuff/) show the scheduler side, with timers and expiring effects driven entirely through `TimeStep`. [ability_cooldowns](../../examples/ability_cooldowns/) advances the tick driver in fixed segments so you can watch ops land at known ticks.
