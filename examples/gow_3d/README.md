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

Everything above is an argument about *complexity*: this genre needs almost no netcode. Bytes are a separate question. A frame here is assembled per client from **shared cell payloads**: the spatial channel is packed once per occupied grid cell and every client is handed the payloads its view touches, which is what keeps the build cost off the client count, and the byte price of it is that relevance is cell-granular rather than a disc.

```
     in zone    in view        bytes          KiB/s
           8          8          179            5.2
          16         16          309            9.1
          32         25          537           15.7
          64         27          793           23.2
```

**Eight times the zone is 4.4x the frame**, because the view saturates: at 64 characters only 27 are in the disc, and past that a bigger zone is not a bigger frame. The bytes roughly doubled when the frame moved from a per-client disc to shared cells, since a cell window is a superset of the disc; `examples/crowd_techniques.rs` measured that overhead at 1.79x on a spread zone collapsing to 1.01x on a packed one, because in a crowd a shared answer is nearly the right answer for everyone in it.

The audience is **hand-packed into bits** ([`src/pack.rs`](src/pack.rs)) while the envelope stays MessagePack, which is the division cube_yard and spacemo both arrived at. A character is 13 bytes rather than the 42 a derive spends on it: positions quantised to 4mm over bounds wider than the map, a 10-bit heading, and no relevance tag at all, since a payload shared by every viewer of a cell cannot say why any one of them is being told. Why somebody is in your frame is derived at decode from which channel carried them.

The second channel prices out at **14 bytes per party member the distance query missed**, which is the answer to "surely a second relevance channel doubles the work". It does not, because it only ever carries what the cells dropped. That figure used to read 6 and was wrong for an interesting reason: the test moved four members out of view and into a party in one step, so the audience count never changed and what it measured was MessagePack writing `"Subscribed"` where it had written `"Near"`. Six bytes was the length of a word.

A cast bar costs about a byte on the characters that have one: sixteen characters casting at once took the frame from 309 bytes to 327. The headline feature of the example is a field, not a frame.

Server-side, the total is what a zone budget is made of, and it is worth **measuring rather than multiplying**:

```
    measured         951 KiB/s
    estimated       1487 KiB/s   (one client's frame times 64)
    per client  345 bytes at the thinnest, 797 at the busiest
```

One client's frame times the client count overstates it by 56%, because every client has a different view: the characters out at the rim of the spiral touch emptier cells than the ones in the middle, and a per-viewer assembly is exactly the design where "times N" is the wrong arithmetic. This README carried the multiplied figure for one commit before the measurement replaced it, which is the same mistake this tree keeps relearning in smaller print.

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

This is why every entry a client decodes carries `Because::{Near, Subscribed, BothOfThose}`. Distance and subscription are different *promises*: the neighbour vanishes when you walk away and the party member does not. A client that cannot tell them apart cannot draw a party frame for somebody out of view, which is the entire feature. The label is no longer spelled per entry on the wire, because a cell payload is shared by every viewer of that cell and cannot carry a per-viewer answer: cell entries decode as `Near`, the per-client extras as `Subscribed`, and a `party` seat list on the frame upgrades a near member to `BothOfThose`.

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

One frame cannot be built and broadcast: two characters in different corners of the zone share nothing. But the spatial channel does not have to be built per client either. `Zone::publish` packs each occupied grid cell once, and a client's frame is the payloads its view touches plus a small per-client remainder: `you`, the party's extras, the landings it can see. The build tracks the occupied-cell count instead of the client count, which is the difference between a zone and a mailing list.

## What a zone costs when it is not small

`cargo run -p gow_3d --release --example zone_scale`

The example is played at 64 characters, which says nothing about whether the shape survives an MMO's population. This sweep answers that in bytes **and** in tick time, because those have different curves and only one of them is a wall. Every character is a connected client, which is the worst case: a zone of bots costs nothing per bot, since a frame is built for the agents holding sockets and a bot has none. `MAX_CHARACTERS` is a default now rather than a cap; `GowState::with_capacity` takes any.

Both delivery modes, through `process_input`, because `Cells` does its addressing inside the tick and an arm calling the assembly directly would measure only the mode that does not. `encode` charges one pass per `TargetedOp`, which is what the session layer spends; `B/client` counts what crosses the wire, so a shared payload is charged to every recipient.

