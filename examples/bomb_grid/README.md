# bomb_grid

A server-authoritative multiplayer game on a **lattice**, and what a lattice changes about netcode.

Drop bombs, blow up walls, be the last one standing. That part is ordinary. The reason this example exists is the part underneath: every other networked playground in this repository is continuous, and a continuous game can *hide* its netcode. A position is a point, an error is a few pixels, and a correction is eased away over a handful of frames so smoothly that nobody sees it happen.

Here a position is a cell. There is nothing between two cells to ease through. So every correction is a jump the player can see, and the panel counts them, because the alternative is pretending they do not happen.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p bomb_grid                      # every claim below, as a test
```

WASD or the arrow keys to walk, space to drop a bomb. On a phone, a thumb pad and a bomb button appear the first time you touch the screen.

## What you are looking at

| On screen | Meaning |
|---|---|
| filled circle | a player, drawn at their cell (or between two, mid-step) |
| hollow circle under yours | where the **server** says you are. Only a host has this; a joiner legitimately cannot |
| solid black bomb | a bomb the server has confirmed |
| hollow bomb | a bomb **you predicted** and the server has not confirmed yet |
| red outline and a line | a **snap**: the cell you were corrected from, and where to. Fades over about a second |
| brown tiles | soft walls, destructible |
| grey tiles | the pillar lattice, permanent |

## The one decision everything follows from

**A player's position is a cell.** A step from one cell to the next takes time and is drawn as motion, but the simulation only ever knows "in cell C" or "walking from C to D, N milliseconds in". Nothing rounds a float to a cell, because there is no float to round. The fractional position exists in exactly one function, [`PlayerState::draw_pos`](src/sim/types.rs), and it is called by the renderer and by nothing else.

That has three consequences, and they are the example.

### 1. A correction cannot be eased, so it is counted instead

`plaza_client_utils::PredictedPlayer` is the right tool for a continuous entity and both other networked playgrounds use it. It is not used here, and not because it is missing a feature: its whole shape is a *seen* position, a *settled* position, and an ease between them. Between two cells there is nothing to ease through. Easing would draw the player somewhere they have never been, in a game where where you are decides whether you are on fire.

So the client snaps, and [`Client::snaps`](src/sim/client.rs) is on the panel next to the rate per hundred frames and the total cells jumped. Two numbers rather than one, because four one-cell snaps and one four-cell snap feel completely different and a count alone cannot tell them apart.

**Latency alone does not cause snaps.** That is the measurement worth taking away, and it is pinned by a test: at 120 ms with 30 ms of jitter the snap count is zero. A client running ahead of the server is not *wrong*, it is ahead, and the comparison is made against what this client believed **at the frame's own timestamp**, not against what it believes now. Comparing against the newest belief reports a snap on every single frame and makes the counter useless. What actually snaps a player is **losing an input**, because then the two sides ran different inputs, and that has no continuous resolution.

### 2. Prediction hides the round trip, not the playout delay

Inputs are addressed by **tick**: an input says which tick it is meant for, and the server runs it on that tick or refuses it. That is what makes two players reaching for the same escape cell resolve by who pressed first rather than by who is nearer the server, and on a lattice the stakes are higher than in a continuous game, because the contested thing is frequently the only cell that is not about to be on fire.

The consequence for prediction is easy to get wrong, and getting it wrong looks exactly like prediction not working. A client that predicts its input the instant the key goes down is running that input a playout depth *earlier* than the server will. In a continuous game that is a small offset that eases away. Here it is a whole cell of disagreement on **every input**, and the snap counter reads as though prediction were broken.

So the client schedules its own prediction for the same tick it named. Prediction removes the round trip and nothing else; the playout delay is still paid, by everybody, which is precisely what makes the game fair. Turn the playout buffer off in the panel and both the fairness and the delay go together.

### 3. A slow link is refused, not merely slow

An input named for `press + playout` that arrives after that tick has passed is **dropped**, not shifted. That is `plaza_server_utils::InputSchedule`'s reject-not-correct rule: correcting a backdated tick still executes the input, so a lag switch loses the lie and keeps the steering.

The honest cost lands on honest players too. A one-way delay longer than the playout depth plus the late window means every input is refused, and the client, which predicted them, can only resolve that by snapping. There is a test for exactly this at 400 ms one way, and it asserts the server's own rejection counters climb on the late side.

**A client cannot detect this by itself**, which is the part worth knowing. An input is acknowledged on *arrival*, before admission, so a refused input and an applied one look identical from the client: the acknowledgement lag stays healthy while nothing you do takes effect. Only the host's panel can say, which is why it prints the per-seat verdicts and the margin in ticks of the last refusal.

## Chain reactions, and why they are one message

A bomb going off can fire another, which fires another, and each arm is cut short by walls that earlier arms *in the same instant* have just removed. Resolving that as "each bomb explodes when its own fuse ends" gives a different board depending on the order the bombs happen to be stored in, which is the kind of bug that reproduces once in fifty rounds and cannot be debugged from a report.

So [`Server::detonate`](src/sim/server.rs) is a breadth-first sweep to a fixed point, and the whole cascade crosses the wire as one [`Op::Blast`](src/sim/protocol.rs) with one timestamp. A client holds every bomb's cell, radius and declared fire time, so it *could* compute the explosion itself. That is exactly the trap: the two sides would agree almost always, and the times they did not are the times somebody dies.

The board is applied from a blast **immediately**, not on the render timeline, and that asymmetry is deliberate: the tiles are an *input* to the movement rule the client is predicting against. Holding a wall the server has destroyed would refuse a step the server allows, which is a snap manufactured out of stale state. The fire is drawn on the timeline; the walls are not.

## What is shared as code, and what that is worth

[`sim/rules.rs`](src/sim/rules.rs) holds the movement rule, the passability rule and the bomb-placement rule, as free functions over plain state. The authoritative server calls them. A client predicting itself calls them. There is no second implementation to drift.

This is the strongest correlation the playgrounds in this repository have found, and on a lattice the stakes are higher than the continuous examples showed: a continuous rule written twice diverges by a few pixels a second and the correction hides it. A discrete rule written twice puts the two sides in different cells, permanently, and every step is a snap.

The bomb-placement rule matters as much as the movement one. A client that guessed differently about the carry limit would draw a bomb the server was certain to refuse, and the player would watch it vanish. Sharing the rule means the client refuses locally exactly what the server would refuse remotely, so a phantom is always a real disagreement and never a self-inflicted one.

## Sharing the rule is not enough: the quantum has to be shared too

This example was written with a shared rule and tick-addressed inputs from the first commit, and it still snapped constantly. Four separate bugs, and every one of them was the same thing: the two sides advanced that shared rule in **different quanta**.

- The **acknowledgement** retired an input from the client's pending list before the tick it named had arrived, so the client never ran it. The server acknowledges on *arrival*, which on a fast link is a hundred milliseconds before execution. It shows up as a player who will not stop walking.
- The **round-over interval** freezes every player on the server so the last explosion stays readable, and nothing in a frame says so. A client that keeps predicting through it is corrected on every frame of the interval.
- The **client** advanced once per rendered frame while the server advanced once per tick. Even at matching rates the grids are unaligned, so the two cross every cell boundary up to a tick apart. It scales with boundaries crossed, so open ground is where it becomes obvious.
- The **server** advanced by `TickDriver`'s *measured* elapsed time: 16 ms, then 17, then 16. That makes the simulation a function of the host's scheduler, which nothing can reproduce. This one survived the three client-side fixes above and cost 2.2 snaps per hundred frames with no loss, no jitter and every input accepted on time.

Both sides now step in whole `SIM_STEP_MS` ticks. The client catches up to `clock / SIM_STEP_MS` one step at a time; the server accumulates elapsed time and spends it the same way, and its host uses [`TickDriver::run_fixed`](../../core/API_REFERENCE.md#struct-tickdriver), which exists because of this example. The `dt` parameter is gone from the client's `tick` entirely: a caller must not be able to influence how fast a prediction runs. The simulation keeps its own accumulator as well as using the fixed driver, deliberately, so a different driver cannot silently break the guarantee.

**Three of the four looked exactly like network faults**, which is the part worth carrying away. A correction is what a network problem looks like, so a correction is where you stop looking.

## Where the wire went

Small and bounded, deliberately, and this is a genuine difference from [`horde_playground`](../horde_playground/) rather than an oversight. Relevance culling and delta compression exist to make an *unbounded* world affordable. This world has a hard ceiling: at most four players, a handful of bombs, and a 15x13 board. A frame goes out whole because a delta of it would be more machinery than the thing it compresses.

What the wire does carry carefully:

- **A cell is one `u16`**, packed, rather than a struct of two `u8`s that would cost a MessagePack array header per cell.
- **Every fieldless enum crosses as a `u8`**, with the numbers pinned in the conversion. MessagePack writes a unit variant as its *name*, so `Tile`, `Dir` and `Powerup` would otherwise spend real bytes spelling themselves out on every frame.
- **The board is sent once per round**, not per frame. It only ever loses walls, and losing one is announced by the blast that did it.
- **A bomb declares when it fires**, on the server clock, rather than counting down. A chained bomb fires early, and a client running its own countdown would keep drawing a fuse for a bomb that has already gone off.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the board, the rules, the authority, and a client that predicts against it. No sockets, no window, no async. Every claim above is a test at this layer, and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**. The arena is the same server behind `plaza`'s `StateLogic`; the client is the same client behind a socket, a clock estimate, and a connection state.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

One thing the networked client has that the harness cannot: a **clock estimate**. The offline harness hands its clients the server's own `now_ms`, which is exactly what a real client does not have. Every input names a tick computed from that estimate, so an estimate that trails the stream names ticks the server has already closed and every input is silently refused. The aim is therefore floored against the newest server timestamp actually received, which is a lower bound that needs no synchronisation at all, because the server wrote it. That failure, and that fix, are the horde example's, paid for with two wrong fixes before the right one.

## Notes

- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`, because the default set pulls in the native socket and the actix server; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
