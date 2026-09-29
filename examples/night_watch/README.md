# night_watch

A village with a wolf in it, written to exercise two parts of `flow_control` that no example had used before it: phases that decide who may act and rounds with no count.

```sh
cargo run -p plaza_example_night_watch                    # the scripted run
cargo run -p plaza_example_night_watch --bin serve        # the browser version, five tabs
```

Social deduction without chat is a thin game, because most of the genre's interest is in the talking. This example is a lab for the phase machine and the secrecy. A vote-only village is not much fun to play.

## What this example covers

**A phase that decides who may act.** `card_table`'s Dealing to Playing to Scoring is one flow in stages; every player may do the same things throughout. Here the phase decides who may act: at night one role may `Hunt` and nobody may `Vote`. By day it is the reverse. Both examples use the same `Phased` block.

**Rounds with no count.** `SequentialRoundManager::new(None, ..)` had been documented and unit-tested since it was written, but this example was its first consumer. The game ends when the wolf is exiled or reaches parity, never on a round number. `the_game_ends_on_a_condition_not_a_count` tests that the round manager never ends the game itself.

**Collect, then resolve.** Most examples in this repository resolve input as it arrives or one actor at a time. Here a day's ballots are collected and nothing happens until dusk, which resolves them all at once. Resubmitting overwrites. Who has voted is public and who they chose is not. Dusk falls early when every living player has voted, which leads to the fourth point:

**A deadline that goes stale.** The day's deadline is scheduled when the day begins. When dusk falls early nothing cancels it. The phase moves, the epoch it carries stops matching and it fires into the night as a no-op. `the_stale_day_deadline_does_not_fire_into_the_night` makes the night long and the day short so the deadline comes due in the wrong phase, then asserts it tallies nothing.

## Secrecy

Every snapshot is per recipient. Only you see `your_role`, the wolf's night choice never crosses the wire before dawn and a death reveals the fallen player's role to everyone. The dead are the exception: a killed player's next snapshot carries every role, face up, since a dead player no longer takes part. A uniform snapshot could not carry this game. The rule for the wire comes from `pellet_maze`: secrecy covers the whole outbound stream, so the tally broadcasts counts and never individual ballots.

## Authorization

`VillageGuard` in [guard.rs](src/guard.rs) checks whether a player may act at all: seated, alive, right phase, right role. It is one `OpGuard` the controller runs ahead of `StateLogic`, so every op the handlers see has already passed those checks. A refusal answers the sender with its reason (`Refused(NotYourRole)` and similar) and never reaches the rules. The guard checks only the sender's standing. Whether the wolf may hunt tonight is the guard's job. Whether the named victim is dead, absent or yourself is checked in [logic.rs](src/logic.rs). This example first wrote the same check inside `StateLogic` because there was nowhere else to put it. The `OpGuard` hook was extracted from that code.

## The lab

Open five tabs at http://127.0.0.1:8094. One tab learns it is the wolf. The others learn only their own role and nothing in any tab's traffic says more, which you can check in the network panel. Get killed and your tab switches to showing every role. Let a day time out to see abstainers counted or vote fast to see dusk fall early. When a side wins, the reveal stays up, then the village deals again with the wolf one seat along.

The scripted run covers the same sequence: a refused hunt, a dawn, an early dusk, an overslept wolf, parity, the reveal and the second deal.
