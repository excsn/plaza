# 3DGoW

A small MMO zone on generated ground. The genre's design already hides most of the latency, so the game needs very little netcode.

The crate is `gow_3d` because Cargo rejects a package name beginning with a digit.

```sh
./run-native.sh          # play it and host it, in one window
./wasm-serve.sh          # the same thing in a browser, on port 8301
cargo test -p gow_3d     # the findings, as assertions
```

WASD walks, **space jumps**, **tab** cycles a beast to fight, **1** Strike, **2** Bolt, **3** Mend, **P** parties with the nearest adventurer and **O** leaves. `--bots N` sets how many adventurers the zone seats for itself; it seats eighteen beasts alongside them.

The zone is populated: two dozen adventurers hunt beasts across a landscape of hills, water and rock while you play. Ten seconds of a headless zone with nobody connected gives 104 casts landing and 8 characters coming back up.

Try walking away from somebody you are partied with. Their body leaves the world but their party entry stays, with a bearing and a height offset. That shows both relevance channels at work.

## What it is for

Compare it with `puck_rink`, which uses a full rollback setup (owned fixed-point arithmetic, per-frame digests, re-simulation of every confirmed frame) to hide a hundred milliseconds on five bodies. This example hides a hundred and fifty on sixty-four bodies and its netcode is a keypress and a report. It can do that because the genre's design already makes the player wait.

This example is more about game design than netcode.

## Cast bars and latency

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

People often say cast times hide latency. That is not quite right: the delay is the same 150ms at every cast time. What changes is its share of the wait and the share is what a player notices. At the cast times the genre uses, a bad connection is a smaller share of the wait than a good connection is for an instant ability.

The global cooldown covers the *inputs* the way a cast time covers the outcome: a player who cannot act again for 1500ms has no next input that needs frame-accurate timing. So instant abilities are covered too.

**The cooldown runs during a cast rather than after it.** The two waits overlap and only the longer one counts, so a long cast adds no cooldown on top. The first version of `zone.rs` assumed they stacked and the test caught it.

## Client authority

`cargo test -p gow_3d movement -- --nocapture`

Every other example in this tree is server-authoritative, because that is the right default and what plaza is built for. In this one the client says where it is and the server sanity-checks the claim. That gives smooth local movement without prediction, reconciliation or any correction to ease off.

```
  a second of running, against an honest 7.0 units:

     claimed     achieved       gain
        1.0x         6.72      1.00x
        1.1x         7.39      1.10x
        1.3x         8.74      1.30x
        2.0x         0.90      0.13x
       10.0x         0.00      0.00x
```

The cutoff is sharp. A cheat inside the tolerance works at exactly the rate it claims. A ten percent overrun looks the same as a late packet and a threshold tight enough to catch it throws out honest players on bad connections. Past the tolerance it collapses: a 2x claim achieves 13% of an honest run, because almost every claim is refused and the server keeps its own position. No setting separates 1.3x from a bad connection, because the server sees the same thing in both cases.

The example shows what client authority costs. It does not recommend it.

### Both modes, one build

The example was planned around comparing the two modes, so both are live in one build and the panel switches between them. Separate builds would mean comparing two sessions from memory.

```
       authority        gap now      worst gap     refusals
          client          0.00u          0.00u            0
          server          0.00u          0.00u            0
```

That is measured with **no simulated network delay**. Under client authority the gap is zero because the server takes the client's position. Under server authority it is also about zero: the server applies an intent for the whole tick it arrives in, so with no delay it is not behind the keys by more than a tick's rounding.

Under server authority the gap is the ground distance from where the held keys would have walked the character to where the server's latest frame has it. Nothing moves locally until the answer arrives, so it grows with the round trip at about run speed times the round trip. Measured with the same one-way delay on both legs:

`cargo test -p gow_3d --test gap_candidates -- --nocapture`

```
      scenario  delay        truth          gap        error
      straight    0ms    0.02/0.05    0.02/0.05    0.00/0.00
      straight   50ms    0.56/0.63    0.56/0.63    0.00/0.00
      straight  100ms    1.32/1.44    1.32/1.44    0.00/0.00
      straight  200ms    2.62/2.84    2.62/2.84    0.00/0.00
```

