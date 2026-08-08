# field_orders

A Fire Emblem-shaped battle, playable from two commanders to thirty-two: two armies on terrain, your whole side acts and then theirs, each unit marches once and acts once, and every blow at matched reach is answered. One crate builds the authoritative server, the native desktop client, and the browser client, all from the same `protocol.rs`.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh -- --role client --connect ws://host:8095/ws
./wasm-serve.sh                          # build the browser client, host it on :8095
cargo run -p plaza_example_field_orders --bin scripted   # the in-process scripted arc
```

Arrivals gather in a lobby, and nothing counts down by itself: the first-mustered commander is the **host**, picks the field size (or leaves it on auto), and starts the countdown, like any real game. The deploy takes the larger of the pick and what the muster needs, so a pair may duel on the Xlarge field but nine squads can never squeeze onto Small; settings lock once the countdown runs, and a leaving host hands the lobby to the next in line. After a battle the field returns to the lobby for the host to restart. The bot takes one squad to even an odd side, which is also how a lone commander gets an opponent. Lose and the next deploy opens you on the other colour.

## The game

- **Squads.** Every commander owns four units: Knight (8 hp, move 3, hits for 3 at reach 1), Soldier (6 hp, move 4, hits for 2 at reach 1), Archer (5 hp, move 4, hits for 2 at reach **exactly** 2), and Healer (5 hp, move 4, mends 2 at reach 1, carries no weapon at all). You command your squad alone; your teammates' units are allies you cannot spend.
- **Maps that fit the muster.** Small (10x7) for 1v1, Medium (16x11) to 2v2, Large (26x19) to 4v4, and Xlarge (48x34) up to 16v16: thirty-two commanders, one hundred and twenty-eight units, one activation set. The small board is authored; the larger three are patterned deterministically from their coordinates, with the deploy bands kept clear.
- **Terrain.** Plains cost 1 to enter; forests cost 2 and blunt a blow landing there by 1; rocks are walls.
- **Counterstrikes.** A struck survivor answers, but only from its own reach and only armed: the archer pelting from two cells is unanswerable by a soldier, unanswerable *at* one cell when the soldier closes in, and a healer answers nothing. Nothing answers a bandage either.
- **Movement is priced, not radial.** Reach is a Dijkstra flood over terrain costs; enemies block, allies can be passed through but not stood on.

## Where the rules live, which is the example's networking claim

The server computes what every unit may still do and ships it in the view as [`UnitOrders`](src/protocol.rs): the cells a march may end on, the enemies a strike lands on. The desktop client, the wasm client, and the bot all pick from those options and none of them re-derives a rule; a click the server would refuse is simply never offered. `protocol.rs` is compiled into every build, so the shared vocabulary is the same code, not a mirror; plaza core builds on wasm32, so even the flow_control notice payloads cross the wire as plaza's own types.

The wire is MessagePack with a protocol version derived from `protocol.rs` at build time (`build.rs`): the wasm bundle is a build product that goes stale, and the handshake is what tells it to reload rather than silently misdecoding.

Presentation is derived, never authoritative: the client drains a `Moment` stream (phase changes, blows, the verdict) into floating announcements, damage pops, hit flashes and eased health bars, and the phase countdown runs off the `duration_hint` the server put in the phase notice, including the redeploy countdown on the result screen. None of it feeds back into an order.

## The finding: a command phase is a set, not a sequence

Every turn-based example before this one commands a single actor at a time; the turn *is* the action. A command phase holds four actors at once, the player picks which acts next, each unit marches at most once and strikes at most once, and the phase is over when the **set** of unspent units is empty or the commander ends it.

`flow_control` has no shape for that, and the finding is what it costs to write by hand: [`Activation`](src/protocol.rs) (`Fresh`, `Moved`, `Done` per unit) plus `maybe_end_phase` in [logic.rs](src/logic.rs), about thirty lines. No cursor-shaped abstraction covers it, because there is no "next unit"; there is a set of things not yet done. Scaling the phase to sixteen commanders a side did not change the shape: the set got wider (sixty-four units), ownership moved into the guard (your squad, not your army), and the emptiness check is the same line it was at two players.

**What that answers about the deferred `TurnOrder`/`TurnPolicy` split.** That entry waited for a third turn order to design a step-policy against. The third consumer arrived and is not an order at all: within a side there is nothing for a cursor to point at. So the honest boundary is that `TurnManager` covers *sequences*, sets are a different primitive, and a policy abstraction stretched over both would be describing neither.

Note what this example does **not** use: no `TurnManager` (two alternating sides are the phase itself, `Command(Blue)` to `Command(Red)`), and no per-recipient snapshots (a battle is open information, so one uniform view serves the room). Between this, `night_watch` (phases and rounds, no turns) and `draft_board` (turns and rounds, phases as bookkeeping), the module's pieces are demonstrated separable: take what the game needs.

## The bots

[`bots.rs`](src/bots.rs) is the second wearer of `card_table`'s bot pattern: bot commanders join only to even an odd muster, and each plays from the same `BattleView` every client renders, picking from the server-computed options for its own squad alone. It holds no movement rules, so it cannot disagree with the server about what a unit may do; at worst it picks badly. Strike the weakest in reach, mend the most wounded patient, otherwise close the distance and take cover on ties, otherwise hold; the phase ends itself when the army's set drains, so no bot ever needs `EndPhase`.

## What it exercises from the rest of the family

- **Unbounded rounds, second consumer.** A battle ends when an army is routed or a commander flees, never on a count. `rounds_are_unbounded` pins it.
- **`PhasedScheduler`, first consumer born after the extraction.** The side deadline is scheduled against `Command(army)`'s occupancy, and a phase ended early leaves its deadline to be dropped inside the scheduler; the example writes no staleness check at all. `the_stale_deadline_does_not_end_the_phase_that_replaced_its_own` puts both deadlines due on the same tick and asserts exactly one phase ends.
- **The `guard` pattern, second wearer.** Seated, right phase, your unit: one auditable function.
- **Sides swap every deployment**, and the assignment is **stored at deployment rather than derived from seat index**, because a test caught the derived version changing the survivor's colours when the loser's departure shifted their index. A forfeit therefore names the right victor, which is also why the victor is a stored field and not a board inference: after a forfeit both armies still stand.

## Build shapes

Same as the other listen-server examples: `default` is the native desktop build (client, server, tungstenite socket); `--no-default-features --features web` is the browser client alone, which `wasm-build.sh` wraps; `--role headless` is the deployable server, which also serves `static/` with the wasm cache-busted. The compiled `static/field_orders.wasm` is a build product and is gitignored; run `wasm-build.sh` before serving a fresh checkout.