```
  in zone   in view  delivery   B/client       build      encode        tick  of budget
        8         8    joined        167       15.5µs       17.1µs       32.6µs       0.1%
        8         8     cells        220       10.9µs        5.5µs       16.4µs       0.0%
       64        44    joined        616      111.7µs       40.4µs      152.1µs       0.5%
       64        44     cells        862       65.1µs       32.1µs       97.2µs       0.3%
      256        44    joined        831      394.5µs      129.9µs      524.4µs       1.6%
      256        44     cells       1174      185.6µs       90.8µs      276.4µs       0.8%
     1024        44    joined        944     1164.8µs      345.4µs     1510.3µs       4.5%
     1024        44     cells       1333      508.8µs      249.9µs      758.7µs       2.3%
     4096        44    joined       1000     3969.8µs     1095.4µs     5065.2µs      15.2%
     4096        44     cells       1414     1976.0µs      921.3µs     2897.3µs       8.7%
```

**One zone holds 4096 connected clients in 15% of a 30Hz tick joined, or 9% fanned out, on one core.** The view saturates at 44 and never moves again, so cost per client is flat and the tick is simply linear in clients. Population is not this genre's netcode problem.

Crowding was, and it is the separate axis. The same 256 people, packed until everyone can see everyone:

```
  spacing   in view  delivery   B/client       build      encode        tick  of budget
      7.0        44    joined        831      237.3µs       69.2µs      306.5µs       0.9%
      7.0        44     cells       1174      116.7µs       56.8µs      173.5µs       0.5%
      3.5       171    joined       2156      201.7µs       79.4µs      281.1µs       0.8%
      3.5       171     cells       2430      103.9µs       47.2µs      151.0µs       0.5%
      1.5       256    joined       3312      195.7µs       85.2µs      280.9µs       0.8%
      1.5       256     cells       3442      100.7µs       44.5µs      145.1µs       0.4%
      0.5       256    joined       3300      144.0µs       82.6µs      226.6µs       0.7%
      0.5       256     cells       3337       98.0µs       44.3µs      142.3µs       0.4%
```

**Read the tick column downward: it is flat, and slightly falling.** Under a per-client frame this table was the example's one real wall, going 675µs to 3504µs over the same spacings, five times the cost for the same population in a smaller field with no amount of population headroom helping. It now runs 307µs to 227µs joined, or 174µs to 142µs fanned out, **up to 24x better at the packed end**, and a 6x change in how many people are in view moves the tick less than 30%. A tighter crowd is fewer occupied cells, and a cell is packed once however many people are looking at it, so the case that used to be worst is now the case the shape is best at.

**Which delivery to pick is a bandwidth question, not a CPU one, and that is the surprise.** The fan-out is faster everywhere by a fairly steady **1.6x to 1.75x**, but what it costs in bytes swings enormously with density: **41% more per client on a spread zone** (1000 to 1414) and **1% more when packed** (3300 to 3337). The reason is the same one that makes the whole shape work. Spread out, a client's 49 cells hold about one body each, so 49 separate op envelopes are nearly all framing; packed, the same envelopes carry a crowd apiece and the framing disappears into the payload. So `Joined` is the better default for a thin world, `Cells` for a dense one, and the dial exists because a zone is both at different hours.

**The harness that argued for this overstated it, and the gap is instructive.** `publish_costs` measured the fan-out at 2.73x the joined path; live it is 1.75x. Nothing was wrong with either number: the harness timed the *delivery step*, and a real tick also runs the simulation, `you_of`, the party's extras and the landing filter for every client, none of which either mode can share. **A ratio measured on one stage of a pipeline is an upper bound on what it does to the pipeline**, and the shared remainder is what separates the two figures.

The standard answers were all measured before one was adopted. `examples/crowd_lod.rs` prices level of detail in screen pixels at the camera this example actually builds, and the aggregation tree from the horde work turns out to be the wrong tool at this radius (394 pixels of worst-case error, on bodies still 57 pixels tall at the view edge), while grading precision by distance is the same picture for fewer bits. `examples/crowd_techniques.rs` prices what MMOs actually do about a crowd; cell publication won and is what ships.

**The prediction and the live example agree to three figures**, which is the strongest evidence in the whole exercise. `crowd_techniques` said the cell window's byte overhead runs 1.79x on a spread zone and collapses to 1.01x on a packed one. The played example independently reproduced 1.79x at spacing 7.0 and 1.00x at spacing 1.5, from a different harness on a different code path.

**The first version of this sweep measured a pile and called it a wall.** The spawn spiral leaves the 232-unit map at 256 characters, and `footing_near` falls back to the origin when it finds nowhere to stand, so 1024 and 4096 stacked on one spot: 3607 in view and a tick 27x over budget. Growing a population without growing the world is a crowding sweep wearing the wrong label. The tell was in the data, since the in-view column stopped saturating and nothing in the netcode could have caused that.