Each column is mean/max in units. Truth is the distance from where the keys would have taken the character with no delay to where the server has it. The test also runs a 90 degree turn, a stop and a stop-and-go. It holds the gap within 0.15u of truth in all of them. The largest error, 0.12u, is a tick's rounding left after a stop: the gap drops it once the keys are idle and the server has stopped. Truth keeps it.

Neither mode produced a refusal on an honest walk. Under server authority that is because no position is ever claimed. A claim sent anyway is refused and **not counted**, because a packet that crossed a mode change is not evidence of cheating. Counting it would make the number jump every time the dial moves, which is when somebody is looking at it.

### The movement budget

This took three tries. The two wrong versions both looked right.

**First version: measure each claim against the time since the last one.** Claims arrive between ticks, so two that bunch up measure zero elapsed time against each other and the second is refused *for arriving together*. Jitter alone produced refusals, which made the refusal count useless as a cheat signal.

**Second version: credit at least one tick of clock grain.** This fixed the false refusals but opened a worse hole: any fixed credit is a rate a client can claim at will. A client sending twice per tick got a full tick of credit each time, so **a 2.0x speed cheat passed at 2.00x**. The table above caught it; the code looked reasonable.

**Third version, the one that ships: a budget that accrues from the clock and is spent by movement.** Sending more often gains nothing, because the budget grows only with elapsed time. Two bunched packets are fine because they spend one budget between them. Two whole steps in no elapsed time are refused, because that is twice the speed.

The budget is capped at three seconds of travel. Without the cap, five minutes of silence would earn the width of the zone several times over and a reconnecting client could teleport. The cost of the cap is that a client returning from a longer stall gets snapped back once.

**A refusal does not stop the client moving.** Measured on the wire: hammering a teleport for 20 seconds gained **180 units against the 187 an honest runner covers** and logged **591 refusals** doing it. The cheat lands an occasional big jump instead of steady small ones, ends up behind and racks up refusals the whole time. The refusal count is this mode's only defence.

## What it costs in bytes

`cargo test -p gow_3d --test wire_cost -- --nocapture`

The sections above are about *complexity*: this genre needs almost no netcode. Bytes are a separate question. A frame here is assembled per client from **shared cell payloads**: the spatial channel is packed once per occupied grid cell and each client gets the payloads its view touches. That keeps the build cost from growing with the client count. The byte cost is that relevance is cell-granular instead of a disc.

```
     in zone    in view        bytes          KiB/s
           8          8          179            5.2
          16         16          309            9.1
          32         25          537           15.7
          64         27          793           23.2
```

**Eight times the zone gives 4.4x the frame**, because the view saturates: at 64 characters only 27 are in the disc and a bigger zone past that does not make a bigger frame. The bytes roughly doubled when the frame moved from a per-client disc to shared cells, since a cell window is a superset of the disc; `examples/crowd_techniques.rs` measured that overhead at 1.79x on a spread zone falling to 1.01x on a packed one, because in a crowd the cell window holds nearly the same people as each viewer's disc.

The audience is **hand-packed into bits** ([`src/pack.rs`](src/pack.rs)) while the envelope stays MessagePack, which is the division cube_yard and spacemo both arrived at. A character is 13 bytes rather than the 42 a derive spends on it: positions quantised to 4mm over bounds wider than the map, a 10-bit heading and no relevance tag at all, since a payload shared by every viewer of a cell cannot say why any one of them is being told. Why somebody is in your frame is derived at decode from which channel carried them.

The second channel costs **14 bytes per party member the distance query missed**. It does not double the work, because it only carries what the cells dropped. That figure used to read 6 and was wrong: the test moved four members out of view and into a party in one step, so the audience count never changed and what it measured was MessagePack writing `"Subscribed"` where it had written `"Near"`. The six bytes were the difference in length between those two words.

A cast bar costs about a byte on the characters that have one: sixteen characters casting at once took the frame from 309 bytes to 327.

Server-side, the zone budget is the total across all clients. Here it is measured and compared with one frame multiplied by the client count:

```
    measured         951 KiB/s
    estimated       1487 KiB/s   (one client's frame times 64)
    per client  345 bytes at the thinnest, 797 at the busiest
```

One client's frame times the client count overstates it by 56%, because every client has a different view: the characters out at the rim of the spiral touch emptier cells than the ones in the middle. With per-viewer assembly, multiplying by N gives the wrong answer. This README carried the multiplied figure for one commit before the measurement replaced it. Other examples in this tree have made the same mistake.

