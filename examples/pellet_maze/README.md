# pellet_maze

A chase in a maze, and the input that a schedule cannot fix.

Eat the pellets, do not get caught, and take the roles in turn. That part is the game. The reason this example exists is one input: **you never stop moving, and pressing a direction does not turn you.** It queues a turn, and the turn happens at the next junction where that direction is a corridor rather than a wall. Which junction that is, is a fact about the maze and the moment, and both sides have to arrive at it independently.

Everything else in this repository has spent its effort on **when** an input happens. [`bomb_grid`](../bomb_grid/) put an input on a tick and made the two sides step the same quantum, and after that a shared rule ran identically on both. Here that is still true and still not enough: two sides can agree perfectly about when a turn was requested, run the same rule on the same tick, and still take the turn at **different junctions**. Then they are in different corridors, and the gap grows instead of closing.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh -- --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p pellet_maze                    # every claim below, as a test
```

WASD or the arrow keys. There is no key for "stop".

## What you are looking at

| On screen | Meaning |
|---|---|
| circle with a white ring and a caret | **you** |
| circle | the runner: eats pellets, is hunted |
| square | a pursuer |
| white arrow off a player | a **turn waiting for a junction**. If it is still there, the corner has not come |
| hollow circle under yours | where the **server** says you are. Only a host has this; a joiner legitimately cannot |
| red box, green box, line between | a **wrong junction**: where you turned, where the server turned, and the distance it opened |
| orange ring on the floor | an **energizer**: the runner eats pursuers for six seconds |
| blue ring on the floor | **vanish**: the runner is hidden from every other client for four and a half seconds |
| orange halo | an energized runner. Contact now goes the other way |
| dimmed square | a pursuer that was eaten, walking home and harmless on the way |

## The one decision everything follows from

**A turn is a request for a place, not for a time.** [`Op::Turn`](src/sim/protocol.rs) carries a direction and the tick it was asked for, and deliberately **no cell**. A client that could name the junction could name any junction, and the junction decides which corridor you end up in for the next several seconds.

So the server decides where. [`TurnQueue`](src/sim/turn_queue.rs) is the whole mechanism and it is about sixty lines: a request is held, and on every tick where the player is exactly on a cell boundary it asks whether that direction is open. If it is, the turn is taken and the queue reports the cell it happened in. If the buffer elapses first, the turn is dropped. A turn is taken on the tick it would otherwise expire, not dropped on it, because the alternative loses turns to a rounding decision nobody would ever guess at from the feel of it.

That has three consequences, and they are the example.

### 1. Predicting a place is a different problem from predicting a position

A cell correction is bounded: one jump, then over. That is what [`bomb_grid`](../bomb_grid/) counts as a snap and it is the honest cost of predicting on a lattice.

A **wrong junction** is not bounded. Take the corner one junction earlier than the server did and you are not one cell out, you are in a different corridor, heading a different way, and the error grows with every step until a frame drags you back across the maze. So the panel counts them separately and puts the wrong junctions first: `wrong junctions: 3 of 40 turns (8%), worst 6 cells apart`. Averaging the two together would let a hundred cheap corrections hide three expensive ones.

The distinction is pinned by tests rather than asserted. `a_perfect_link_never_turns_at_the_wrong_junction` and `latency_alone_still_turns_at_the_right_junction` are the baseline: **latency alone does not cause a wrong junction**, at any depth, because a client running ahead of the server is not wrong, it is ahead. What does is `losing_a_turn_request_is_what_sends_the_two_sides_down_different_corridors`: a request the server never heard means the client turned and the server did not. Drag packet loss up in the panel and watch the counter move; drag latency up and watch it stay at zero.

### 2. The buffer is a server setting, and a client is told it

`turn_buffer_ms` is in [`ServerPolicy`](src/sim/protocol.rs) and arrives in the `Welcome`. A client does not assume it, because a client with a longer buffer would predict a turn the server had already forgotten, then run down a corridor the server never entered: a wrong junction manufactured out of a disagreement about policy rather than about the world.

It is also the slider worth playing with, because it is a real design decision with no correct answer. Short is precise and unforgiving: press a hair early into a corner and nothing happens. Long takes corners you pressed for four junctions ago. The panel reports both failure modes as separate counters, `turns taken` and `turns expired waiting for a place`, because they say opposite things and a single number cannot tell them apart.

### 3. Invisibility has to be a property of the frame

The vanish power-up is the reason [`Frame`](src/sim/protocol.rs) is built **per recipient** rather than broadcast:

```rust
players: self.players.iter()
  .filter(|p| p.id == recipient || !p.hidden(now))
  .cloned().collect(),
