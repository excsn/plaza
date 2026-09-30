# draft_board

A snake draft, written to test whether `TurnManager` fits a second turn order or only describes the one type that implements it.

```sh
cargo run -p plaza_example_draft_board                    # the scripted run
cargo run -p plaza_example_draft_board --bin serve        # the browser version
```

## Background

`RoundRobinTurnManager` had been the trait's only implementation since it was written. With one implementor there was no way to tell whether the trait fit other turn orders or only round-robin. Nothing in the workspace had tried to write a second, so this example writes one and reports what happened.

[`SnakeTurnManager`](src/snake.rs) runs down the roster and then back along it, so with three drafters the order is `1,2,3` then `3,2,1` then `1,2,3`. Real drafts use this order because the drafter who picks last in one round picks first in the next.

## What the second implementation found

**The advance fit.** At a reversal `end_current_turn_and_advance` returns the same actor, because the drafter closing one pass opens the next. The contract allows this: it promises the next turn and does not require a different actor. A wrapping manager cannot express that boundary, so a draft needs its own manager.

**The rest of the lifecycle was not on the trait.** The trait held `current_turn_actor` and the advance while every consumer called five methods: `begin`, `restart`, `add_actor` and `remove_actor` were inherent on `RoundRobinTurnManager` alone. A conforming manager could be written that no application could seat, restart or change the roster of. The trait now carries all six and `it_is_usable_behind_the_trait_it_implements` seats, advances and mutates the roster entirely through `dyn TurnManager`, which it could not do when it was first written.

**A pass boundary was invisible from the return value.** Round-robin hides this problem: its actor changes at the wrap, so a caller can infer the boundary. Under a snake the actor stays the same there, so the same check misses the boundary. The advance now returns [`Advanced::PassClosed`](../../core/API_REFERENCE.md) at a boundary. This example's own pick counter was deleted when that landed.

**What still differs.** `remove_actor` at the end of the roster wraps to the first seat in round-robin and pulls back to the last seat here, since a snake at the end is about to turn around. There is a test for each. The two implementations differ in the advance, in the removal fixup and in what `restart` resets. A single "give me the next index" hook could not carry all of that, so they stay separate until a third turn order exists to design a shared policy against.

## What else is in here

The rest of the example is a small fixture around that finding.

**A public board.** A draft has nothing to hide, so [`BoardSnapshotter`](src/snapshot.rs) builds one view and the controller sends it to everyone. `card_table` does the opposite: it pays one build and one encode per recipient to keep each hand secret. Both use the same trait. Which one you want depends on your game.

**A pick clock on the same `Epoch`-guarded scheduler.** Sit on the clock and the board takes the best remaining prospect for you. A clock fires only if the drafter it names is still on the clock and no pick has been made since it started. Counting turn changes would not work: a drafter holds two turns in a row at a reversal and the turn does not change there. Identity alone is not enough either: a drafter's clock from one pass would fire during their next turn, possibly moments after it began.

**A finished draft racks the board and drafts again.** The standings stay up for `INTERMISSION_TICKS`, then scores zero and a fresh pool is dealt. Every pool is racked from a seed: eleven prospects worth 10 to 120 each, drawn by `XorShift` from `plaza_client_utils::determinism` and shown most valuable first. The seed is the board's own with the draft count mixed in, so two drafts differ. The browser board seeds from the clock and logs it. The scripted run uses a constant and reads its picks from the same rack. `restart` puts the order back at the first seat travelling forwards, since a new draft starts over and does not continue the old snake.

## The lab

Open three tabs at http://127.0.0.1:8093. With fewer, a bot takes an open seat after ten seconds and another ten after that. The bots pick from `BoardView`, the same payload a browser is sent. Each takes the most valuable prospect left. The order strip at the top is drawn in the direction the order is currently running. The arrows flip at the end of every pass and whoever picked last picks again immediately. Stall on the clock to see the board pick for you and let the draft finish to see it rack.

The scripted run shows the same thing in a log and stalls on purpose in the third pass.