## Tab targeting

With a projectile, two machines have to agree whether one moving thing hit another. Each holds a different idea of where both were, off by the round trip. `hit_scan` and `puck_rink` exist to solve that and it takes real work.

A named target avoids that problem. The client says who it is aiming at and when the cast lands the server does **one range check, at one instant, on positions it already has**. There is no projectile in flight to disagree about, so there is no rewind, lag compensation or hit registration. This is the third part of the genre's answer to latency and by far the cheapest.

The range is checked when the cast **lands** rather than when it starts, because a target walking out of reach during a one-and-a-half second bar is an ordinary part of a fight. Checking at the start would make the same decision earlier and get it wrong more often.

A landing takes the ability's damage off: 9 for Strike, 26 for Bolt and 7 for a beast's Claw. A character brought to zero goes **down**: unable to act, out of view once its 1.6-second fall has played and back up six seconds later at its spawn point. Besides giving the health bars a purpose, going down is the third way somebody leaves your frame, after walking away and disconnecting. It separates the two relevance channels most visibly: **a downed party member stays in the party frame at zero health while their body leaves the world.** A client with one channel cannot draw that. A client that treated absence as "gone" would delete the party entry for the person who needs help.

## Two channels of relevance

`cargo test -p gow_3d relevance -- --nocapture`

**Spatial**: who is near me. That is `SpatialGrid`, it is what every example in this tree uses and it is rebuilt every tick because everyone moves.

**Subscription**: who have I chosen to care about, wherever they are. Your party's health bars update across the zone and a guild roster is not a distance query at all. Plaza had no concept of it when this example was written. [`plaza_server_utils::subscription`](../../server_utils/API_REFERENCE.md) now exists because this example needed it and its shape came from here. gow_3d still has its own `Parties`, written before that block existed.

The two also have different shapes. A grid query is a fresh answer every tick over a set that changes constantly; a party is five entries that last about an hour. Modelling a party as a relevance radius would need an infinite radius, while modelling a grid query as a subscription would mean resubscribing everybody every tick.

```
  a party of 5 against a view of 40:

    0 of them nearby: 41 near, 4 added by subscription
    2 of them nearby: 43 near, 2 added by subscription
    4 of them nearby: 45 near, 0 added by subscription
```

Taking the union keeps the second channel cheap: it only pays for the members the distance query missed.

This is why every entry a client decodes carries `Because::{Near, Subscribed, BothOfThose}`. Distance and subscription behave differently: a neighbour vanishes when you walk away and a party member does not. A client that cannot tell them apart cannot draw a party frame for somebody out of view. The label is no longer written per entry on the wire, because a cell payload is shared by every viewer of that cell and cannot carry a per-viewer answer: cell entries decode as `Near`, the per-client extras as `Subscribed` and a `party` seat list on the frame upgrades a near member to `BothOfThose`.

## Derived ground

`terrain.rs` derives the height of any point from its coordinates with three octaves of value noise over one seed, so a landscape of hills, coastline, rock and snow costs **nothing on the wire** and has no load step. Both ends run the same function: the client builds its mesh from it and the server validates against it.

Because the server validates against the same ground, a third movement rule is possible. A speed budget cannot catch a client hovering, because hovering covers no horizontal distance. A height rule can and it is exact because the ground is derived rather than sent:

- a claim further than the budget allows is refused,
- a claim outside the world is refused,
- a claim more than a jump's apex above the ground is refused.

Jumping is client-side physics, since the client owns its position. The apex falls out of `JUMP_SPEED` and `GRAVITY` and `MAX_AIR` is that apex plus slack, so an honest jump is never refused and a flying client always is.

The terrain also changed the validator: the budget is spent on **ground distance** rather than the 3D step. Charging for the climb would make walking up a slope look like running, so an honest player on a hillside would get refused.

## Height filter against a volumetric grid

`cargo test -p gow_3d --test tower -- --nocapture`

spacemo asked whether a volumetric grid is worth having and found it was not: a flat `(x, z)` grid with a height filter is **exact at the same query cost**, because it touches the same cells and examines the same candidates. That was measured in open space. A stacked crowd is where the flat grid should do worst.

```
        strategy     returned     examined     wasted
   flat + y band        202.9        720.0        72%
          volume        202.9        270.0        25%
```

