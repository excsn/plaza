# 3DGoW

A small MMO zone on generated ground, and the netcode a game does not need because its design already absorbed the latency.

The crate is `gow_3d` because Cargo rejects a package name beginning with a digit.

```sh
./run-native.sh          # play it, and host it, in one window
./wasm-serve.sh          # the same thing in a browser, on port 8301
cargo test -p gow_3d     # the findings, as assertions
```

WASD walks, **space jumps**, **tab** cycles a beast to fight, **1** Strike, **2** Bolt, **3** Mend, **P** parties with the nearest adventurer and **O** leaves. `--bots N` sets how many adventurers the zone seats for itself; it seats eighteen beasts alongside them.

The zone is not empty and never was meant to be: two dozen adventurers hunt beasts across a landscape of hills, water and rock while you do. In ten seconds of a headless zone that is 104 casts landing and 8 characters coming back up, with nobody connected at all.

The thing worth doing is walking away from somebody you are partied with. Their body leaves the world and their entry stays, with a bearing and a height offset. That is the whole argument of this example in one action.

## What it is for

Set this beside `puck_rink`, which spends an entire rollback apparatus (owned fixed-point arithmetic, per-frame digests, re-simulation of every confirmed frame) to hide a hundred milliseconds on five bodies. This example hides a hundred and fifty on sixty-four bodies and the netcode for it is a keypress and a report. The difference is not cleverness, it is that the genre's designers already made the player wait.

So the lesson is about game design wearing a netcode example's clothes, and the numbers below are the argument.

## A cast bar is a latency budget the player agreed to in advance

`cargo test -p gow_3d casting -- --nocapture`

```
  the share of the wait a player can blame on the network:

      cast     rtt 30    rtt 150    rtt 300
         0       100%       100%       100%
       400        7%        27%        43%
      1000        3%        13%        23%
      1500        2%         9%        17%
      2500        1%         6%        11%
```

Worth being precise, because "cast times hide latency" is a claim people repeat and it is not quite true: **the delay never shrinks.** It is the same 150ms at every cast time. What changes is what fraction of the wait it was. A player perceives the share, not the delay, and at a cast time the genre actually uses, a bad connection is a smaller share than a good connection is of an instant ability.

The global cooldown does the same job for the *inputs* that a cast time does for the outcome: a player who cannot act again for 1500ms is a player whose next input was never going to be frame-tight. That is why an instant ability is not an exception.

**The cooldown runs during a cast rather than after it.** The two waits overlap and the longer is the whole of it, so a long cast is free rather than doubly expensive. The first version of `zone.rs` assumed they stacked and the test caught it.

## Client authority is a cliff, not a gradient

`cargo test -p gow_3d movement -- --nocapture`

Every other example in this tree is server-authoritative, because that is the right default and plaza is built for it. This one is not: the client says where it is and the server sanity-checks. It buys perfectly smooth local movement with no prediction, no reconciliation and no correction to ease off.

```
  a second of running, against an honest 7.0 units:

     claimed     achieved       gain
        1.0x         6.72      1.00x
        1.1x         7.39      1.10x
        1.3x         8.74      1.30x
        2.0x         0.90      0.13x
       10.0x         0.00      0.00x
```

Read it as a cliff rather than a curve. **A cheat inside the tolerance simply works, at exactly the rate it claims.** A ten percent overrun is indistinguishable from a late packet, and a threshold tight enough to catch it throws out honest players on bad connections. Past the tolerance it collapses: a 2x claim achieves 13% of an honest run, because almost every claim is refused and the server keeps its own position. There is no setting that separates 1.3x from a bad connection, because they are the same observation.

Anyone reading this as an endorsement of client authority has been failed by the example. It is a demonstration of a trade with the price visible.

### Both modes, one build

The plan for this example asked for the comparison rather than either mode alone, so both are live and the panel switches between them. Two builds and two sessions compare two memories of how something felt.

```
       authority        gap now      worst gap     refusals
          client          0.00u          0.00u            0
          server          0.23u          0.23u            0
```

