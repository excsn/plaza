# seed_defense

A co-op tower defence where the wire carries the causes of the world state (a seed, wave starts and builds) instead of the state itself, plus the machinery that detects when two machines stop agreeing.

Place towers and hold the line. The waves do not stop and each one is harder than the last, so a run ends when the line breaks, not when a counter runs out. What this example demonstrates is that **the enemies are never sent**. Every machine gets a seed once, is told which tick each wave begins on and produces the whole wave itself. A screen full of enemies costs about the same bandwidth as an empty one, because enemies are not sent either way.

This is an old technique and it has a cost: once two machines compute one number differently they are playing different games and nothing on either screen shows it. Half of this example is the technique and the other half detects when it breaks.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p seed_defense                   # every claim below, as a test
```

Pick a tower in the strip along the bottom, click a buildable tile to place it, click a tower to upgrade it. Hovering a tower shows what it does now and what the next level costs and buys.

The waves never stop and each is harder than the last, so every run ends with the line overrun. The session keeps going: the board stays up long enough to read, then the server lays out a fresh field and sends it as a `Snapshot`, the message that already means "stop computing and adopt this". So a restart needs no new op and no new agreement rule. The tick keeps running throughout, because it is the session's clock and a new run does not reset it.

## What you are looking at

| On screen | Meaning |
|---|---|
| the bar above the map | **agreement**. Green while every digest has matched, red from the tick one did not |
| the banner over the map | the next wave and its countdown, computed from the announcement's own tick |
| the strip along the bottom | the build menu and the inspector: cost, damage per second, range, fire rate and the upgrade price |
| brown corridor | the path. Not buildable |
| squares | towers, outlined in the colour of whoever paid |
| circles | enemies, with a health bar |
| pale circle outline | the reach of the tower under your cursor, drawn from the same function the simulation shoots with |
| thin beams | shots. These are not sent: both sides derive them from the same step |

## What the wire carries

The wire carries only the causes of the world state. A whole session is:

- **one seed**, at join;
- **two integers per wave**, the wave number and the tick it starts on;
- **one small op per build**, naming the tick every machine applies it on;
- **eight bytes of digest**, twice a second, which describes no state and only lets the two sides check they agree;
- **a full snapshot, only when a digest has already proved something is wrong.**

`what_crossed_the_wire_never_described_an_enemy` checks this against the actual wire traffic, not what the server meant to send: across forty seconds and dozens of enemies, no message carried a position.

That has three consequences.

### 1. Latency and loss

In every other playground here, latency causes corrections and the design work is about making them cheap: interpolation, prediction, reconciliation, easing. Here there are no corrections, because nothing a client computes depends on when anything arrived. `latency_costs_nothing_at_all` runs the whole thing at 0, 60, 200 and 400 ms one way and asserts **zero mismatches and zero snapshots at every depth**. Drag the latency slider in the panel and the agreement bar does not change.

Loss is different. In a streamed game a lost position sample is replaced by the next one. Here a lost cause never happened on one machine and waiting does not fix it. Loss is recovered with a snapshot, the one expensive message this design has. `loss_costs_a_snapshot_rather_than_a_wrong_world` pins both halves: at 25% loss there are some resyncs, but far fewer than one per digest.

### 2. Detecting divergence

When a predicted client is wrong you can see it as a snap, a rubber band or a stutter. When this client is wrong there is no symptom: enemies walk, towers fire, money accrues and the frame rate is fine, but its game has diverged from the server's. The screen shows nothing wrong, so the digest, which drives the agreement bar, is the only way to find out.

`Field::digest` folds the whole field using `plaza_client_utils::SetDigest`, an order-independent additive fold, so two machines holding the same set in a different order still agree. Everything goes in, including each enemy's position to the last bit. `the_digest_key_notices_a_single_step_of_drift` checks this, because a digest over *rounded* positions would still agree while the two sides were a tile apart.

The client compares digests at the tick the server named, not at its own newest tick. A client running behind is only earlier, so comparing across the gap would report a mismatch on every message. A digest naming a tick this client has not reached yet is held until it gets there, the same way `pellet_maze` handles turn reports.

### 3. Fixed-point arithmetic

The simulation contains no floating point. `Fx` (from `plaza_client_utils::fixed`, behind its `fixed` feature) is a signed 32-bit fixed-point value with 8 fractional bits and `Fx::to_f32` is one-way and called only by the renderer.

This is because `f32` is not guaranteed to give the same answer in a wasm build and a native one: a compiler may contract a multiply and an add into a fused multiply-add, keep an intermediate in a wider register or reassociate a sum. Any of those changes the last bit and here the last bit is never corrected.

Measuring it showed that the obvious version of that worry is wrong. See the next section.

## Three ways to break it

The panel can turn each of these on for this client only. Each one changes the arithmetic itself; none of them fakes a readout. They exist so you can cause divergence on demand and watch the detection fire.

Two of the first three attempts could not diverge at all and would have sat in the panel implying a detection that never fires.

- **A float in an accumulator.** The first attempt moved enemies with an `f32` multiply and add. It never changed a single tick, on any seed, over any length of run, because the result is truncated back to 1/256 of a tile *every tick*, which throws the float error away before it can accumulate. Re-quantising every tick is a large part of why fixed point works, so "we use floats but round the positions" really does give most of the protection. What it does not cover is a float-derived constant that is multiplied by time, which is the third case below.
- **A float in a range.** The second attempt made a tower's radius a float, changing it by 1/256 of a tile. That is a real difference, but it only matters during the fraction of a tick an enemy spends crossing that band while a tower happens to be off cooldown. It did not show up in a minute of play, so it was useless as a demonstration. How often a determinism bug diverges depends on how often the differing value is read. The size of the difference matters much less.
- **A float in a constant that multiplies time.** This one diverges immediately. A runner covers 4.2 tiles a second, which is `26.88` in 256ths per tick. The integer ratio floors that to 26; working it out in floating point and rounding gives 27. That is four percent too far on every tick, so ten seconds later the two machines' runners are a tile and a half apart and each is being shot at by a different tower. Audit the constants before the loops.

The other two toggles are common determinism bugs: **target the first enemy in range** instead of the one furthest along, which makes the container's iteration order part of the game rules; and **round a timer to a tenth of a second**, which looks harmless in a diff. Rounding changes when a slow ends, which changes where the enemy is and what every tower picks next.

A fourth was written and deleted: "iterate the towers in hash order". Order cannot matter here, because damage is additive and the dead are collected after every tower has fired, so no tower can take another's kill within a tick. The loop has a comment saying why its order is safe. `each_quirk_actually_diverges` asserts that the remaining three do change the world, so a toggle that stops diverging fails a test.

## What is shared as code

This example shares more code between server and client than any other in the repository. [`rules::step`](src/sim/rules.rs) advances the whole field by one tick and the server and every client call it. There is no separate client approximation.

Every ordering it depends on is defined explicitly. Towers fire in placement order, targets are chosen by progress along the path with the id as an explicit tie-break and the spawn schedule is a list built once from the seed. No rule depends on the order a collection happens to iterate in, because two builds are allowed to iterate differently.

The random generator is part of that. `plaza_client_utils::net_sim::Rng` is deterministic but is not used here: it is documented as a test and demo aid, so its algorithm may change. When the whole wire is a seed, changing the generator changes what every seed means, so this example has its own and `the_stream_is_pinned_by_its_actual_numbers` lists the numbers it must produce. A test that only reseeds and compares would pass for any generator, including a changed one.

## What is deliberately not predicted

Your own builds. When you click, the tower appears once the server's op comes back naming the tick everyone applies it on. In every other example here that would be the obvious thing to predict. Here it must not be: predicting it means simulating a cause the server might refuse and there is no correction to undo that. It would be a divergence the digest catches half a second later.

So this client predicts the whole world and none of its own input, which is the reverse of every other playground here.

## Ops that arrive after their tick

An op that arrives *after* the tick it names cannot be applied late, because that would produce a history no other machine has. The client reports it and asks for the state.

So the build lead has to be longer than the worst one-way delay. `a_build_lead_shorter_than_the_link_cannot_be_met` sets a 100 ms lead over a 300 ms link and asserts the late builds climb and the snapshots follow. The panel counts late builds and has a slider for the lead.

The wave announcement has the same constraint and a much larger margin: it goes out at the *start* of the prep phase, seconds ahead of the tick it names, rather than one tick ahead. The first version announced it one tick ahead, which worked perfectly at zero latency and resynced every client on every wave at sixty milliseconds.

## Game UI and the panel

The build menu, the tower stats and the upgrade price are drawn on the canvas, not in the egui panel. The panel holds the diagnostics: what crossed the wire, whether the machines still agree and the switches that break them. Choosing a tower is part of the game, so it is on the canvas. The first version put it inside a collapsing header in the panel, so you had to open a diagnostics window to take a turn.

The numbers in the strip come from the same functions the simulation shoots with: `TowerKind::damage`, `range`, `cooldown_ms` and `upgrade_cost`. The UI does not keep its own copy of the price list, since that would be a second implementation of a shared rule.

## What the session sent

The host panel keeps two counters: what actually went out and what the same session would have cost with the field streamed at the send rate, which is what every other playground here does. `what_was_sent_is_a_fraction_of_what_streaming_would_have_cost` asserts at least a twentyfold difference over forty seconds. The gap grows with the enemy count, because what is actually sent does not depend on it.

None of the saving comes from compression. The state is never encoded because it is never sent.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the generator, the map, the shared rules, the authority and a client that reproduces it. No sockets, no window, no async. Every claim above is a test at this layer and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and adds no rules.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

The host stands up on [`SimHost`](../../session/API_REFERENCE.md#struct-simhost-and-struct-simwiring), whose driver is [`TickDriver::run_fixed`](../../core/API_REFERENCE.md#struct-tickdriver), never `run`. `run` delivers the measured elapsed time, so the simulation's rate would depend on the host's scheduler. In the lattice examples that causes a correction. Here there are no corrections, so every client would permanently disagree with the server.

## Notes

- Excluded from `default-members`, so a bare `cargo build` in `examples/` skips it. `-p seed_defense` or `--workspace` includes it.
- Building for wasm needs `--no-default-features --features web`; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
