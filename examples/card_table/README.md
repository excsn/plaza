# card_table

Turns, rounds and phases, with hidden information: each player sees their own cards by rank and everyone else's only by count.

Two binaries, one game. The rules, the per-recipient snapshot and the flow control live in the lib; the binaries are two transports onto it.

```sh
cargo run -p plaza_example_card_table                    # the scripted run
cargo run -p plaza_example_card_table --bin serve        # the browser version
```

## The scripted run

Three players over `InProcessSession`, fixed cards, one scenario per round: everyone plays in time, then a player stalls and the table plays for them, then a player disconnects mid-match and the turn order closes over the gap. Deterministic, so what shows through is the plaza wiring rather than the rules.

## The browser version

`--bin serve` hosts on http://127.0.0.1:8081. Open **three tabs**: the table deals once three seats are filled. Click a card on your turn; stall and the turn timeout plays your best card for you.

This is the part a log cannot show. Your tab holds three ranks and three face-down backs per opponent, and the backs are not a rendering choice: [`TableSnapshotter`](src/snapshot.rs) never put those ranks in your frame. Hidden information is visible as an absence, which is the only way to see it.

## Snapshots are not the only thing a client learns from

The per-recipient pass is the expensive one: N recipients means N provider calls and N encodes, which is the price of every player seeing something different. So this game sends one only when the whole view changes, a deal or a resolved trick, and narrates the rest as ops: `CardPlayed`, `TurnChanged`, `PhaseChanged`.

That shapes the client. A page that read `whose_turn` from the snapshot alone would sit out its own turn until the timeout played for it, because no snapshot arrives between one player's card and the next. The page applies the notices to its view instead, and only what is public: a card on the table, one fewer card in a hand, whose turn it is. It never learns a rank it was not sent.

`tag_arena` is the opposite end of this trade: no hidden information at all, so one uniform snapshot goes to everyone every tick and there is nothing to narrate.
