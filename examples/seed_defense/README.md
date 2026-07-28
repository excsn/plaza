# seed_defense

A co-op tower defence whose wire carries **causes instead of consequences**, and the machinery that catches you when that goes wrong.

Place towers, hold twelve waves. That part is ordinary. The reason this example exists is that **the enemies are never sent**. Every machine is handed a seed once, told which tick each wave begins on, and produces the entire wave itself. A screen full of enemies costs about the same as an empty one, because there is nothing to send either way.

That is the oldest trick in networked games and it comes with a bill: the moment two machines compute one number differently, they are not slightly out of sync, they are playing different games, and nothing on either screen says so. So half of this example is the trick and the other half is the audit.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh -- --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p seed_defense                   # every claim below, as a test
```

Click a buildable tile to place the selected tower, click a tower to upgrade it.

## What you are looking at

| On screen | Meaning |
|---|---|
| the bar above the map | **agreement**. Green while every digest has matched, red from the tick one did not |
| brown corridor | the path. Not buildable |
| squares | towers, outlined in the colour of whoever paid |
| circles | enemies, with a health bar |
| pale circle outline | the reach of the tower under your cursor, drawn from the same function the simulation shoots with |
| thin beams | shots. **Nobody sends these**: both sides derive them from the same step |

## The one decision everything follows from

**The wire carries what caused the world, never the world.** A whole session is:

- **one seed**, at join;
- **two integers per wave**, the wave number and the tick it starts on;
- **one small op per build**, naming the tick every machine applies it on;
- **eight bytes of digest**, twice a second, which describes nothing and proves everything;
- **a full snapshot, only when a digest has already proved something is wrong.**

`what_crossed_the_wire_never_described_an_enemy` reads that off the wire rather than off the server's intent: across forty seconds and dozens of enemies, not one message carried a position.

That has three consequences, and they are the example.

### 1. Latency costs nothing. Not "a little". Nothing.

In every other playground here, latency buys corrections, and the design work is about making them cheap: interpolation, prediction, reconciliation, easing. Here there is no correction to make cheap, because nothing a client computes depends on when anything arrived. `latency_costs_nothing_at_all` runs the whole thing at 0, 60, 200 and 400 ms one way and asserts **zero mismatches and zero snapshots at every depth**. Drag the latency slider in the panel and watch the agreement bar not move.

**Loss is a different thing entirely, and the difference is the point.** A lost position sample is superseded by the next one. A lost *cause* never happened on one machine, and no amount of waiting repairs it. So loss is paid for in the one expensive message this design has, and `loss_costs_a_snapshot_rather_than_a_wrong_world` pins both halves: at 25% loss the resyncs are greater than zero, and they are far fewer than one per digest, because a recovery that fires on every check is not a recovery.

### 2. A diverged client looks completely healthy

This is the part worth carrying away, and it is why the agreement bar exists.

When a predicted client is wrong, you can see it: the snap, the rubber band, the stutter. When *this* client is wrong there is no symptom at all. Enemies walk, towers fire, money accrues, the frame rate is fine. It is simply a different game on the same screen. "It looks right" is worth nothing here, which makes the digest the only instrument there is.

So `Field::digest` folds the whole field, built on `plaza_client_utils::SetDigest`, which exists for exactly this shape: an order-independent additive fold, so two machines holding the same set in a different order still agree. Everything goes in, including each enemy's position to the last bit. `the_digest_key_notices_a_single_step_of_drift` exists because a digest over *rounded* positions would agree while the two sides were a tile apart, which is precisely the drift it is there to catch.

The comparison is made **at the server's tick, not the newest one**. A client running behind is not wrong, it is earlier, and comparing across the gap would report a mismatch on every message. Digests that name a tick this client has not reached are held until it gets there, the same shape `pellet_maze` uses for turn reports.

### 3. Everything is integer, and the floats are in one place

The simulation contains no floating point. `Fx` is a signed 32-bit fixed-point value with 8 fractional bits, and `Fx::to_f32` is one-way and called only by the renderer.

That is not fastidiousness. `f32` is not guaranteed to give the same answer in a wasm build and a native one: a compiler may contract a multiply and an add into a fused multiply-add, keep an intermediate in a wider register, or reassociate a sum. Any of those changes the last bit, and here the last bit is never corrected.

**But the obvious version of that worry is wrong, and this example measured it.** See below.

## Three ways to break it, on purpose

The panel can turn each of these on, for this client only, and each is a real change to the arithmetic rather than a fault injected into a readout. A determinism claim that cannot be falsified on demand is not a demonstration.

Writing them was the most instructive part of building this, because **two of the first three could not diverge at all**, and would have sat in the panel implying a detection that never fires.

- **A float in an accumulator does nothing.** The first attempt moved enemies with an `f32` multiply and add. It never changed a single tick, on any seed, over any length of run, because the result is truncated back to 1/256 of a tile *every tick*: the error is discarded before it can accumulate. Re-quantising every tick is a large part of why fixed point works, and it means "we use floats but round the positions" really is most of the protection.
- **A float in a range diverges too rarely to see.** The second attempt made a tower's radius a float, changing it by 1/256 of a tile. Genuine, and it only matters during the fraction of a tick an enemy spends crossing that band while a tower happens to be off cooldown. Undetectable in a minute of play, and therefore useless as a demonstration. A determinism bug's *frequency* depends on how often the differing value is consulted, not on how wrong it is.
- **A float in a constant that multiplies time breaks it immediately.** A runner covers 4.2 tiles a second, which is `26.88` in 256ths per tick. The integer ratio floors that to 26; working it out in floating point and rounding gives 27. Four percent, every tick, for ever. Ten seconds later the two machines' runners are a tile and a half apart and each is being shot at by a different tower. **Audit the constants, not the loops.**

The other two toggles are the classics: **target the first enemy in range** instead of the one furthest along, which silently encodes the container's iteration order into the rules of the game; and **round a timer to a tenth of a second**, the kind of tidying that looks harmless in a diff and changes when a slow ends, which changes where an enemy is, which changes what every tower picks next.

A fourth was written and deleted: "iterate the towers in hash order". It is impossible to make matter here, because damage is additive and the dead are collected after every tower has fired, so no tower can steal another's kill within a tick. The loop now carries a comment saying why its order is safe, rather than leaving the next reader to assume it. `each_quirk_actually_diverges` asserts the surviving three do change the world, because **a fault injector has to be tested like anything else**.

## What is shared as code, and how far that goes

Further than anywhere else in this repository. [`rules::step`](src/sim/rules.rs) takes the whole field and advances it by one tick, and it is called by the server and by every client. There is no "the server does the real one and the client approximates": there is one function.

Every ordering it depends on is defined rather than incidental. Towers fire in placement order, targets are chosen by progress along the path with the id as an explicit tie-break, and the spawn schedule is a list built once from the seed. Nowhere does a rule depend on the order a collection happens to iterate in, because two builds are entitled to iterate differently.

The generator is part of that. `plaza_client_utils::net_sim::Rng` exists and is deterministic, and it is deliberately not used: its documented contract is a test and demo aid, so its algorithm may change. In an example whose entire wire is a seed, **the generator is the wire format**, so it lives here and `the_stream_is_pinned_by_its_actual_numbers` names the numbers it must produce. A test that only reseeds and compares passes for any generator, including a changed one.

## What is deliberately not predicted

**Your own build.** You click, and the tower appears when the server's op comes back naming a tick everyone applies it on. In every other example here that would be the obvious thing to predict, and here it is the one thing that must not be: predicting it means simulating a cause the server might refuse, and there is no correction to undo that. It would be a divergence the digest catches half a second later.

The trade is inverted from every other playground: **the world is predicted entirely and the input is not predicted at all.**

## The one genuinely fragile point

An op that arrives *after* the tick it names cannot be applied late, because late means simulating a history no other machine will ever hold. So a client in that position has one honest move, which is to say so and ask for the state.

That makes the build lead a real policy with a real constraint: it has to clear the worst one-way delay. `a_build_lead_shorter_than_the_link_cannot_be_met` sets a 100 ms lead over a 300 ms link and asserts the late builds climb and the snapshots follow. The panel counts them, and the slider is there to fix it.

The wave announcement has the same constraint and a much larger margin: it goes out at the *start* of the prep phase, seconds ahead of the tick it names, rather than one tick ahead. The first version announced it one tick ahead, which worked perfectly at zero latency and resynced every client on every wave at sixty milliseconds.

## Where the wire went

Nowhere, mostly, which is the measurement. The host panel keeps two counters: what actually went out, and what the same session would have cost with the field streamed at the send rate, which is what every other playground here does. `what_was_sent_is_a_fraction_of_what_streaming_would_have_cost` asserts at least a twentyfold difference over forty seconds, and it grows with the enemy count, because one side of that comparison does not grow at all.

The saving is not compression. The state was never encoded, because it was never sent.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the fixed-point maths, the generator, the map, the shared rules, the authority, and a client that reproduces it. No sockets, no window, no async. Every claim above is a test at this layer, and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

The host uses [`TickDriver::run_fixed`](../../core/API_REFERENCE.md#struct-tickdriver), never `run`. `run` delivers the measured elapsed time, which would make the simulation's rate a property of the host's scheduler. In the lattice examples that costs a correction; here there are no corrections, so it would cost every client its agreement with the server, permanently.

## Notes

- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