Both return exactly the same people, so the filter is still exact. But it now examines **2.7x** what a volumetric grid does, because a flat cell holds every floor at once and 72% of what it pulls out is thrown away. The same 720 people on **one floor** bring the two back level at 1.00x, so the difference comes from the stacking rather than the crowd size.

So spacemo's recommendation holds with a limit: filter on height when things are spread out and index the third axis when they stack. This is the first time a result from one example in this tree changed under another.

The first version of the scene was eight floors against a thirty metre view, so the volume grid's vertical reach covered the whole building and excluded nothing. The tower has to be taller than the view reaches for the comparison to mean anything and the test now asserts that.

### The running zone

The same test file asks the same question of the real `Zone`, using the grid the server queries every tick:

```
   arrangement     examined     returned     wasted
    spread out         29.3         20.4        30%
       a tower         64.0         40.8        36%
```

Spread across a 240-metre world against a 46-metre view, the index works: a query looks at 29 of 64 characters, so most of the zone is never touched. Push the same people into one footprint and it examines all 64, **2.2x** the work for 2.0x the answer.

This is the second version of that measurement. When the world was an 80-metre tower the index excluded nobody, because the zone was smaller than a single query. The test reported that rather than being tuned until it agreed. Once the world was bigger than a query, `SpatialGrid` started excluding characters instead of doing cell arithmetic over what amounted to a linear scan.

## What the tick does

Very little, as expected. Under the default client authority no adventurer's position is computed, because the clients own those; the server moves only the beasts and the clocks it runs are cast bars, cooldowns and downed characters coming back up. The rest is working out, once per client, who that client is told about and why.

One frame cannot be broadcast to everyone, because two characters in different corners of the zone share nothing. The spatial channel does not have to be built per client either. `Zone::publish_at` packs each occupied grid cell once and a client's frame is the payloads its view touches plus a small per-client remainder: `you`, the party's extras, the landings it can see. The build tracks the occupied-cell count instead of the client count.

## What a zone costs when it is not small

`cargo run -p gow_3d --release --example zone_scale`

The example is played at 64 characters, which does not show whether the design holds up at an MMO's population. This sweep measures bytes **and** tick time, because they grow on different curves and only one of them hits a hard limit. Every character is a connected client, which is the worst case: a zone of bots costs nothing per bot, since a frame is built for the agents holding sockets and a bot has none. `MAX_CHARACTERS` is a default now rather than a cap; `GowState::with_capacity` takes any.

Both delivery modes run through `process_input`, because `Cells` does its addressing inside the tick and an arm calling the assembly directly would measure only the mode that does not. `encode` charges one pass per `TargetedOp`, which is what the session layer spends; `B/client` counts what crosses the wire, so a shared payload is charged to every recipient. Two more dials, `Precision::{CellRelative, Graded}`, move bytes rather than time and are covered below.

```
  in zone   in view    scheme   B/client       build      encode        tick  of budget
       64        44 joined            630       74.9µs       34.6µs      109.6µs       0.3%
       64        44 cells             876       45.3µs       27.2µs       72.5µs       0.2%
      256        44 joined            845      250.5µs      110.4µs      360.9µs       1.1%
      256        44 cells            1188      136.9µs       80.5µs      217.4µs       0.7%
     1024        44 joined            958      796.8µs      318.0µs     1114.8µs       3.3%
     1024        44 cells            1347      455.7µs      268.8µs      724.5µs       2.2%
     4096        44 joined           1014     3469.9µs     1261.5µs     4731.4µs      14.2%
     4096        44 cells            1428     1824.2µs     1056.9µs     2881.1µs       8.6%
```

**One zone holds 4096 connected clients in 14% of a 30Hz tick on one core (9% fanned out).** The view saturates at 44, so cost per client is flat and the tick grows linearly with clients. Population is not what limits this genre's netcode.

Crowding was the limit and it is measured separately. The same 256 people, packed until everyone can see everyone:

