# ghost_trials

A time trial whose opponents are **replays of an op log**, and a server that decides your time by replaying it too.

Drive two laps through the rings, as fast as you can. Every run you finish becomes a ghost, and every ghost is raced by everyone who joins afterwards. That part is a racing game. The reason this example exists is what a ghost turns out to be.

`plaza`'s op stream is an event-sourced record, which means state is not the thing you keep, it is the thing you can always get back. Nothing else in this repository takes that literally. Here it is the whole design: a ghost is not a recorded path, it is the **inputs**, replayed through the same rules that produced them. And a lap time is not a number a client reports, it is a number the server derives by running the evidence.

## Running it

```sh
./run-native.sh                              # host and drive, serves the browser page too
./run-native.sh -- --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p ghost_trials                   # every claim below, as a test
```

Pick a mode: **time trial** alone against the clock and the ghosts, or **race** against three CPU drivers who shove and take your pickups.

Left and right steer. Hold space to charge: you slow down, you turn harder, and you bank a boost that spends when you let go. `R` starts again, `Escape` goes back to the menu.

## What you are looking at

| On screen | Meaning |
|---|---|
| solid arrow | you |
| hollow arrows | ghosts. Hollow on purpose: an echo of a run, not another car |
| yellow ring | the one you are looking for. They count **in order** |
| ring around your car | charge, winding up |
| tail behind your car | a boost, being spent |
| the number in the middle | your split against the ghost you are chasing |
| the strip at the bottom | the board, and what each ghost cost to send against what a path would have |
| purple arrows | the CPU field, in a race |
| **T** and **G** discs | pickups. **T** is a turbo, **G** is grip. An outline is one that has been taken and is coming back |
| rim around a car | grip, running |

## The one decision everything follows from

**A run is stored as the inputs that produced it.** [`InputLog`](src/sim/log.rs) is a rules version and a list of spans, where a span is one held input and the tick it stops being held on. That is not run-length encoding applied to a recording; it is what an event log already looks like, because an event is a *change*.

Three things follow, and they are the example.

### 1. A ghost costs its inputs, not its positions

Measured on the fixture in `the_log_is_a_fraction_of_the_path_it_describes`: a two-lap run is **146 entries over 1208 ticks, 738 bytes**, against **12,088 bytes** of positions sampled once per tick. Sixteen times less, and the gap widens with the length of the run, because one side of it does not grow with the ticks at all.

The honest caveat is in the test, next to the number: the saving is a function of how often the *input changes*. The fixture that drives these tests originally steered every single tick, because a bang-bang autopilot flips its wheel constantly, and it scored barely three times better. A deadband made it drive like a person and the ratio jumped to sixteen. An event log is small exactly to the degree that the input holds still, so a player sawing at the wheel gets less of this than a smooth one, and a machine gets least of all.

### 2. A time is not a claim, it is a consequence

The server never watches anybody race. There is nothing to arbitrate in a time trial, so `LogicInput::TimeStep` does nothing here but move a clock, which `a_tick_simulates_nothing` asserts rather than leaves to be assumed.

What the server does is **decide by reconstruction**. A submission is a log and the time the client believes it takes; [`verify`](src/sim/log.rs) replays the log through the shared rules and reads the time off the replay. The claim is only ever a checksum on the evidence:

```rust
if time != claimed_ms {
  return Err(Rejection::TimeDoesNotMatch { claimed: claimed_ms, replayed: time });
}
```

That is the whole anti-cheat, and it is not a heuristic. There is no plausibility check, no speed cap, no statistical model. The inputs either produce that time or they do not. `a_faked_time_is_refused_because_the_log_does_not_produce_it` sends a halved time and gets back the real one; the panel has a switch to do it live.

The cost of deciding this way is worth having as a number rather than as a worry, so the arena counts the ticks it replays: **one trial is about 1200 ticks of integer arithmetic, once, at the end of a run somebody spent half a minute driving.**

### 3. Latency cannot affect a lap time

Not "barely". Not "within a tolerance". `latency_cannot_change_a_lap_time` drives the same inputs at 0, 80, 250 and 400 ms one way and asserts the four times are **identical**, because the run happens entirely on the machine driving it and the link is not in the loop. Every other playground here spends its design effort making latency cheap; this is the one where it is not on the path at all.

What the link does decide is when a ghost turns up and how quickly a lie is caught, and neither touches the driving. A lost submission costs the run and never the board: there is no retry, deliberately, so a dropped lap is a disappointment rather than a corruption. The board only ever holds runs that were verified.

## Two modes, one log

The menu picks between two arrangements of the same track, the same rules and the same op log, which is the comparison worth drawing:

**A time trial has nothing to arbitrate.** One car, one clock. So the client owns the whole of the feel, the server never watches, and the verdict arrives afterwards. Latency is not on the path at any depth.

**A race has three CPU drivers on the circuit with you**, shoving for room and taking pickups out from under you. And the log does not get any bigger, because **the opponents are a pure function of the world**. `bot_input` reads a racer and the track and returns what that racer holds this tick; nothing else. So one player's key presses reproduce a four-way race, every shove and every stolen pickup included, which `one_players_log_reproduces_a_whole_four_way_race` asserts by driving one, replaying it, and comparing the whole field.

