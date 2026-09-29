# ghost_trials

A time trial whose opponents are **replays of an op log** and a server that decides your time by replaying it too.

Drive two laps through the rings as fast as you can. Every run you finish becomes a ghost and everyone who joins afterwards races the six fastest. The example exists because of how a ghost is stored.

`plaza`'s op stream is an event-sourced record, which means state never has to be kept because it can always be rebuilt. This is the only example in the repository that relies on that directly: a ghost is stored as the **inputs**, replayed through the same rules that produced them, rather than as a recorded path. The server derives a lap time from those inputs rather than taking the client's report.

## Running it

```sh
./run-native.sh                              # host and drive, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p ghost_trials                   # every claim below, as a test
```

Pick a mode: **time trial** alone against the clock and the ghosts or **race** against a CPU field of up to 32 who shove and take your pickups. The menu also picks the circuit, small, medium or large.

Left and right steer. Hold space to charge: you slow down, you turn harder and you bank a boost that spends when you let go. `R` starts again, `Escape` goes back to the menu.

On a phone, steer and charge buttons appear the first time you touch the screen. They are buttons rather than a stick because the input is one of three values and because **charging has to be holdable at the same time as steering**: macroquad synthesises a mouse click from a touch and a synthesised mouse is one pointer, so anything held in combination has to read the real touches.

## What you are looking at

| On screen | Meaning |
|---|---|
| solid arrow | you |
| hollow arrows | ghosts. Hollow because a ghost is a replay of a run rather than another car |
| yellow ring | the one you are looking for. They count **in order** |
| ring around your car | charge, winding up |
| tail behind your car | a boost, being spent |
| the number in the middle | your split against the ghost you are chasing |
| the strip at the bottom | the board and what each ghost cost to send against what a path would have |
| purple arrows | the CPU field, in a race |
| a car going hollow and fading | somebody who has finished. They stop being an obstacle the moment they cross |
| **T**, **G**, **S** and **L** discs | pickups. **T** is a turbo, **G** is grip, **S** is a shield and **L** is slick. An outline is one that has been taken and is coming back |
| rim around a car | grip, slick or shield, running |

## Runs are stored as inputs

A run is stored as the inputs that produced it. [`InputLog`](src/sim/log.rs) is a rules version and a list of spans, where a span is one held input and the tick it stops being held on. This is the normal shape of an event log, where each event is a *change*, rather than run-length encoding applied to a recording.

Three things follow from that.

### 1. The size of a ghost

Measured on the fixture in `the_log_is_a_fraction_of_the_path_it_describes`: a two-lap run is **146 entries over 1208 ticks, 738 bytes**, against **12,088 bytes** of positions sampled once per tick. That is sixteen times smaller and the gap widens with the length of the run, because the log grows with input changes rather than with ticks.

The test records the caveat next to the number: the saving depends on how often the *input changes*. The fixture that drives these tests originally steered every single tick, because a bang-bang autopilot flips its wheel constantly. That version scored barely three times better. A deadband made it drive like a person and the ratio jumped to sixteen. A player sawing at the wheel saves less than a smooth one and a bang-bang autopilot saves the least.

### 2. The server derives the time from the log

The server never watches anybody race. There is nothing to arbitrate in a time trial, so `LogicInput::TimeStep` does nothing here but move a clock, which `a_tick_simulates_nothing` asserts.

Instead the server **replays the submission**. A submission is a log and the time the client believes it takes. [`verify`](src/sim/log.rs) replays the log through the shared rules and reads the time off the replay. The claimed time is only checked against that result:

```rust
if time != claimed_ms {
  return Err(Rejection::TimeDoesNotMatch { claimed: claimed_ms, replayed: time });
}
```

That comparison is all the anti-cheat there is. There is no heuristic, plausibility check, speed cap or statistical model. `a_faked_time_is_refused_because_the_log_does_not_produce_it` sends a halved time and gets back the real one; the panel has a switch to do it live.

The arena counts the ticks it replays so the cost is measured: **one trial is about 1200 ticks of integer arithmetic, run once at the end of a run that took about half a minute to drive.**