**And the second version measured a different pile at a different boundary, which is why this paragraph is now two paragraphs.** Placing characters straight onto the spiral fixed the terrain fallback but left the *index* sized to `terrain::EDGE`, and at constant density the spiral reaches `7·√n`, so it passes 120 units between 256 and 1024. `GridQuantizer` clamps everything past its origin into the boundary cells: at 4096 that was **56% of the population in the border, and one cell holding 490 bodies against about three in an honest one**. The tell was the same shape as last time, a column that could not move doing so anyway, bytes per client growing 6x while the in-view count sat pinned at 44. The fix is `Zone::spanning`, and the sweep now sizes its index to the spiral it builds.

That second pile exposed a real property of the new shape rather than just a harness bug. **A shared payload has no per-viewer backstop: the cell window is a promise the index must keep alone.** The per-client build re-checked every candidate with `distance(...) <= VIEW`, and a filter of that shape cannot exist on bytes shared between viewers, since the payload is fixed before anyone's position is consulted. That check had been silently absorbing the quantizer's clamping the whole time, charging it to the waste counter. With an honest index the missing backstop costs only the bounded cell-granular superset priced above; under an index that lies, whatever it clamped into the border cells goes on the wire. An undersized index used to make a zone slow, and now makes it wrong. `a_body_past_the_index_rides_a_border_cell_into_a_frame` pins both halves.

Three changes took the tick from 79.6% of budget to 41.7% at 4096, with every test still passing:

| change | what it was | at 4096 |
|---|---|---|
| allocations | a `Vec` per grid query, a `HashSet` per client, a cloned `landed`, a party walked twice | 79.6% to 73.1% |
| dense seats | `HashMap<Seat, Character>` hashing a dense index, behind a map-shaped surface so no call site moved | 73.1% to 43.6% |
| packed audience | written straight into bits, no `Vec<Seen>` in between | 43.6% to **41.7%**, and 3.2x fewer bytes |
| published per cell | each occupied cell packed once, joined into one byte string per client, keyed by a flat `CellSpace` | 41.7% to **14.6%**, and crowding 10.5% to **0.7%** |

**The packing is a bandwidth win and not a CPU win**, which was not the prediction. Encode fell 5.2x and bytes 3.2x, and the *total* went up, because quantising three positions and two varints costs more arithmetic than MessagePack spends writing them raw: the cost moved from the codec into the packer rather than disappearing. Removing the intermediate `Vec<Seen>` is what made it a net win. Watch the total, not the column being optimised.

**The fourth change is the one the sweep argued for: the spatial channel is published per cell.** `Zone::publish` packs each occupied grid cell once and `frame_for` assembles each client's frame from the payloads its view touches, on `SpatialGrid::occupied` and `GridQuantizer::keys_in_radius` from `plaza_server_utils`. `crowd_techniques` priced the alternatives against a moving zone before this was adopted: rest detection loses here because nobody in a zone is at rest (2x the build for 0.2% of the bytes), grading the refresh rate by distance is lossy in time, and cell publication won. What it trades is cell-granular relevance, the byte cost measured above, and the per-entry relevance tag, which moved off the wire because a shared payload cannot carry a per-viewer answer.

**On the population axis it was at first a wash, and the reason was worth more than the win would have been.** At 4096 the tick moved 13912µs to 13201µs, five percent. `build` fell 12740µs to 8191µs (1.56x) while `encode` rose 1160µs to 5001µs (4.3x), so almost the whole gain was handed straight back one column to the right, because a frame carried up to 49 separate byte strings where it had carried one and each paid its own MessagePack framing. That is the same lesson the packing row taught, arriving by a different route: **watch the total, not the column being optimised.**

**Two changes since then took it the rest of the way, and both came out of `publish_costs`.** The cell payloads a client's view touches are **concatenated into one self-delimiting byte string** rather than sent as one field each, which is what `Delivery::Joined` means and which kills 48 of 49 envelope framings. And the publication is keyed by [`CellSpace`](../../server_utils/API_REFERENCE.md) in a flat `Vec` rather than hashed by Morton code, which matters because the assembly does ~49 lookups per client per tick and there are 200k of them at 4096 clients. Together, at 4096: **13201µs to 4880µs, 2.70x**, against a harness that predicted 2.73x. `encode` fell 5001µs to 1308µs and `build` 8191µs to 3561µs, so this time neither column paid for the other.

## What is left on the table, priced

`cargo run -p gow_3d --release --example publish_costs`