That is measured with **no network delay simulated at all**, which is what makes it worth having: it is the floor rather than a reading off one connection. Client authority cannot disagree with itself. Server authority is already one tick of travel behind, because nothing local moves until the answer arrives, and everything a real connection adds sits on top of that.

Neither arm produced a refusal on an honest walk, and under server authority that is because no position was ever claimed. Sending one anyway is refused and **not counted**: a packet that merely crossed a mode change is not evidence of cheating, and counting it would make the number jump every time the dial moves, which is exactly when somebody is reading it.

### The validator is a budget, not a per-claim allowance

This is the part that took three tries and is worth the space, because the two wrong versions both looked right.

**First version: measure each claim against the time since the last one.** Claims arrive between ticks, so two that bunch up measure zero elapsed against each other and the second is refused *for arriving together*. Jitter alone produced refusals, which destroys the only signal the design has.

**Second version: credit at least one tick of clock grain.** This fixed the false refusals and quietly opened a hole twice as bad as the thing it fixed: whatever you credit is a rate a client can claim at will. A client sending twice per tick got credited a full tick each time, and **a 2.0x speed cheat passed at a full 2.00x**. The table above is what caught it; the code looked completely reasonable.

**Third version, and the correct shape: a budget that accrues from the clock and is spent by movement.** It cannot be gamed by asking more often, because it accrues from elapsed time alone however many times it is asked. Two bunched packets are fine (they spend one budget between them), and two whole steps in no elapsed time are refused, because that is not a bunched packet, it is twice the speed.

The budget is capped at three seconds of travel. Uncapped, a disconnection is a teleport: five minutes of silence would earn the width of the zone several times over. The cap is a compromise and costs exactly what it says, which is that a client returning from a longer stall gets snapped back once.

**What a refusal does not do is stop anyone.** Measured on the wire: hammering a teleport for 20 seconds gained **180 units against the 187 an honest runner covers**, and logged **591 refusals** doing it. The cheat lands a big jump rarely instead of a small one often, ends up behind, and is loud the entire time. Being loud is the whole of the defence.

## What it costs, since the complexity argument says nothing about bytes

`cargo test -p gow_3d --test wire_cost -- --nocapture`

Everything above is an argument about *complexity*: this genre needs almost no netcode. Bytes are a separate question, and a frame here is built per client rather than broadcast, which is the price the design pays for the relevance it gets.

```
     in zone    in view        bytes          KiB/s
           8          8          220            6.4
          16         16          422           12.4
          32         25          647           19.0
          64         31          797           23.3
```

**Eight times the zone is 3.6x the frame**, because the view saturates: at 64 characters only 31 are in range, and past that a bigger zone is not a bigger frame. That is relevance working, and it is the only thing that makes a per-client frame affordable at all. Without it, building N frames that each cost what a broadcast costs would be strictly worse than broadcasting.

The second channel prices out at **6 bytes per party member the distance query missed**, which is the answer to "surely a second relevance channel doubles the work". It does not, because the union means it only ever carries what the first one dropped.

A cast bar costs about 2 bytes on the characters that have one: sixteen characters casting at once took the frame from 422 bytes to 454. The headline feature of the example is a field, not a frame.

Server-side, the total is what a zone budget is made of, and it is worth **measuring rather than multiplying**:

```
    measured        1068 KiB/s
    estimated       1494 KiB/s   (one client's frame times 64)
    per client  422 bytes at the thinnest, 797 at the busiest
```

One client's frame times the client count overstates it by 40%, because every client has a different audience: the characters out at the rim of the spiral see fewer people than the ones in the middle, and a per-client design is exactly the design where "times N" is the wrong arithmetic. This README carried the multiplied figure for one commit before the measurement replaced it, which is the same mistake this tree keeps relearning in smaller print.

## Tab targeting removes the thing two machines would disagree about

A projectile has to be agreed about. Two machines must decide whether a moving thing met another moving thing, they hold different ideas of where both were, and they disagree by exactly the round trip. That is the problem `hit_scan` and `puck_rink` exist to solve, and it is real work.