### 3. Latency cannot affect a lap time

`latency_cannot_change_a_lap_time` drives the same inputs at 0, 80, 250 and 400 ms one way and asserts the four times are **identical** rather than close, because the run happens entirely on the machine driving it and the link is not involved. Every other playground here spends its design effort making latency cheap; here latency is not on the path at all.

The link only decides when a ghost turns up and how quickly a faked time is caught. Neither affects the driving. A lost submission loses that run without touching the board; there is deliberately no retry. The board only holds runs that were verified.

## Time trial and race

The menu picks between two modes that share the track, the rules and the op log.

**A time trial** has nothing to arbitrate. It is one car against the clock, so the client runs all of the driving, the server never watches and the verdict arrives afterwards. Latency is not on the path at any depth.

**A race** puts a CPU field on the circuit with you, anything from one other car to thirty-one, shoving for room and taking pickups out from under you. **A race of thirty-two is recorded by the same log as a race of two**, because the opponents are a pure function of the world. `bot_input` reads only a racer and the track and returns what that racer holds this tick. So one player's key presses reproduce the whole field, every shove and every stolen pickup included, which `one_players_log_reproduces_a_whole_four_way_race` asserts by driving one, replaying it and comparing every car.

The circuit and the field size are in the log too, for the same reason the mode is: they are cheap (a byte and a number) and a run cannot be reproduced without them. The track itself is **never sent**. Both ends build it from the size, because the layouts are constants both ends already have.

`seed_defense` uses the same approach for a wave of enemies; here it is applied to opponents. It is also why the mode is stored *in* the log: replaying a race log as a trial would leave the CPU field out and produce a time that no run actually took.

### Making the CPU field uneven

A field of identical drivers moves as one block, fast or slow. The CPU seats cycle through a set of driver profiles with different tolerances for being off line, different appetites for charging and different rates of simply not paying attention for a moment. `the_cpu_field_is_uneven` asserts the sharp one finishes ahead of the sloppy one, because a change that flattened the field would otherwise pass every other test here.

The mistakes come from **a hash of the tick and the seat** rather than a random generator. There is no random state anywhere in this example, because a generator is hidden state that a log does not carry, so a ghost would need it saved and restored to replay. A hash of the tick needs nothing saved.

The noise is also sampled in *chunks* of ticks rather than per tick, for two reasons. A driver whose mind changed every tick would drive like a bang-bang controller and look like a twitch rather than a mistake. In a trial, where the player's own inputs *are* recorded, driving in that pattern is what makes an event log large.

### The power-ups

There are four kinds and they are part of the circuit rather than events: fixed positions, fixed kinds and a fixed respawn interval. Nothing about them is random, so a run can be reproduced from its inputs alone.

- **Turbo** gives you the boost you would otherwise have had to slow down to earn.
- **Grip** gives you the charge turn *without* the charge speed.
- **Slick** is the same trade in the other direction: faster in a straight line and it will not turn.
- **Shield** takes no shove but still gives one. That follows from the ordering rule below: the impulses are all computed before any of them lands, so a shielded car has already pushed everyone it touched by the time its own push is skipped.

Grip and slick are opposites because the game is built on one trade (pace against cornering). Pickups that move you along that trade are more interesting than ones that give you more of everything.

A contested pickup goes to the racer with the lowest index rather than to whoever was closest or whoever the loop reached first. Both of those are rules about the container rather than about the game. Shoves follow the same rule: **every impulse is computed from the state before any of them lands** and `a_shove_is_the_same_whichever_order_the_pairs_come_up_in` reverses the list and checks the outcome is mirrored.

### A finished car leaves the track

When a racer crosses the line it stops moving and it stops being an obstacle: it is taken out of the collision set and fades off the screen over a couple of seconds rather than parking on the finish line.

This is for fairness. Cars that stop where they finish pile up exactly where everybody else is heading, so a late finisher's time would depend on **how many people beat them there**. `a_finished_racer_stops_being_an_obstacle` covers it.

### The starting grid