```

A hidden runner is not dimmed on the other clients' screens, and not flagged. They are **absent**. A client handed a position it should not see has already lost the secret, whatever it chooses to draw, and "please do not render this" is a request rather than a rule. This is the same principle [`card_table`](../card_table/) applies to a hand of cards, in a game where the hidden thing moves sixty times a second, and it is what `plaza`'s per-recipient dispatch is for. `a_hidden_runner_is_absent_from_other_players_frames` asserts both halves: gone from theirs, present in their own.

## The match, and why the score is cumulative

A round is rarely cleared of pellets. Three pursuers converging on one runner is not a fair fight and is not meant to be, so a round is a few seconds of pressure rather than a board to complete. What is played for is the **total over five rounds**, and the roles rotate every round, so every seat runs and every seat hunts.

Pellets pay one, a catch pays twenty-five, eating a pursuer while energized pays fifteen. A round you lose is not a round you scored nothing in. `a_match_runs_a_fixed_number_of_rounds_and_then_resets_the_scores` holds the shape.

The role rotation is a seat rotation, not an identity one. `the_role_rotates_for_a_given_seat_while_its_identity_does_not` exists because the first version rotated by reassigning ids, and a client that drives `players[seat]` then finds itself playing somebody else's character between rounds.

## The power-ups, and what makes one interesting here

Two, and they were picked because each one changes a rule rather than a number.

**Energize** inverts contact. `resolve_contact` is one function on the server, and while the runner is energized it reads the same collision the other way round: the pursuer is eaten, sent home at a faster step, and harmless on the walk. Speed boosts and shields would have been a coefficient; this is the sign flipping on the rule the whole round is about.

**Vanish** removes the runner from what other clients are *sent*, per the section above. That one could not have been done at all without per-recipient frames, and doing it any other way would have been a lie.

Pursuers step at 205 ms per cell against the runner's 145. Being hunted by three things at your own speed in a maze this size is not a game, and the numbers are constants at the top of [`sim/types.rs`](src/sim/types.rs) precisely so that finding out is one edit away.

## The bots, and why they had to be made good

Three of the four seats are usually bots, so the bots **are** the game, and a bot that runs in circles makes the whole example look broken rather than looking like a bot problem.

Both jobs are BFS over the maze in [`sim/rules.rs`](src/sim/rules.rs), and both got a fix that was invisible until measured:

- A pursuer does not reverse in a corridor, except at a dead end. Without that, two pursuers oscillate around the runner and never close.
- A runner's route excludes the direction it came from unless that is the only exit, or it paces between two pellets it can no longer eat.
- Under threat the runner **still eats**; it just refuses to walk toward a pursuer. The first version fled instead, which meant it never ate anything under pressure, which is all the time. Eating went from 36 pellets in 45 seconds to 165 as a direct consequence, and `a_bot_runner_actually_eats` fails on the old behaviour.

The bots do **not** seek power-ups, and a version that did was written, measured and deleted: routing a threatened runner to a nearby energizer devoured no more pursuers over a minute, ate 22 fewer pellets, and left six power-ups on the board. A runner already crosses every corridor eating, so it walks over them anyway. The measurement is recorded in `a_bot_runner_reaches_the_energizers_and_turns_on_its_pursuers`, which asserts the board ends empty and the inversion is reached.

`drive_bots` had its own version of the same bug: it originally skipped any player that was mid-step, and since a player begins the next step the instant it finishes the last, that was very nearly always. Bots decide about the cell they are **entering**, every tick.

## What is shared as code, and what that is worth

[`sim/rules.rs`](src/sim/rules.rs) holds the movement rule, the passability rule and the turn resolution, as free functions over plain state, and both sides call them. The server is the authority. A client predicting itself runs the same functions on the same tick grid.

The rule being shared is what makes a wrong junction *rare*. It is what makes a wrong junction **meaningful** when it happens: with one implementation, a disagreement about where the turn happened is always a disagreement about the input history, which is always a network fact. A second implementation would produce wrong junctions with no network cause at all, and the counter on the panel would measure nothing.

## Where the wire went

- **A cell is one `u16`**, packed. A struct of two `u8`s would cost a MessagePack array header per cell.
- **Every fieldless enum crosses as a `u8`**. MessagePack writes a unit variant as its *name*, so `Dir`, `Role` and `Power` would otherwise spell themselves out on every frame.
- **Pellets ride as events**, not as a diff of the set. There are several hundred and they only ever disappear.
- **The maze is sent once per round.** It does not change during one.
- **`TurnTaken` is not needed to play.** The next frame's heading already implies the turn. It carries the *place* because the place is the thing this example is about, and without it there is nothing to compare.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the maze, the rules, the turn queue, the authority, and a client that predicts against it. No sockets, no window, no async. Every claim above is a test at this layer, and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**. The arena is the same server behind `plaza`'s `StateLogic`, dispatching a frame per seat; the client is the same client behind a socket, a clock estimate, and a connection state.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

Both sides step in whole `SIM_STEP_MS` ticks and the server's host uses [`TickDriver::run_fixed`](../../core/API_REFERENCE.md#struct-tickdriver). `the_prediction_is_driven_by_the_clock_not_by_how_often_it_is_polled` and `an_irregular_tick_driver_produces_the_same_world_as_a_regular_one` hold that from both ends. This is not a matter of taste: the four bugs it prevents are written up in [LEARNINGS.md](../LEARNINGS.md#four-bugs-with-one-shape-bomb-grid) and cost a full debugging session in the example before this one.

## Notes

- The maze generator's repair pass has `every_corridor_cell_has_a_way_out_on_every_seed` and `every_spawn_can_move_on_every_seed` over 400 seeds, because the first version of each test checked one seed and passed while a player was walled in on screen.
- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`, because the default set pulls in the native socket and the actix server; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