The sweep above split, so the candidates it pointed at are measured rather than argued. Every arm is charged for the whole per-client path (choose the cells a view touches, find their payloads, assemble, encode), which is the correction that matters: an earlier revision hoisted the window walk out of the timed region and priced the flat index on bucketing, and got two conclusions wrong for it.

Totals per tick, in microseconds, for the same zone under five delivery schemes. `per/cel` is clients per occupied cell, which is the column that decides the last one:

| case | people | cells | per/cel | frame now | joined | + flat | + held | ops (hashed) | **ops + flat** |
|---|---|---|---|---|---|---|---|---|---|
| spread | 256 | 155 | 1.7 | 812 | 412 | 304 | 299 | 332 | **111** |
| spread | 1024 | 652 | 1.6 | 2494 | 1277 | 914 | 900 | 1013 | **325** |
| spread | 4096 | 2643 | 1.5 | 10878 | 5739 | 3958 | 3911 | 4377 | **1360** |
| density | 1024 | 349 | 2.9 | 2621 | 1330 | 984 | 959 | 900 | **303** |
| density | 1024 | 173 | 5.9 | 2430 | 1316 | 987 | 966 | 818 | **285** |
| density | 1024 | 91 | 11.3 | 2605 | 1476 | 1125 | 1086 | 746 | **271** |
| packed | 256 | 14 | 18.3 | 362 | 262 | 205 | 200 | 144 | **60** |
| packed | 1024 | 42 | 24.4 | 2960 | 2064 | 1498 | 1477 | 667 | **267** |
| packed | 4096 | 135 | 30.3 | 15618 | 11098 | 9629 | 9541 | 3071 | **1068** |

The last two columns are the same scheme, and the difference between them is the correction that matters most here. `ops (hashed)` builds its recipient lists and does its payload lookups through a `HashMap` while every other arm was given a flat `Vec`; `ops + flat` is the fair fight. It is worth **8x on that arm's build alone** (777µs to 97µs at spread 1024), and it turns a scheme that appeared to lose below three clients per cell into one that wins at every density measured, by 2.7x to 8.9x over the best alternative and 6.1x to 14.6x over what ships.

And the bytes each client is sent per tick, which the delivery scheme barely moves (joining saves the envelope framings, the fan-out spends them):

| case | per/cel | frame now | joined / flat / held | per-cell ops |
|---|---|---|---|---|
| spread | 1.6 | 1015 | 930 | 1057 |
| density | 5.9 | 3171 | 3092 | 3209 |
| packed | 24.4 | 10327 | 10247 | 10354 |
| packed | 30.3 | 15916 | 15803 | 15953 |

The split between building and encoding, at the two extremes, since the whole reason to do this was an encode that had regressed 4.3x:

| case | scheme | build | encode | total |
|---|---|---|---|---|
| spread 1024 | frame now | 1328µs | 1165µs | 2494µs |
| spread 1024 | joined + flat | 646µs | 267µs | 914µs |
| spread 1024 | ops + flat | 97µs | 229µs | 325µs |
| packed 4096 | frame now | 7600µs | 8018µs | 15618µs |
| packed 4096 | joined + flat | 5676µs | 3954µs | 9629µs |
| packed 4096 | ops + flat | 348µs | 720µs | 1068µs |

Read the fan-out's two columns rather than its total: its encode is only 1.17x better than `joined + flat`, and its **build is 6.7x better**, because it never copies payload bytes into a per-client buffer at all. The bytes still reach every client; they are just never assembled per client on the way.

Publishing itself is never the cost: 31µs at 256 spread, 313µs at 4096 spread, 162µs at 4096 packed, shared by every arm. Concatenating the payloads into one self-delimiting byte string is what takes encode down, because 48 of 49 MessagePack envelope framings disappear. Indexing payloads by cell in a `Vec` rather than hashing a Morton key is worth another 1.39x, on ~50k lookups a tick. Holding each client's cell window until it crosses a boundary sounds like the big one and is not: windows go stale on **6.6-6.9%** of ticks at every population and density measured, and holding them is worth 1.00-1.05x, because deciding *which* cells is cheap and the payloads change every tick anyway.

**Population does not decide anything and occupancy decides only how much you win.** Across 256 to 4096 clients at constant density the fan-out beats the best alternative by 2.69x, 2.77x and 2.88x, essentially unmoved by a 16x range, because a spawn spiral at fixed spacing adds people and cells together and leaves clients-per-cell alone. Turning up `MAX_CHARACTERS` therefore never changes which scheme to use. Crowding does change the margin, from 2.7x sparse to 8.9x packed, since a per-client scheme pays per client and a per-cell scheme pays per cell.