Thirty-two cars in one row is wider than the small circuit's arena and thirty-two rows deep runs off the back of it, so the grid is the smallest square that holds the field, centred on the start line in **both** directions. Some cars therefore begin a fraction ahead of others. That is unfair in a race and makes no difference in a trial, where the field is one car.

`a_full_grid_starts_inside_the_arena_and_not_on_top_of_itself` checks both halves on all three circuits, because a pile-up at the start would decide the race.

## Keeping replays exact

A replay only reproduces a run if today's arithmetic matches the arithmetic that recorded it. `seed_defense` depends on the same match between two machines running at the same time. This example depends on it between a machine and **a recording made somewhere else at some other time**, which cannot be changed to fit.

So the same rules apply, plus one more:

- **No floating point in the simulation.** The fixed-point type is [`plaza_client_utils::fixed`](../../client_utils/src/fixed.rs), shared with `seed_defense` rather than copied, because two copies of a type that must agree to the bit would be the "shared rule written twice" mistake.
- **The angles go through a table of integer literals** rather than `sin`. A library trigonometric function is not specified to the last bit across platforms or versions and it is on the path of every single tick.
- **The rules file is hashed into the wire version.** `build.rs` feeds `rules.rs` to `plaza_wire::build::emit` alongside the message shapes, because a change to how a racer handles invalidates every recorded log just as a change to a message would. A log carries the version it was made under. A log from a different version is **refused**, because replaying it would produce a run its player never drove.

That last case is the main failure this example guards against. Nobody is at fault in it: the player is honest and the log is valid, but the rules have changed since it was recorded.

## The self check

When a run ends, the client replays its own finished log and compares the result to the racer it actually drove. That should never fail on one machine with one implementation.

It failed the first time it ran. `finished_tick` is the *index* of the tick a lap completed on, so the number of ticks taken is one more than it. The client counted ticks taken and the replay counted the index. The difference was twenty milliseconds and invisible on screen, but every honest submission would have been refused with `TimeDoesNotMatch`, off by exactly one tick. Nothing else in the example would have noticed, because the physics and the log were both correct.

So the check stays and the panel counts it. It tests the **recorder** rather than the simulation, since nothing else checks the recorder. A recorder that closes a span one tick early makes a ghost that slowly drifts away from the run it came from, which looks like bad luck.

## What is left out

- **Nothing is predicted or corrected.** No authority races alongside you, so there is nothing to disagree with. The client runs all of the driving and the server only gives a verdict after the run is over.
- **The clock is not in the simulation.** A lap is counted in ticks taken rather than wall time, so a client with a badly fitted clock still records the same lap. That makes a run comparable with one driven on another machine a week later.
- **The frame rate is not in the simulation either.** The input held during a frame is applied to every whole tick that frame covers, with the remainder carried. Advancing by "however long the last frame took" is tempting in a racing game and would make every recorded lap a function of the frame rate that recorded it. `the_simulation_runs_in_whole_ticks_however_long_a_frame_took` feeds the same total time in awkward pieces and asserts the tick count matches.

## The impairment sliders

If you turn the latency to 800 ms and drive a lap, **the time is identical**. The slider is working: the run happens on the machine driving it and the link is not involved.

The link decides when the verdict on your run comes back and when somebody else's ghost turns up. The impairment is on the real path for both: the session holds back every frame the connection carries in either direction and on a datagram link it can drop one. The panel reports the frames it dropped, read back from the session rather than counted here, because a frame the link discarded never reaches this arena to be counted.

An earlier version of this example did **not** impair the live path at all. The sliders were wired only to the offline harness, so on a real host they changed nothing and a player who turned the latency up saw no change anywhere and concluded the example was not doing anything. `the_sliders_are_published_to_the_link_rather_than_applied_here` now asserts the panel's numbers reach the session as a link profile. Holding the frames back is the session's job and is tested there.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the table, the track, the rules, the log, the authority and the client. No sockets, no window, no async. Every claim above is a test at this layer and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**. It is the thinnest arena in the repository because there is nothing to simulate centrally.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

## Notes

- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