That is `seed_defense`'s trick pointed at opponents instead of a wave of enemies, and it is why the mode is stored *in* the log: replaying a race log as a trial would leave three cars out, and the time it produced would be a time nobody drove.

### The CPU field is deliberately uneven, and deliberately sloppy

A field of identical drivers is a wall or a parade. The three seats have different tolerances for being off line, different appetites for charging, and different rates of simply not paying attention for a moment. `the_cpu_field_is_uneven` asserts the sharp one finishes ahead of the sloppy one, because a change that flattened the field would otherwise pass every other test here.

The mistakes come from **a hash of the tick and the seat, not from a generator**. There is no random state anywhere in this example, and that is the point: a generator is hidden state that a log does not carry, so a ghost would need it saved and restored to replay. A pure function of the tick needs nothing.

The noise is also sampled in *chunks* of ticks rather than per tick, which matters twice. A mind that changed every tick drives like a bang-bang controller and reads as a twitch rather than a mistake. And in a trial, where the player's own inputs *are* recorded, driving in that shape is exactly what makes an event log stop being small.

### The power-ups change a rule, not a number

Two, and they are part of the circuit rather than events: fixed positions, fixed kinds, a fixed respawn interval. Nothing about them is drawn from anywhere, which is what lets a run be reproduced from its inputs alone.

- **Turbo** hands over the boost you would otherwise have had to slow down to earn.
- **Grip** gives you the charge turn *without* the charge speed, which inverts the trade the whole game is built on for a few seconds.

A contested pickup goes to the racer with the lowest index, not to whoever was closest and not to whoever the loop reached first. Both of those are rules about the container rather than about the game. The shoves are the same discipline: **every impulse is computed from the state before any of them lands**, and `a_shove_is_the_same_whichever_order_the_pairs_come_up_in` reverses the list and checks the outcome is mirrored.

## What a replay is a bet on

Replay is reproduction, and reproduction is a bet that today's arithmetic matches the arithmetic that recorded it. `seed_defense` makes the same bet between two machines *now*; this one makes it between a machine and **a recording made somewhere else, at some other time**, and the recording cannot be asked to compromise.

So the same discipline applies, and one piece more:

- **No floating point in the simulation.** The fixed-point type is [`playground_common::fixed`](../playground_common/src/fixed.rs), shared with `seed_defense` rather than copied, because two copies of a type that must agree to the bit is the "shared rule written twice" mistake with the stakes raised.
- **The angles go through a table of integer literals**, not `sin`. A library trigonometric function is not specified to the last bit across platforms or versions, and it is on the path of every single tick.
- **The rules file is hashed into the wire version.** `build.rs` feeds `rules.rs` to `plaza_wire::build::emit` alongside the message shapes, because a change to how a racer handles invalidates every recorded log exactly as surely as a change to a message would. A log carries the version it was made under, and one from a different version is **refused rather than replayed wrong**: replaying it would produce some run, and that run would be a lie about what its player drove.

That last one is the failure this example is really about, and it is the friendly kind of failure: an honest player, a valid log, and a world that has moved on underneath it.

## The self check, and what it caught

When a run ends, the client replays its own finished log and compares the result to the racer it actually drove. On one machine, with one implementation, that should be impossible to fail.

It failed the first time it ran.

`finished_tick` is the *index* of the tick a lap completed on, so the number of ticks taken is one more than it. The client counted ticks taken; the replay counted the index. Twenty milliseconds, invisible on screen, and every honest submission would have been refused with `TimeDoesNotMatch` off by exactly one tick. Nothing else in the example would have noticed, because the physics were fine and the log was fine.

So the check stays, and the panel counts it. It does not test the simulation, it tests the **recorder**, which is the part with no other witness: a recorder that closes a span one tick early makes a ghost that drifts away from the run it came from, slowly, in a way that looks like bad luck.

## What is deliberately not done

- **Nothing is predicted, and nothing is corrected.** There is no authority racing alongside you to disagree with. The client owns the whole of the feel; the server owns the verdict, and the verdict arrives after the run is over.
- **The clock is not in the simulation.** A lap is counted in ticks taken, not in wall time, so a client with a badly fitted clock still records the same lap. That is what makes a run comparable with one driven on another machine a week later.
- **The frame rate is not in the simulation either.** The input held during a frame is applied to every whole tick that frame covers, with the remainder carried. A racing game is the easiest place in the world to advance by "however long the last frame took", and it would make every recorded lap a function of the frame rate that recorded it. `the_simulation_runs_in_whole_ticks_however_long_a_frame_took` feeds the same total time in awkward pieces and asserts the tick count matches.

## How it is built

- **[src/sim/](src/sim/)** is the whole game, headless: the table, the track, the rules, the log, the authority and the client. No sockets, no window, no async. Every claim above is a test at this layer, and [`sim/world.rs`](src/sim/world.rs) is the harness that puts a server and its clients in one process with an impaired link between them.
- **[src/net/](src/net/)** wraps that for a real wire and **adds no rules**. It is the thinnest arena in the repository, which is the finding rather than a shortcut: there is nothing to simulate centrally.
- **[src/render.rs](src/render.rs)** and **[src/ui.rs](src/ui.rs)** draw it and put the numbers on screen.

## Notes

- Excluded from `default-members`, so a bare `cargo build` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- Building for wasm needs `--no-default-features --features web`; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build product and is gitignored. Run `wasm-build.sh` before serving a fresh checkout.