An earlier revision of this section claimed a crossover at just under three clients per cell and concluded the schemes were for different worlds. That was an artefact of the handicap described above, and it is recorded here rather than quietly deleted because the shape of the mistake is worth more than the number was: **an unfair comparison does not produce a vague answer, it produces a sharp one that is wrong.**

Two other numbers in the table were measured wrong before they were measured right. The fan-out read 9.4x until it was charged for inverting client-to-cells into cell-to-clients, which is what `MessageTarget::Agents` needs. The flat index read as a decline until it was priced on lookups instead of on bucketing, where it wins 5.19x on 0.6% of the tick.

Separately, and the only saving here a per-client frame could never have had: **a cell payload knows which cell it is**, so a position inside it can be written relative to the cell. It ships as `Precision::CellRelative` and **the harness overpromised it by a factor of the whole win.** Predicted at 10-12% of the bytes; measured end-to-end it is **1% to nothing on a spread zone and 9% when packed**:

```
          case   in view         joined          cells
     64 spread        44          0.98x          0.99x
   1024 spread        44          1.00x          1.00x
   4096 spread        44          1.00x          1.00x
   3.5 spacing       171          0.93x          0.94x
   1.5 spacing       256          0.91x          0.91x
   0.5 spacing       256          0.91x          0.91x
```

Two things ate it, and neither was visible in a harness that priced only the bodies. **A payload written relative to a cell must name the cell**, and that index costs a varint per cell against ten bits saved per body, so it breaks even at about one body per cell and gow's own density is 1.6. And the saving is ten bits an axis pair, not twelve, because a body may sit slightly outside the cell carrying it and the range needs padding: **12 bits over the padded range is coarser than the 18-bit absolute layout it replaces**, which a const assertion caught and a comment would not have. So it is a real saving that arrives exactly where bytes are already worst, and nothing at all where they are not.

**`publish_costs` has since been fixed and now agrees with the wire.** Its packing arms write with the shipped packer and read back with the shipped reader, so an arm cannot price a format that will not decode. Under that rule it predicts 0.97-0.99x spread and 0.90-0.91x packed against a wire that measured 0.98-1.00x and 0.91x. **Grading the width by cell distance then shipped too, as `Precision::Graded`, and it is the one dial that does not pay its way.** It saves a further 3-5 points of bytes (0.94-0.97x spread and 0.87-0.91x packed against absolute), because a cell beyond half the view radius can drop three bits an axis and stay well under a pixel. What it costs is the same shape as everything else here: **a width cannot be chosen per viewer when the payload is shared**, so the zone publishes both widths and each viewer takes the one its distance earns. Under `Joined` that is about 3% of the tick for 4-6% of the bytes, roughly a wash. Under `Cells` it **doubles the tick** (3101µs to 6284µs at 4096), because addressing has to split each cell's listeners into near and far, send two ops instead of one, and measure a distance per listener per cell. It ships because the dial is the deliverable, and it is off by default.

**Then the layer underneath all of it was re-keyed, and graded stopped being a disaster.** Everything between publishing a cell and a client receiving bytes was keyed by *viewer* when the information varies only by *cell*: two viewers in one cell touch the same cells, are owed identical bodies, and read every cell in the window at the same width. `Packed` is now refcounted, the body blob is assembled once per occupied viewer-cell, and addressing walks cell pairs against a fixed offset mask rather than measuring a distance per listener per cell. `joined` at 4096 went **6141µs to 4724µs** and packed **301µs to 166µs**; `cells/grad` went **6326µs to 3615µs**, which takes graded from doubling the tick to costing about 7% when packed. The gain tracks clients-per-cell, which by now is the only variable in this file that has ever decided anything.

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
| `zone.rs` | the characters, the beasts that hunt them, the rules, and the per-cell publication |
| `bots.rs` | the adventurers the zone seats for itself |
| `protocol.rs` | the wire, including `Because` |
| `pack.rs` | the audience, written by hand into bits |
| `logic.rs` | the tick, which is mostly a send |
| `state.rs` | what the server owns |
| `net/` | both ends of the wire |
| `render.rs`, `ui.rs`, `main.rs` | the landscape, the bars, and the party frame |
| `examples/zone_scale.rs` | what a zone costs at population, and at crowding |
| `examples/crowd_lod.rs` | level of detail priced in pixels, at the camera that ships |
| `examples/crowd_techniques.rs` | the four things an MMO does about a crowd, priced here |
| `tests/tower.rs` | where the height filter stops being free |
| `tests/mirror.rs` | both sides run together, which is where the seams show |
