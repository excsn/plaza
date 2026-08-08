# draft_board

A snake draft, written to answer one question: is `TurnManager` a seam, or a description of the one thing that implements it?

```sh
cargo run -p plaza_example_draft_board                    # the scripted run
cargo run -p plaza_example_draft_board --bin serve        # the browser version, three tabs
```

## The question, and why it was open

`RoundRobinTurnManager` had been the trait's only implementation since it was written. A public trait with one implementor tells you nothing: the trait might genuinely describe a category, or it might describe its one member and no one had noticed. Nothing in the workspace had ever tried to write a second, so this example writes one and reports what happened.

[`SnakeTurnManager`](src/snake.rs) runs down the roster and then back along it, so with three drafters the order is `1,2,3` then `3,2,1` then `1,2,3`. That is the order every real draft uses, and picking last is compensated by picking first next round.

## What the trait carried, and what it did not

**Both trait methods carried it, including the part that looks illegal.** `current_turn_actor` is a lookup. `end_current_turn_and_advance` returns the **same actor** at a reversal, because the drafter closing one pass opens the next, and the trait's contract permits that: it promises the next actor rather than a different one. A wrapping manager cannot express that boundary at all, which is the whole reason a draft needs its own.

**What did not carry is everything around them.** `begin`, `restart`, `add_actor` and `remove_actor` are inherent on `RoundRobinTurnManager` and absent from the trait. This manager had to declare its own by hand, and nothing checks that the two agree. So the honest answer to the question is **half a seam**: a caller holding `dyn TurnManager` can read whose turn it is and advance it, and cannot seat the first actor, restart the order, or remove somebody who left. Those are three of the five operations every existing consumer calls.

`it_is_usable_behind_the_trait_it_implements` is the test that shows the gap rather than describing it: it has to reach past the trait to a concrete `begin` before it can use the trait at all.

**And a pass boundary is invisible from the trait's return value.** A round-robin caller can watch for the actor coming back around. Under a snake the actor is *unchanged* across the boundary, so "did it change" reports the exact opposite of the truth where it matters most. The application counts picks instead, and `SnakeTurnManager::in_pass` exists because the trait has no way to say it.

## What else is in here

The rest is a fixture around that finding, deliberately small.

**A public board, which is the contrast with `card_table` worth noticing.** A draft has nothing to hide, so [`BoardSnapshotter`](src/snapshot.rs) builds one view and the controller sends it to everyone. `card_table` is the opposite case and pays one build and one encode per recipient to keep a hand secret. Both are the same trait; which one you want is a property of your game and not of plaza.

**A pick clock on the same `Epoch`-guarded scheduler.** Sit on the clock and the board takes the best remaining prospect for you. The stale-token check earns its keep twice over here: a drafter legitimately holds two turns in a row at a reversal, so a generation counter would call the second one stale and a plain identity check is what works.

**A finished draft racks the board and drafts again.** The standings stay up for `INTERMISSION_TICKS`, then scores zero and a fresh pool is dealt. `restart` puts the order back at the top travelling forwards rather than continuing the snake, because a new draft is not the next pass of the old one.

## The lab

Open three tabs at http://127.0.0.1:8093. The order strip at the top draws itself in the direction it is currently running, so the reversal is a thing you watch happen rather than a claim: the arrows flip at the end of every pass, and whoever picked last picks again immediately. Stall on the clock to see the board pick for you, and let the draft finish to see it rack.

The scripted run makes the same point in a log, and stalls on purpose in the third pass.
