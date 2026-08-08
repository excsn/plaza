# draft_board

A snake draft, written to answer one question: is `TurnManager` a seam, or a description of the one thing that implements it?

```sh
cargo run -p plaza_example_draft_board                    # the scripted run
cargo run -p plaza_example_draft_board --bin serve        # the browser version, three tabs
```

## The question, and why it was open

`RoundRobinTurnManager` had been the trait's only implementation since it was written. A public trait with one implementor tells you nothing: the trait might genuinely describe a category, or it might describe its one member and no one had noticed. Nothing in the workspace had ever tried to write a second, so this example writes one and reports what happened.

[`SnakeTurnManager`](src/snake.rs) runs down the roster and then back along it, so with three drafters the order is `1,2,3` then `3,2,1` then `1,2,3`. That is the order every real draft uses, and picking last is compensated by picking first next round.

## The answer: half a seam, and it has since been closed

**The advance carried it, including the part that looks illegal.** `end_current_turn_and_advance` returns the **same actor** at a reversal, because the drafter closing one pass opens the next, and the contract permits that: it promises the next turn rather than a different holder of it. A wrapping manager cannot express that boundary at all, which is why a draft needs its own.

**Everything around it did not.** The trait held `current_turn_actor` and the advance while every consumer called five methods: `begin`, `restart`, `add_actor` and `remove_actor` were inherent on `RoundRobinTurnManager` alone. A conforming manager could be written that no application could seat, restart, or change the roster of. The trait now carries all six, and `it_is_usable_behind_the_trait_it_implements` seats, advances, and mutates the roster entirely through `dyn TurnManager`, which it could not do when it was first written.

**And a pass boundary was invisible from the return value.** Round-robin hides this: its actor changes at the wrap, so a caller can infer the boundary. Under a snake the actor is *unchanged* there, so the same inference reports the exact opposite of the truth at the only moment it matters. That is now [`Advanced::PassClosed`](../../core/API_REFERENCE.md), returned by the advance, and this example's dedicated pick counter was deleted when it landed.

**What deliberately still differs, and why it matters.** `remove_actor` at the end of the roster **wraps** in round-robin and **pulls back** here, since a snake at the end is about to turn around rather than start over; there is a test on each. Two implementations differing in the advance, in the removal fixup, and in what `restart` resets is more variation than a single "give me the next index" hook would carry, which is the argument against factoring these into shared machinery plus a policy until a third order exists to design against.

## What else is in here

The rest is a fixture around that finding, deliberately small.

**A public board, which is the contrast with `card_table` worth noticing.** A draft has nothing to hide, so [`BoardSnapshotter`](src/snapshot.rs) builds one view and the controller sends it to everyone. `card_table` is the opposite case and pays one build and one encode per recipient to keep a hand secret. Both are the same trait; which one you want is a property of your game and not of plaza.

**A pick clock on the same `Epoch`-guarded scheduler.** Sit on the clock and the board takes the best remaining prospect for you. The stale-token check earns its keep twice over here: a drafter legitimately holds two turns in a row at a reversal, so a generation counter would call the second one stale and a plain identity check is what works.

**A finished draft racks the board and drafts again.** The standings stay up for `INTERMISSION_TICKS`, then scores zero and a fresh pool is dealt. `restart` puts the order back at the top travelling forwards rather than continuing the snake, because a new draft is not the next pass of the old one.

## The lab

Open three tabs at http://127.0.0.1:8093. The order strip at the top draws itself in the direction it is currently running, so the reversal is a thing you watch happen rather than a claim: the arrows flip at the end of every pass, and whoever picked last picks again immediately. Stall on the clock to see the board pick for you, and let the draft finish to see it rack.

The scripted run makes the same point in a log, and stalls on purpose in the third pass.
