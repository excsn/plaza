# pellet_maze

A maze chase whose turn input needs more than tick scheduling to stay in sync.

Eat the pellets, don't get caught and take the roles in turn. The example is about one input. You never stop moving and **pressing a direction does not turn you straight away**: it queues a turn and the turn happens at the next junction where that direction is a corridor rather than a wall. Which junction that is depends on the maze and the moment and both sides have to work it out independently.

The other examples in this repository deal with **when** an input happens. [`bomb_grid`](../bomb_grid/) put an input on a tick and made both sides step the same quantum, so a shared rule ran identically on both. That still holds here, but two sides can agree about when a turn was requested, run the same rule on the same tick and still take the turn at **different junctions**. Then they are in different corridors and the gap grows instead of closing.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p pellet_maze                    # every claim below, as a test
```

WASD or the arrow keys. There is no key for "stop". On a phone, a thumb pad appears the first time you touch the screen and a press on it requests a turn the same way a key press does.

## What you are looking at

| On screen | Meaning |
|---|---|
| circle with a white ring and a caret | **you** |
| circle | the runner: eats pellets, is hunted |
| square | a pursuer |
| white arrow off a player | a **turn waiting for a junction**. If it is still there, the corner has not come |
| hollow circle under yours | where the **server** says you are. Only a host has this; a joiner has no way to know it |
| red box, green box, line between | a **wrong junction**: where you turned, where the server turned and the distance it opened |
| orange ring on the floor | an **energizer**: the runner eats pursuers for six seconds |
| blue ring on the floor | **vanish**: the runner is hidden from every other client for four and a half seconds |
| orange halo | an energized runner. Contact now goes the other way |
| white squares with a coloured outline | the pursuers **while that lasts**: they are prey and they flash back to their own colour as it runs out |
| dimmed square | a pursuer that was eaten, walking home and harmless on the way |

## How a turn is resolved

[`Op::Turn`](src/sim/protocol.rs) carries a direction and the tick it was asked for and deliberately **no cell**. The server decides which junction the turn happens at. A client that could name the junction could name any junction and the junction decides which corridor you end up in for the next several seconds.

[`TurnQueue`](src/sim/turn_queue.rs) implements this in about sixty lines. A request is held and on every tick where the player is exactly on a cell boundary the queue checks whether that direction is open. If it is, the turn is taken and the queue reports the cell it happened in. If the buffer elapses first, the turn is dropped. If a turn becomes possible on the tick it would expire, it is taken, because dropping it there would lose turns to a rounding decision nobody could detect by feel.

The example is built around three consequences of that.

### 1. Wrong junctions and cell corrections

A cell correction is bounded: one jump and it's done. [`bomb_grid`](../bomb_grid/) counts that as a snap and it is the normal cost of predicting on a lattice.

A **wrong junction** is not bounded. Take the corner one junction earlier than the server did and you end up in a different corridor heading a different way, rather than one cell out. The error grows with every step until a frame drags you back across the maze. So the panel counts them separately and puts the wrong junctions first: `wrong junctions: 3 of 40 turns (8%), worst 6 cells apart`. Averaging the two together would let a hundred cheap corrections hide three expensive ones.

Tests pin the distinction. `a_perfect_link_never_turns_at_the_wrong_junction` and `latency_alone_still_turns_at_the_right_junction` are the baseline: **latency alone does not cause a wrong junction** at any depth, because running ahead of the server does not make a client wrong. Lost input does cause one, as `losing_a_turn_request_is_what_sends_the_two_sides_down_different_corridors` shows: a request the server never heard means the client turned and the server did not. Raising packet loss in the panel moves the counter and raising latency leaves it at zero.

### 2. The turn buffer

`turn_buffer_ms` is in [`ServerPolicy`](src/sim/protocol.rs) and arrives in the `Welcome`. A client does not assume it, because a client with a longer buffer would predict a turn the server had already forgotten, then run down a corridor the server never entered. That would be a wrong junction caused by a policy mismatch alone.

It is also a good slider to play with, since there is no right value. A short buffer is precise and unforgiving: press slightly early into a corner and nothing happens. A long one takes corners you pressed for four junctions ago. The panel reports the two failure modes as separate counters, `turns taken` and `turns expired waiting for a place`, because they point in opposite directions and one number cannot tell them apart.

### 3. Vanish and the per-recipient frame

The vanish power-up is why [`Frame`](src/sim/protocol.rs) is built **per recipient** rather than broadcast:

```rust
players: self.players.iter()
  .filter(|p| p.id == recipient || !p.hidden(now))
  .cloned().collect(),
