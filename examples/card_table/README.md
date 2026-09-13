# card_table

Turns, rounds and phases, with hidden information: each player sees their own cards by rank and everyone else's only by count.

The example has two binaries that run the same game. The rules, the per-recipient snapshot and the flow control live in the lib; the two binaries run it over different transports.

```sh
cargo run -p plaza_example_card_table                    # the scripted run
cargo run -p plaza_example_card_table --bin serve        # the browser version
```

## The scripted run

Three players over `InProcessSession`, fixed cards, one scenario per round: everyone plays in time, then a player stalls and the table plays for them, then a player disconnects mid-match and the turn order closes over the gap. It is deterministic, so the log shows the plaza wiring and not the rules.

## The browser version

`--bin serve` hosts on http://127.0.0.1:8081. The table deals once three seats are filled, so open **three tabs**, or open one and wait: a bot takes an open seat after ten seconds and another ten after that. Click a card on your turn; stall and the turn timeout plays your best card for you.

The bots play from `player_view`, the same payload a browser is sent. A bot reading `TableState` would hold every hand at the table, which the example says a client cannot do.

The turn timeout is a field on the state rather than a constant, because the two binaries want different answers: the scripted run wants one short enough to reach on purpose in a few seconds and a person choosing a card wants one long enough to choose in.

**After a match ends, the table deals again.** The standings stay up for `INTERMISSION_TICKS`, then the table zeroes the scores and deals again, so nobody reloads to play a second match. It is scheduled through the same `TickEventScheduler` as the turn timeout and carries the same epoch token, so it drops itself if an arriving player fills the table and deals first. It keeps the roster instead of clearing it, which is the difference between `reset_all_scores` and `clear_all_scores`. It does not deal to an emptied table and leaves that to the next arrival.

Your tab holds three ranks and three face-down backs per opponent. The backs are there because [`TableSnapshotter`](src/snapshot.rs) never put those ranks in your frame.

## Ops between snapshots

A per-recipient pass is expensive: N recipients means N provider calls and N encodes. So this game sends one only when the whole view changes (a deal or a resolved trick) and sends everything else as ops: `CardPlayed`, `TurnChanged`, `PhaseChanged`.

The client has to handle this. A page that read `whose_turn` from the snapshot alone would sit out its own turn until the timeout played for it, because no snapshot arrives between one player's card and the next. The page applies the public parts of the notices to its view instead: a card on the table, one fewer card in a hand, whose turn it is. It never learns a rank it was not sent.

`tag_arena` has no hidden information, so one uniform snapshot goes to everyone every tick and no separate ops are needed.
