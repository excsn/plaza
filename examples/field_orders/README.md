# field_orders

Two armies on a board, in the shape Fire Emblem settled: your whole side acts, then theirs. Written for the structure no example had, and the deliverable is what a complex multiplayer turn-based game has to hand-write.

```sh
cargo run -p plaza_example_field_orders                    # the scripted run
cargo run -p plaza_example_field_orders --bin serve        # the browser version, two tabs
```

The game is a fixture and says so: three units a side, one attack, no terrain, because a weapon triangle would drown the flow machinery this exists to show.

## The finding: a command phase is a set, not a sequence

Every turn-based example before this one commands a single actor at a time; the turn *is* the action. A command phase holds three actors at once, the player picks which acts next, each unit marches at most once and strikes at most once, and the phase is over when the **set** of unspent units is empty or the commander ends it.

`flow_control` has no shape for that, and the finding is what it costs to write by hand: [`Activation`](src/types.rs) (`Fresh`, `Moved`, `Done` per unit) plus `maybe_end_phase` in [logic.rs](src/logic.rs), about thirty lines. No cursor-shaped abstraction covers it, because there is no "next unit"; there is a set of things not yet done.

**What that answers about the deferred `TurnOrder`/`TurnPolicy` split.** That entry waited for a third turn order to design a step-policy against. The third consumer arrived and is not an order at all: within a side there is nothing for a cursor to point at. So the honest boundary is that `TurnManager` covers *sequences*, sets are a different primitive, and a policy abstraction stretched over both would be describing neither. The thirty hand-written lines are cheap enough that the set side may never need a block; a second set-shaped game decides.

Note what this example does **not** use: no `TurnManager` (two alternating sides are the phase itself, `Command(Blue)` to `Command(Red)`), and no per-recipient snapshots (a battle is open information, so one uniform view serves the room). Between this, `night_watch` (phases and rounds, no turns) and `draft_board` (turns and rounds, phases as bookkeeping), the module's pieces are demonstrated separable: take what the game needs.

## What it exercises from the rest of the family

- **Unbounded rounds, second consumer.** A battle ends when an army is routed or a commander flees, never on a count. `rounds_are_unbounded` pins it.
- **`PhasedScheduler`, first consumer born after the extraction.** The side deadline is scheduled against `Command(army)`'s occupancy, and a phase ended early leaves its deadline to be dropped inside the scheduler; the example writes no staleness check at all. `the_stale_deadline_does_not_end_the_phase_that_replaced_its_own` puts both deadlines due on the same tick and asserts exactly one phase ends.
- **The `guard` pattern, second wearer.** Seated, right phase, your unit: one auditable function, `night_watch`'s answer to authorization worn by a second game.
- **Sides swap every deployment**, and the assignment is **stored at deployment rather than derived from seat index**, because a test caught the derived version changing the survivor's colours when the loser's departure shifted their index. A forfeit therefore names the right victor, which is also why the victor is a stored field and not a board inference: after a forfeit both armies still stand.

## The lab

Open two tabs at http://127.0.0.1:8095. Click a unit, then a dashed cell to march, an adjacent enemy to strike, or the unit again to hold; End Phase hands the field over, and an idle minute hands it over by itself. Rout the enemy or watch them quit, and the redeploy swaps your colours.

The scripted run compresses the arc: a refused out-of-phase order, a march, a duel across two rounds, a deadline ending an idle phase, a forfeit, and a redeploy with the sides swapped.
