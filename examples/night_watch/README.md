# night_watch

A village with a wolf in it, written for the half of `flow_control` that nothing drives: phases that decide who may act, and rounds with no count.

```sh
cargo run -p plaza_example_night_watch                    # the scripted run
cargo run -p plaza_example_night_watch --bin serve        # the browser version, five tabs
```

Be honest about what this is: social deduction without chat is a thin game, because the genre's interest is in the talking. This is a lab for the phase machine and the secrecy, and it does not pretend a vote-only village is fun.

## The four things this example is for

**A phase that is the rule set, not a stage.** `card_table`'s Dealing to Playing to Scoring is one flow in stages; every player may do the same things throughout. Here the phase decides *who may act at all*: at night one role may `Hunt` and nobody may `Vote`, by day it is exactly reversed. Same `Phased` block, a different and heavier job.

**Rounds with no count.** `SequentialRoundManager::new(None, ..)` has been documented and unit-tested since it was written, and this is its first consumer. The game ends when the wolf is exiled or when the wolf reaches parity, never on a round number, and `the_game_ends_on_a_condition_not_a_count` pins the shape: the manager never says stop, the game does.

**Collect, then resolve.** Everything else in this repository resolves input as it arrives or strictly one actor at a time. A day's ballots are collected and *nothing happens*: resubmitting overwrites, who has voted is public, who they chose is not, and dusk resolves the lot at once. Dusk falls early when every living player has voted, which sets up the fourth thing:

**A deadline that discovers it is stale.** The day's deadline is scheduled when the day begins. When dusk falls early, nothing cancels it; the phase moves, the epoch it carries stops matching, and it fires into the night as a no-op. `the_stale_day_deadline_does_not_fire_into_the_night` makes the night long and the day short so the deadline genuinely comes due in the wrong phase, and asserts it tallies nothing.

## The secrecy, which is the game

Every snapshot is per recipient. `your_role` is yours alone, the wolf's night choice never crosses the wire before dawn, and a death reveals the fallen player's role to everyone. The inversion worth the trip: **the dead see everything.** A killed player's next snapshot carries every role, face up, because the dead know and can no longer be asked. A uniform snapshot could not carry this game at all, and the wire discipline is `pellet_maze`'s lesson: the tally broadcasts counts, never ballots, because secrecy is a property of the whole outbound stream.

## The authorization, in the seam built for it

`VillageGuard` in [guard.rs](src/guard.rs) is "may this player do this": seated, alive, right phase, right role, one auditable `OpGuard` the controller runs ahead of `StateLogic`, so every op the handlers see already has standing. A refusal answers the sender with its reason (`Refused(NotYourRole)` and kin) and never reaches the rules. The line it draws is standing versus content: whether the wolf may hunt tonight is the guard's, whether the named victim is dead, absent, or yourself stays in [logic.rs](src/logic.rs). This example wrote the same function inside `StateLogic` first, for want of anywhere else, and was the consumer the hook was extracted for.

## The lab

Open five tabs at http://127.0.0.1:8094. One tab learns it is the wolf; the others learn only their own role, and nothing in any tab's traffic says more, which you can verify from the network panel. Get killed and watch your own tab flip to seeing every role. Let a day time out to watch abstainers counted; vote fast to watch dusk fall early. When a side wins, the reveal stays up, then the village deals again with the wolf one seat along.

The scripted run compresses the same arc: a refused hunt, a dawn, an early dusk, an overslept wolf, parity, the reveal, and the second deal.