A named target is not that problem. The client says who it is aiming at, and when the cast lands the server does **one range check, at one instant, on positions it already has**. There is no projectile in flight for anyone to disagree about, and no rewind, no lag compensation and no hit registration to argue over. This is the third leg of the genre's latency argument and by far the cheapest.

The range is checked when the cast **lands**, not when it starts, which is the ordinary case rather than an edge one: a target that walks out of reach during a one-and-a-half second bar is what a fight looks like. Checking at the start would only move the same decision earlier and make it wrong more often.

A landing takes 12 off, and a character brought to zero goes **down** rather than away: out of view, unable to act, and back up three seconds later where it stands. That exists for one reason beyond making the health bars mean something. It is the third way somebody leaves your frame, after walking away and disconnecting, and the one that separates the two relevance channels most visibly: **a downed party member stays in the party frame at zero health while their body leaves the world.** A client with one channel cannot draw that, and a client that treated absence as "gone" would delete the entry that is telling you to go and help them.

## Two channels of relevance, because an MMO asks two questions

`cargo test -p gow_3d relevance -- --nocapture`

**Spatial**: who is near me. That is `SpatialGrid`, it is what every example in this tree uses, and it is rebuilt every tick because everyone moves.

**Subscription**: who have I chosen to care about, wherever they are. Your party's health bars update across the zone, and a guild roster is not a distance query at all. Nothing in plaza had a concept of it when this example was written, which is what the example was for: [`plaza_server_utils::subscription`](../../server_utils/API_REFERENCE.md) now exists because building this forced its shape. gow_3d still carries its own `Parties`, from before the block did.

The two are different shapes as well as different questions. A grid query is a fresh answer every tick over a set that changes constantly; a party is five entries with a lifetime of an hour. Expressing a party as a relevance radius means an infinite radius; expressing a grid query as a subscription means resubscribing everybody every tick.

```
  a party of 5 against a view of 40:

    0 of them nearby: 41 near, 4 added by subscription
    2 of them nearby: 43 near, 2 added by subscription
    4 of them nearby: 45 near, 0 added by subscription
```

The union is what keeps the second channel cheap: it costs only the members distance missed, and nothing at all for the ones standing beside you.

This is why `Because::{Near, Subscribed, BothOfThose}` is on the wire. Distance and subscription are different *promises*: the neighbour vanishes when you walk away and the party member does not. A client that cannot tell them apart cannot draw a party frame for somebody out of view, which is the entire feature.

## The ground is a rule, not a payload

`terrain.rs` derives the height of any point from its coordinates with three octaves of value noise over one seed, so a landscape of hills, coastline, rock and snow costs **nothing on the wire** and has no load step. Both ends run the same function: the client builds its mesh from it, and the server validates against it.

That second half is what makes the third movement rule possible. A speed budget cannot see a client hovering, because hovering costs no horizontal distance at all. A height rule can, and it is exact precisely because the ground is derived rather than sent:

- a claim further than the budget allows is refused,
- a claim outside the world is refused,
- a claim more than a jump's apex above the ground is refused.

Jumping is client-side physics, since the client owns its position. The apex falls out of `JUMP_SPEED` and `GRAVITY`, and `MAX_AIR` is that apex plus slack, so an honest jump is never refused and a flying client always is.

One thing the terrain changed about the validator itself: the budget is spent on **ground distance**, not on the 3D step. Charging the climb means walking up a slope is indistinguishable from running, and an honest player on a hillside gets refused for the crime of going uphill.

## Where spacemo's answer stops being free

`cargo test -p gow_3d --test tower -- --nocapture`

spacemo asked whether a volumetric grid earns its place and answered no: a flat `(x, z)` grid with a height filter is **exact at identical query cost**, because it touches the same cells and examines the same candidates. That was measured in open space. A stacked crowd is the arrangement it should be worst at.