```
  spacing   in view    scheme   B/client       build      encode        tick  of budget
      7.0        44 joined            845      169.4µs       76.8µs      246.2µs       0.7%
      7.0        44 cells            1188      113.5µs       67.5µs      181.0µs       0.5%
      3.5       171 joined           2170      100.0µs       88.5µs      188.5µs       0.6%
      3.5       171 cells            2444       87.5µs       57.8µs      145.3µs       0.4%
      1.5       256 joined           3326       82.2µs       89.1µs      171.2µs       0.5%
      1.5       256 cells            3456       77.3µs       55.0µs      132.3µs       0.4%
      0.5       256 joined           3314       73.1µs       90.1µs      163.2µs       0.5%
      0.5       256 cells            3351       71.6µs       52.5µs      124.2µs       0.4%
```

**The tick column falls as the crowd tightens.** Under a per-client frame this table was the example's one real limit: it ran 675µs to 3504µs over the same spacings and population headroom did not help, because it is the same people in a smaller field. It now runs 246µs to 163µs joined and 181µs to 124µs fanned out: **up to 28x better at the packed end** and a 6x change in how many people are in view moves the tick *down* by a third. A tighter crowd is fewer occupied cells, a cell is packed once however many people look at it and every viewer in a cell shares one assembled blob.

**Pick the delivery mode by bandwidth rather than CPU.** The fan-out is faster everywhere by a fairly flat margin, but what it costs in bytes swings with density: **41% more per client on a spread zone** (1014 to 1428) and **1% more when packed** (3314 to 3351). Spread out, a client's 49 cells hold about one body each, so 49 op envelopes are nearly all framing; packed, the same envelopes carry a crowd apiece and framing is a small share of the bytes. `Joined` suits a thin world and `Cells` a dense one, so the panel offers both.

### Optimisation history and measurement errors

Each step is listed. Every wrong turn came from a measurement that looked convincing at the time.

| change | what it was | at 4096 |
|---|---|---|
| allocations | a `Vec` per grid query, a `HashSet` per client, a cloned `landed`, a party walked twice | 79.6% to 73.1% |
| dense seats | `HashMap<Seat, Character>` hashing a dense index, behind a map-shaped surface so no call site moved | 73.1% to 43.6% |
| packed audience | written straight into bits, no `Vec<Seen>` in between | 43.6% to 41.7% and 3.2x fewer bytes |
| published per cell | each occupied cell packed once instead of each client's view | 41.7% to 39.6%, a **wash** |
| joined and flat-keyed | one self-delimiting byte string per client, payloads in a `Vec` rather than hashed | 39.6% to 14.6% |
| re-keyed by viewer-cell | one blob per occupied viewer-cell, refcounted; addressing by cell pair | 14.6% to **14.2%** and 8.6% fanned out |

**The packing saved bandwidth but not CPU**, contrary to the prediction. Encode fell 5.2x and bytes 3.2x but the *total* went up, because quantising three positions and two varints costs more arithmetic than MessagePack spends writing them raw. The cost moved from the codec into the packer. Removing the intermediate `Vec<Seen>` made it a net win.

**Publishing per cell was a wash for the same reason.** `build` fell 1.56x while `encode` rose 4.3x and cancelled the gain, because a frame carried up to 49 byte strings instead of one and each paid its own framing. Joining them and keying the lookups flat recovered it. In both rows the column being optimised improved and the total did not, so check the total.

**The last row fixed four costs with one cause.** The window walk, the assembly copy, the audience push and the graded width test were all O(viewers) work on information that varies only per cell: two viewers in one cell touch the same cells and are owed identical bytes. Re-keying the layer by the viewer's cell fixed all four and the gain tracks clients-per-cell, since that is how many viewers were repeating the same work.

The table leaves out the alternatives priced and declined before that row: rest detection loses here because nobody in a zone is at rest (2x the build for 0.2% of the bytes) and the aggregation tree does not fit at this radius. They are in [`crowd_techniques`](examples/crowd_techniques.rs) and [`crowd_lod`](examples/crowd_lod.rs).

**Two sweeps measured pile-ups before they were caught.** The first sweep grew a population without growing the world, so `footing_near` stacked 1024 and 4096 characters on one spot and the "scaling wall" was a crowd. The second placed them correctly but left the *index* sized to `terrain::EDGE` and at constant density the spiral passes 120 units between 256 and 1024, so at 4096 **56% of the population sat in the border cells with one holding 490 bodies**. Both showed the same sign: a column that should not change did. `Zone::spanning` fixes the second and `a_body_past_the_index_rides_a_border_cell_into_a_frame` pins it.