```

A hidden runner is left out of the other clients' frames entirely rather than dimmed or flagged. Once a client has been sent the position the secret is out, whatever it draws. [`card_table`](../card_table/) does the same with a hand of cards; here the hidden thing moves sixty times a second. This is what `plaza`'s per-recipient dispatch is for. `a_hidden_runner_is_absent_from_other_players_frames` checks that the runner is missing from other players' frames and present in its own.

The per-recipient frame was not enough on its own. The first version hid the player from every frame and still leaked its position completely, because the *events* kept going out to everybody:

- `Op::Eaten` names the exact cell a pellet went from, on the exact tick. That is a **better** position report than a frame, because a frame is rate limited and an event is not.
- `Op::PowerTaken` names the cell of the pickup.
- `Op::TurnTaken` names the junction. Nobody even read it: a client discards every turn report that is not its own.
- Even `Frame::pellets_left` and `Frame::powerups` gave it away, one as a count that dropped while nothing visible was happening, the other as a pickup that vanished from a cell.

So an event now carries an [`Audience`](src/sim/server.rs). Anything that names a hidden player's cell goes to that player alone and is **held** for everybody else until the vanish ends. It is sent then, because a client that was never told would draw pellets that are gone for the rest of the round. Turn reports are addressed to the player they describe, hidden or not, since no other client reads them. The two frame fields are computed per recipient, adding back what that recipient has not been told about.

The end to end test `nothing_on_the_wire_says_where_a_hidden_player_is` checks what was actually sent: it takes the hidden player's cell on every tick and checks every op every other seat was handed against it. The one deliberate exception is the tick the vanish expires, when everything held back goes out and the player is in the frames again anyway.

## The match and why the score is cumulative

A round is rarely cleared of pellets. Three pursuers against one runner is deliberately unfair, so a round is a few seconds of pressure rather than a board to complete. The score that counts is the **total over the match**: one round per seat, with the roles rotating every round, so every seat runs exactly once and hunts in all the others.

Pellets pay one, a catch pays twenty-five, eating a pursuer while energized pays fifteen. Points scored in a round you lose still count. `a_match_runs_a_fixed_number_of_rounds_and_then_resets_the_scores` tests this.

The final table gets five seconds to itself before the next match is laid out. It first went out in the same tick as the next `RoundStart` and a client clears the table when a round starts, so the final scores were on screen for a single frame. `the_final_table_gets_an_interval_of_its_own` checks that nothing is laid out in that tick and nothing starts until the interval is up.

Roles rotate by seat while ids stay fixed. `the_role_rotates_for_a_given_seat_while_its_identity_does_not` exists because the first version rotated by reassigning ids and a client that drives `players[seat]` then found itself playing somebody else's character between rounds.

## The power-ups

There are two and each one changes a rule rather than a number.

**Energize** inverts contact. `resolve_contact` is one function on the server and while the runner is energized it reads the same collision the other way round: the pursuer is eaten, sent home at a faster step and harmless on the walk. A speed boost or a shield would only have changed a coefficient. Energize reverses who wins a contact and contact is what decides a round.

The inversion is drawn for **both** sides; the first version missed this. The runner gets a halo and every pursuer turns white for as long as it lasts, keeping its own colour as an outline so you can still tell which one you are and flashing over the last stretch so you can see the end coming. In the first version only the runner could see it, so the three players who had become prey found out by being eaten.

**Vanish** removes the runner from what other clients are *sent*, as described above. It needs per-recipient frames, since any other approach still sends the runner's position.

Pursuers step at 205 ms per cell against the runner's 145. Being chased by three pursuers at your own speed in a maze this size is unplayable. The numbers are constants at the top of [`sim/types.rs`](src/sim/types.rs) so you can check that with one edit.

## The bots

Three of the four seats are usually bots, so a bot that runs in circles makes the whole example look broken.

Both roles use BFS over the maze in [`sim/rules.rs`](src/sim/rules.rs) and each needed a fix that only showed up once it was measured:

- A pursuer does not reverse in a corridor, except at a dead end. Without that, two pursuers oscillate around the runner and never close.
- A runner's route excludes the direction it came from unless that is the only exit, or it paces between two pellets it can no longer eat.
- Under threat the runner **still eats**; it just refuses to walk toward a pursuer. The first version fled instead, so it never ate under pressure and the runner is under pressure nearly all the time. Eating went from 36 pellets in 45 seconds to 165 with this change and `a_bot_runner_actually_eats` fails on the old behaviour.

The bots do **not** seek power-ups. A version that did was tried and removed after measuring it: routing a threatened runner to a nearby energizer devoured no more pursuers over a minute, ate 22 fewer pellets and left six power-ups on the board. A runner already crosses every corridor eating, so it walks over them anyway. The measurement is recorded in `a_bot_runner_reaches_the_energizers_and_turns_on_its_pursuers`, which asserts the board ends empty and the inversion is reached.

`drive_bots` had the same kind of bug: it originally skipped any player that was mid-step and since a player begins the next step the instant it finishes the last, that was nearly always. Bots now decide about the cell they are **entering**, every tick.

## The shared rule code

[`sim/rules.rs`](src/sim/rules.rs) holds the movement rule, the passability rule and the turn resolution, as free functions over plain state and both sides call them. The server is the authority. A client predicting itself runs the same functions on the same tick grid.

Sharing the rule makes wrong junctions rare. It also means each one has a network cause: with one implementation, disagreeing about where a turn happened always means disagreeing about the input history. A second implementation would produce wrong junctions with no network cause at all and the panel counter would no longer measure the network.

## Wire format

- **A cell is one `u16`**, packed. A struct of two `u8`s would cost a MessagePack array header per cell.
- **Every fieldless enum crosses as a `u8`**. MessagePack writes a unit variant as its *name*, so `Dir`, `Role` and `Power` would otherwise spell themselves out on every frame.
- **Pellets are sent as events** rather than as a diff of the set. There are several hundred and they only ever disappear.
- **The maze is sent once per round.** It does not change during one.
- **`TurnTaken` is not needed to play.** The next frame's heading already implies the turn. It carries the cell because the wrong-junction comparison needs it.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the maze, the rules, the turn queue, the authority and a client that predicts against it. No sockets, no window, no async. Every claim above is a test at this layer and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**. The arena is the same server behind `plaza`'s `StateLogic`, dispatching a frame per seat; the client is the same client behind a socket, a clock estimate and a connection state.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

Both sides step in whole `SIM_STEP_MS` ticks and the server's host uses [`TickDriver::run_fixed`](../../core/API_REFERENCE.md#struct-tickdriver). `the_prediction_is_driven_by_the_clock_not_by_how_often_it_is_polled` and `an_irregular_tick_driver_produces_the_same_world_as_a_regular_one` test that from both ends. [bomb_grid](../bomb_grid/) took a full debugging session to establish that this is required.

## Notes

- The maze generator's repair pass has `every_corridor_cell_has_a_way_out_on_every_seed` and `every_spawn_can_move_on_every_seed` over 400 seeds, because the first version of each test checked one seed and passed while a player was walled in on screen.
- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`, because the default set pulls in the native socket and the actix server; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
