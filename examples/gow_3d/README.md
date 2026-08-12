# 3DGoW

A zone of characters in a tower, and the netcode a game does not need because its design already absorbed the latency.

The crate is `gow_3d` because Cargo rejects a package name beginning with a digit.

```sh
./run-native.sh          # play it, and host it, in one window
./wasm-serve.sh          # the same thing in a browser, on port 8300
cargo test -p gow_3d     # the findings, as assertions
```

Arrows or WASD walk, **Q and E change floor**, 1 casts, 2 parties with the nearest character, 3 leaves. The panel switches who decides where you are, which is the comparison the example is built around. The thing worth doing is walking two floors away from somebody you are partied with: their body leaves the world and their entry stays, with a bearing and a floor offset. That is the whole argument of this example in one action.

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

## Two channels of relevance, because an MMO asks two questions

`cargo test -p gow_3d relevance -- --nocapture`

**Spatial**: who is near me. That is `SpatialGrid`, it is what every example in this tree uses, and it is rebuilt every tick because everyone moves.

**Subscription**: who have I chosen to care about, wherever they are. Your party's health bars update across the zone, and a guild roster is not a distance query at all. **Nothing in plaza has a concept of it**, and it is the one thing this example needs that the library does not offer.

The two are different shapes as well as different questions. A grid query is a fresh answer every tick over a set that changes constantly; a party is five entries with a lifetime of an hour. Expressing a party as a relevance radius means an infinite radius; expressing a grid query as a subscription means resubscribing everybody every tick.

```
  a party of 5 against a view of 40:

    0 of them nearby: 41 near, 4 added by subscription
    2 of them nearby: 43 near, 2 added by subscription
    4 of them nearby: 45 near, 0 added by subscription
```

The union is what keeps the second channel cheap: it costs only the members distance missed, and nothing at all for the ones standing beside you.

This is why `Because::{Near, Subscribed, BothOfThose}` is on the wire. Distance and subscription are different *promises*: the neighbour vanishes when you walk away and the party member does not. A client that cannot tell them apart cannot draw a party frame for somebody out of view, which is the entire feature.

## Where spacemo's answer stops being free

`cargo test -p gow_3d --test tower -- --nocapture`

spacemo asked whether a volumetric grid earns its place and answered no: a flat `(x, z)` grid with a height filter is **exact at identical query cost**, because it touches the same cells and examines the same candidates. That was measured in open space. A tower is the arrangement it should be worst at.

```
        strategy     returned     examined     wasted
   flat + y band        202.9        720.0        72%
          volume        202.9        270.0        25%
```

Both return exactly the same people, so the filter is still exact. But it now examines **2.7x** what a volumetric grid does, because a flat cell holds every floor at once and 72% of what it pulls out is thrown away. The same 720 people on **one floor** put the two back level at 1.00x, which is what says this is about geometry rather than crowding.

So the recommendation holds with a boundary attached: **filter on height when things are spread out, and index the third axis when they stack.** This is the first time an answer from one example in this tree changed under another.

The first version of the scene was eight floors against a thirty metre view, so the volume grid's vertical reach covered the whole building and excluded nothing. A tower has to out-reach the view for the question to exist, and the test asserts that now.

## What the tick actually does

Almost nothing, and that is the finding rather than a gap. Nobody's position is computed, because the clients own those, and the only thing with a clock is a cast bar. What is left is answering, once per client, who you are told about and why.

That per-client shape is the cost. One frame cannot be built and broadcast: two characters in different corners of the zone share nothing, and a party makes even neighbours differ.

## Seams worth knowing about

Every one of these is a place where both halves were individually correct.

- **Absence means two different things.** A neighbour missing from a frame has walked away and must be dropped; a party member missing has left the zone. The same wire event, separated only by `Because`.
- **A landing is an event.** No later frame mentions it, so a client that misses it misses it for good, and it is only sent to clients near enough to have a character for it. Otherwise a client plays an animation on nothing.
- **The client learns its spawn from the wire.** Computing it from the seat number would be a second derivation of one fact, and those drift.
- **Leaving the zone leaves the party**, or a health bar keeps updating for somebody who is not here.
- **The spawn ring wrapped.** A fixed angular step of 0.9 radians reaches 2π at seat 7, so seats 0 and 7 spawned on top of each other. It looked fine for the first handful, which is why the test checks all 64. A golden-angle spiral is the one step that never repeats.

## Layout

| file | what is in it |
| --- | --- |
| `casting.rs` | cast times, the global cooldown, and what they are worth |
| `movement.rs` | the claim validator, and what it cannot do |
| `relevance.rs` | parties, and the union of the two channels |
| `zone.rs` | the characters, and the rules that hold them together |
| `protocol.rs` | the wire, including `Because` |
| `logic.rs` | the tick, which is mostly a send |
| `state.rs` | what the server owns |
| `net/` | both ends of the wire |
| `render.rs`, `ui.rs`, `main.rs` | the tower, and the party frame |
| `tests/tower.rs` | where the height filter stops being free |
| `tests/mirror.rs` | both sides run together, which is where the seams show |