```
        strategy     returned     examined     wasted
   flat + y band        202.9        720.0        72%
          volume        202.9        270.0        25%
```

Both return exactly the same people, so the filter is still exact. But it now examines **2.7x** what a volumetric grid does, because a flat cell holds every floor at once and 72% of what it pulls out is thrown away. The same 720 people on **one floor** put the two back level at 1.00x, which is what says this is about geometry rather than crowding.

So the recommendation holds with a boundary attached: **filter on height when things are spread out, and index the third axis when they stack.** This is the first time an answer from one example in this tree changed under another.

The first version of the scene was eight floors against a thirty metre view, so the volume grid's vertical reach covered the whole building and excluded nothing. A tower has to out-reach the view for the question to exist, and the test asserts that now.

### And what the running zone actually does

The same test file asks the same question of the real `Zone`, using the grid the server queries every tick:

```
   arrangement     examined     returned     wasted
    spread out         29.3         20.4        30%
       a tower         64.0         40.8        36%
```

Spread across a 240-metre world against a 46-metre view, the index is doing its job: a query looks at 29 of 64 characters, so most of the zone is never touched. Push the same people into one footprint and it examines all 64, **2.2x** the work for 2.0x the answer.

This is the second version of that measurement, and the first one is worth recording. When the world was an 80-metre tower the index excluded nobody at all, because the zone was smaller than a single query, and the test said so rather than being tuned until it agreed. Making the world bigger than the question is what turned `SpatialGrid` from cell arithmetic over a linear scan into something that pays for itself.

## What the tick actually does

Almost nothing, and that is the finding rather than a gap. Nobody's position is computed, because the clients own those, and the only thing with a clock is a cast bar. What is left is answering, once per client, who you are told about and why.

That per-client shape is the cost. One frame cannot be built and broadcast: two characters in different corners of the zone share nothing, and a party makes even neighbours differ.

## Seams worth knowing about

Every one of these is a place where both halves were individually correct.

- **Absence means two different things.** A neighbour missing from a frame has walked away and must be dropped; a party member missing has left the zone. The same wire event, separated only by `Because`.
- **A landing is an event.** No later frame mentions it, so a client that misses it misses it for good, and it is only sent to clients near enough to have a character for it. Otherwise a client plays an animation on nothing.
- **The client learns its spawn from the wire.** Computing it from the seat number would be a second derivation of one fact, and those drift.
- **Leaving the zone leaves the party**, or a health bar keeps updating for somebody who is not here.
- **A draw batch counted the wrong thing.** The renderer flushed every 64 bodies, which was right when a body was one box and wrong the moment it became eight: 18432 indices against macroquad's limit of 5000. Past that limit the batcher warns once and draws the front of the buffer, so characters were silently missing from the scene. The batch is now bounded by what is in the buffer, asked at every push, so the invariant holds however many boxes a body turns out to be.
- **The spawn ring wrapped.** A fixed angular step of 0.9 radians reaches 2π at seat 7, so seats 0 and 7 spawned on top of each other. It looked fine for the first handful, which is why the test checks all 64. A golden-angle spiral is the one step that never repeats.

## Layout

| file | what is in it |
| --- | --- |
| `terrain.rs` | the ground, derived from a seed on both ends |
| `abilities.rs` | three abilities: a cost, a wait, a range, an effect |
| `casting.rs` | cast times, the global cooldown, and what they are worth |
| `movement.rs` | the claim validator, and what it cannot do |
| `relevance.rs` | parties, and the union of the two channels |
| `zone.rs` | the characters, the beasts that hunt them, and the rules that hold it together |
| `bots.rs` | the adventurers the zone seats for itself |
| `protocol.rs` | the wire, including `Because` |
| `logic.rs` | the tick, which is mostly a send |
| `state.rs` | what the server owns |
| `net/` | both ends of the wire |
| `render.rs`, `ui.rs`, `main.rs` | the landscape, the bars, and the party frame |
| `tests/tower.rs` | where the height filter stops being free |
| `tests/mirror.rs` | both sides run together, which is where the seams show |