**The harness overestimated twice for the same reason.** [`publish_costs`](examples/publish_costs.rs) priced the fan-out at 2.73x and the tick moved 1.75x, because it timed the *delivery stage* and a tick also runs the simulation, `you_of`, the extras and the landing filter for every client. It priced cell-relative packing at 10-12% and the wire gave 0-9%, because it never wrote down the cell index a reader needs. A ratio measured on one stage is only an upper bound for the whole pipeline. Both arms now round-trip through the shipped reader.

## Other delivery schemes, measured

`cargo run -p gow_3d --release --example publish_costs`

The candidates the sweep above pointed at are measured here. Every arm is charged for the whole per-client path (choose the cells a view touches, find their payloads, assemble, encode). An earlier revision hoisted the window walk out of the timed region and priced the flat index on bucketing and got two conclusions wrong as a result.

Totals per tick, in microseconds, for the same zone under five delivery schemes. `per/cel` is clients per occupied cell, which decides how the last scheme does:

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

The last two columns are the same scheme. `ops (hashed)` builds its recipient lists and does its payload lookups through a `HashMap` while every other arm was given a flat `Vec`; `ops + flat` is the fair comparison. The flat version is **8x faster on that arm's build alone** (777µs to 97µs at spread 1024) and it turns a scheme that appeared to lose below three clients per cell into one that wins at every density measured, by 2.7x to 8.9x over the best alternative and 6.1x to 14.6x over what ships.

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

Look at the fan-out's two columns rather than its total: its encode is only 1.17x better than `joined + flat` and its **build is 6.7x better**, because it never copies payload bytes into a per-client buffer. The bytes still reach every client, but they are never assembled per client.

Publishing itself is cheap: 31µs at 256 spread, 313µs at 4096 spread, 162µs at 4096 packed, shared by every arm. Concatenating the payloads into one self-delimiting byte string is what takes encode down, because 48 of 49 MessagePack envelope framings disappear. Indexing payloads by cell in a `Vec` rather than hashing a Morton key is worth another 1.39x, on ~50k lookups a tick. Holding each client's cell window until it crosses a boundary looks promising but gains little: windows go stale on **6.6-6.9%** of ticks at every population and density measured and holding them is worth 1.00-1.05x, because deciding *which* cells is cheap and the payloads change every tick anyway.

**Population does not change which scheme wins and occupancy only changes the margin.** Across 256 to 4096 clients at constant density the fan-out beats the best alternative by 2.69x, 2.77x and 2.88x, barely moving over a 16x range, because a spawn spiral at fixed spacing adds people and cells together and leaves clients-per-cell alone. So raising `MAX_CHARACTERS` does not change which scheme to use. Crowding does change the margin, from 2.7x sparse to 8.9x packed, since a per-client scheme pays per client and a per-cell scheme pays per cell.

An earlier revision of this section claimed a crossover at just under three clients per cell and concluded the schemes were for different worlds. That came from the `HashMap` handicap described above.

Two other numbers in the table were first measured wrong. The fan-out read 9.4x until it was charged for inverting client-to-cells into cell-to-clients, which is what `MessageTarget::Agents` needs. The flat index read as a decline until it was priced on lookups instead of on bucketing, where it wins 5.19x on 0.6% of the tick.

One saving is only possible with cell payloads: **a cell payload knows which cell it is**, so a position inside it can be written relative to the cell. It ships as `Precision::CellRelative` and **the harness overestimated it badly.** It predicted 10-12% of the bytes; measured end-to-end it is **1% to nothing on a spread zone and 9% when packed**:

```
          case   in view         joined          cells
     64 spread        44          0.98x          0.99x
   1024 spread        44          1.00x          1.00x
   4096 spread        44          1.00x          1.00x
   3.5 spacing       171          0.93x          0.94x
   1.5 spacing       256          0.91x          0.91x
   0.5 spacing       256          0.91x          0.91x
```

Two things reduced it and the harness missed both because it priced only the bodies. First, **a payload written relative to a cell must name the cell** and that index costs a varint per cell against ten bits saved per body, so it breaks even at about one body per cell; gow's own density is 1.6. Second, the saving is ten bits an axis pair rather than twelve, because a body may sit slightly outside the cell carrying it and the range needs padding: **12 bits over the padded range is coarser than the 18-bit absolute layout it replaces**. A const assertion caught that.

