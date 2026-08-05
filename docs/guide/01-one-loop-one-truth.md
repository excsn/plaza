# 01. One loop, one truth

The question this chapter answers: where does my game's state live, and what is allowed to change it?

## One actor owns the world

Plaza's core is a single loop: the `StateController` owns your state struct and mutates it from its own task, one input at a time. Ops from players, ticks from the clock, joins and leaves from the transport all funnel into the same queue and are applied in sequence. Because only one task ever touches the state, your game logic needs no locking, no `Arc<Mutex<World>>`, no reasoning about interleaving. If two bids race, they arrive in some order and your rules decide in that order; nothing is ever half-applied.

This is the actor model with the ceremony removed, and the crate keeps a matching discipline about hidden machinery: nothing in `plaza` spawns a task except `TickDriver` and the `controller.run()` you spawn yourself. What runs is what you started.

## Your rules live in one trait

You implement [`StateLogic`](../../core/API_REFERENCE.md): one method, `process_input`, taking a `LogicInput` and returning a `LogicOutput`.

`LogicInput` is the complete list of things that can happen to your world: `AgentOps` (a player did something), `TimeStep` (time passed), `AgentJoined`, `AgentLeft`. `LogicOutput` is the complete list of consequences: ops to send (each with a target: everyone, one player, everyone except the culprit) and snapshot requests. A returned error is logged and the loop continues, because a rejected op is a normal event in a server's life, not a reason to stop the world.

One ordering guarantee is worth knowing early because you will eventually rely on it: **ops are sent before snapshots in the same output**, so a client always sees the event that explains a change before the state that reflects it. "You were eaten" arrives before the board without you on it.

## Who is acting

An `Agent` is an identity and nothing more: `Human(id)`, `Bot(id)`, or `System`. Display names, loadouts, and wallets are application data keyed by the ID in your own state. The payoff of that austerity shows up twice. First, the same agent types compile to wasm, so a browser client speaks about players in the same terms the server does. Second, bots are not a special path: a bot is an agent that submits ops like anyone else, and the well-behaved examples make their bots read the same filtered view a browser receives, because a bot playing from privileged state is not playing the game it appears to be playing. [Chapter 12](12-players-come-and-go.md) covers letting bots fill empty seats.

## Time is an input, not a fact

The controller does not advance time on its own; a [`TickDriver`](../../core/API_REFERENCE.md) feeds it `TimeStep` inputs. It has two modes, and the choice matters more than it looks:

- `run` passes measured elapsed time. Right for physics-free decay, cooldowns, anything where "how long has it been" is the honest question.
- `run_fixed` spends accumulated time as exact whole steps. Required the moment anything predicts, replays, or rolls back, because a simulation advanced by measured deltas is a function of the scheduler as well as of its inputs, and no client can reproduce it.

`run_fixed` also refuses to repay a stall as a freeze: after the process hiccups, it advances a bounded number of steps and lets the world fall behind rather than fast-forwarding in one unplayable burst. The client-side twin of this discipline is `FixedTimestep` in the client crate, and [chapter 20](20-hiding-the-wire.md) shows what happens when the two sides step different quanta (spoiler: four bugs that all looked like network faults).

## Watching it run

The controller exposes live counters through shared atomics rather than a query command, for a reason stated memorably in its docs: a query that travels the same queue it reports on goes blank exactly when it becomes interesting. You cannot ask a stalled thing how stalled it is. Both the mean tick time and the worst tick time are kept, because one slow tick in a thousand is invisible in a mean and is exactly the hitch a player notices.

For questions about your state rather than the loop's health, `query_with` runs a closure inside the controller's task and copies nothing.

## The loop without a network

`InProcessSession` is a complete session that delivers messages in memory, with real per-client inboxes and real targeting. It exists so tests, demos, and local play exercise the identical loop, and it is not a mock: bytes aside, everything downstream of the session boundary behaves as it does in production. When [chapter 32](32-serving-your-game.md) swaps in a WebSocket, your `StateLogic` does not change by a line.

## Ripping it apart

The controller is a prescription. The seam under it is the `ControllerCommand` channel: joins, ops, time steps, snapshot requests, and shutdown are all just commands, and `TickDriver` is nothing but a loop that sends one of them on a schedule. Replace the driver with your own cadence, feed ops from a replay file instead of a session, or drive the whole controller from a test harness one command at a time; the lobby crate itself talks to rooms this way and never links their game types.

## The lab

[shared_counter](../../examples/shared_counter/) is the whole chapter in one file: two in-process clients, one shared value, join, snapshot, op, broadcast, no networking to set up. Then [whack_a_mole](../../examples/whack_a_mole/) and [timed_debuff](../../examples/timed_debuff/) show the scheduler side, timers and expiring effects driven entirely through `TimeStep`, and [ability_cooldowns](../../examples/ability_cooldowns/) advances the tick driver in fixed segments so you can watch ops land at known ticks.
