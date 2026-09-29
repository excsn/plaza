# pong

Two-player Pong over real WebSockets, at 60Hz.

```sh
cargo run -p plaza_example_pong
```

Open http://127.0.0.1:8080 in two tabs to play. Or open one and wait: after eight seconds a bot takes the other paddle. It stands down the moment a second person arrives and takes the seat back if one leaves.

The mouse moves your paddle. Space skips a countdown.

## One uniform snapshot per tick

Pong is a state-sync game: the whole world goes to everyone, every tick and the client keeps no history. It used to do this by hand, broadcasting a `GameUpdate` op carrying the entire state alongside a `Snapshot` op carrying the same type on join. Now it is one pass:

```rust,ignore
Ok(LogicOutput::ops(ops).and_snapshot(SnapshotRequest::uniform(state.everyone())))
```

One provider call, one encode and a refcounted frame per recipient. That needs a roster, since `SnapshotRequest` names its recipients where `MessageTarget::All` did not, which is why the state now tracks who is connected: spectators included, because they are watching the same world.

Clients are sent `PongSnapshotPayload`, not the state itself. The state carries a roster and a physics clock that only the server needs. A type used for both tends to collect a `#[serde(skip)]` for every field that should not be in the payload.

## Server-timed phases

Every timed phase counts down on the server: three seconds before a game, one and a half after a point, five on the final score before the next match begins. `ReadyToPlay` still exists and now cuts a countdown short. It used to be the only thing that ended one.

The phases used to advance only when a client sent `ReadyToPlay`, so a browser that did not answer left the game stopped for everyone and `GameOver` was the end of the session rather than the end of a match. Scores are cleared when a new game starts, not when the old one ends, so a match that finished 5-3 does not begin the next one still holding both numbers.

## Two smaller fixes

**Seats are decided every tick.** Seating on join meant a joiner skipped the countdown entirely and walked into a game holding the previous one's scores and a freed seat was never taken by anyone already waiting. One rule now covers arrival, departure and a bot standing aside for a person: whoever has waited longest gets the seat and a person outranks a bot.

**Physics uses the tick's own interval.** It used to measure wall-clock time since the last input *of any kind*, so a client sending paddle ops between ticks left the tick almost no elapsed time to integrate. A bot playing at 40Hz brought the ball to a crawl. Nobody noticed before because a mouse produces events only sporadically.