**`publish_costs` has since been fixed and now agrees with the wire.** Its packing arms write with the shipped packer and read back with the shipped reader, so an arm cannot price a format that will not decode. It now predicts 0.97-0.99x spread and 0.90-0.91x packed against a wire that measured 0.98-1.00x and 0.91x. **Grading the width by cell distance also shipped, as `Precision::Graded`. It is the one dial that does not pay for itself.** It saves a further 3-5 points of bytes (0.94-0.97x spread and 0.87-0.91x packed against absolute), because a cell beyond half the view radius can drop three bits an axis and stay well under a pixel. The cost comes from sharing: **a width cannot be chosen per viewer when the payload is shared**, so the zone publishes both widths and each viewer takes the one for its distance. Under `Joined` that is about 3% of the tick for 4-6% of the bytes, roughly a wash. Under `Cells` it **doubles the tick** (3101µs to 6284µs at 4096), because addressing has to split each cell's listeners into near and far, send two ops instead of one and measure a distance per listener per cell. It ships so the dial can be compared and it is off by default.

**Then the layer underneath was re-keyed, which brought graded's cost down.** Everything between publishing a cell and a client receiving bytes was keyed by *viewer* when the information varies only by *cell*: two viewers in one cell touch the same cells, are owed identical bodies and read every cell in the window at the same width. `Packed` is now refcounted, the body blob is assembled once per occupied viewer-cell and addressing walks cell pairs against a fixed offset mask rather than measuring a distance per listener per cell. `joined` at 4096 went **6141µs to 4724µs** and packed **301µs to 166µs**; `cells/grad` went **6326µs to 3615µs**, which takes graded from doubling the tick to costing about 7% when packed. The gain tracks clients-per-cell, like every other result in this section.

## Seams worth knowing about

In each of these, both halves were correct on their own.

- **Absence means two different things.** A neighbour missing from a frame has walked away and must be dropped; a party member missing has left the zone. Both look the same on the wire and only `Because` tells them apart.
- **A landing is an event.** No later frame mentions it, so a client that misses it never sees it. It is only sent to clients near enough to have a character for it; otherwise a client would play an animation on nothing.
- **The client learns its spawn from the wire.** Computing it from the seat number would derive the same fact twice and the two copies can drift apart.
- **Leaving the zone leaves the party.** Otherwise a health bar keeps updating for somebody who is not here.
- **A draw batch counted the wrong thing.** The renderer flushed every 64 bodies. That worked while a body was one box, but at eight boxes a body it reached 18432 indices against macroquad's limit of 5000. Past that limit the batcher warns once and draws the front of the buffer, so characters were silently missing from the scene. The batch now checks what is in the buffer at every push, so it stays under the limit however many boxes a body has.
- **The spawn ring wrapped.** A fixed angular step of 0.9 radians reaches 2π at seat 7, so seats 0 and 7 spawned on top of each other. It looked fine for the first handful, which is why the test checks all 64. Spawns now follow a golden-angle spiral, which never puts two seats at the same angle.

## Layout

| file | what is in it |
| --- | --- |
| `terrain.rs` | the ground, derived from a seed on both ends |
| `abilities.rs` | three abilities: a cost, a wait, a range, an effect |
| `casting.rs` | cast times, the global cooldown and what they are worth |
| `movement.rs` | the claim validator and what it cannot do |
| `relevance.rs` | parties and the union of the two channels |
| `zone.rs` | the characters, the beasts that hunt them, the rules and the per-cell publication |
| `bots.rs` | the adventurers the zone seats for itself |
| `protocol.rs` | the wire, including `Because` |
| `pack.rs` | the audience, written by hand into bits |
| `logic.rs` | the tick, which is mostly a send |
| `state.rs` | what the server owns |
| `net/` | both ends of the wire |
| `render.rs`, `ui.rs`, `main.rs` | the landscape, the bars and the party frame |
| `examples/zone_scale.rs` | what a zone costs at population and at crowding |
| `examples/crowd_lod.rs` | level of detail priced in pixels, at the camera that ships |
| `examples/crowd_techniques.rs` | the four things an MMO does about a crowd, priced here |
| `tests/tower.rs` | the height filter against a volumetric grid in a stacked crowd |
| `tests/mirror.rs` | both sides run together, to test the seams between them |
