# blackhole_playground

You are a black hole. Pellets drift toward you, slowly at the rim and faster the deeper they fall, and swallowing them makes you bigger. Running into a rival costs you mass, so the arena is a chase with a penalty for contact.

Underneath the game is one question: **thousands of pellets move entirely because of a handful of black holes.** So the server can send the *field* (a few positions and masses) and let every client integrate the pellets itself, or it can send thousands of pellet positions the conventional way. This example implements both and measures the difference.

It is deliberately the **hard** case. The [`horde_playground`](../horde_playground/) enemies home toward a target, so prediction errors shrink on their own; gravity is divergent, so they grow. A second consumer that is hostile to the technique is worth more than another flattering one.

## Running it

**The host is the server.** A native run hosts by default and its own player is just another client on a real socket, which is what keeps every omniscient readout it shows legitimate: it genuinely owns both sides. One `--role` argument decides what a process is, because the combinations are not independent and a flag soup would let you ask for contradictions.

```sh
./run-native.sh                                                  # --role host: play, and serve joiners
./run-native.sh -- --role observer                               # watch and drive the settings, no hole of your own
./run-native.sh -- --role client --connect ws://<host>:8080/ws   # join someone else's arena
./wasm-build.sh                                                  # build the browser client only
./wasm-serve.sh                                                  # build it and host it; open the printed URL
```

| `--role` | server | window | your own hole |
|---|---|---|---|
| `headless` | yes | no | no | the windowless deployable, and what `wasm-serve.sh` runs |
| `observer` | yes | yes | no | full control panel, watching; a free camera (drag / WASD / wheel, `C` recenters) |
| `host` | yes | yes | yes | plays and serves. **The default** |
| `client` | no | yes | yes | join only. The only role a browser can take |

A host prints a local URL and a LAN URL; open either in a browser to join, or send the LAN one to a friend. The browser client connects back to whoever served it (over `wss://` if the page was secure), so a `--role headless` deploy behind a TLS terminator works the same way. The impairment sliders (latency, jitter, loss) act on the **real** per-connection link and in **both** directions, so a host can show a joiner what 200 ms and 10% loss feel like.

**WASD / arrows** to move, **space** to dash; on a phone, touch and drag anywhere to steer.

The pure single-process teaching build, with no networking compiled in at all, is still here and is where the measurements below come from: `cargo run -p blackhole_playground --no-default-features --features native,client`.

Black holes pull *each other*, not just the pellets, so contact is sticky: drift too close and the attraction closes the rest of the distance for you and keeps it closed. Holes never pass through each other. They press, like two marshmallows being squeezed: whatever overlap the pull would have created is measured as *pressure* and then undone, leaving them exactly tangent. **Both** drain the whole time they are pressed, harder the harder the press, so dashing into someone bites more than drifting into them, and because draining shrinks their radii they stay in contact while getting smaller. Squeeze a rival to zero and they are **gone**; that is what merging is, and the survivor simply has the space to itself. Eliminated players return after a few seconds. You cannot walk out of a grapple, the pull at contact is tuned above walking speed, so a dash outruns it briefly and the pull starts eating the gap back at once. Breaking away usually takes a few.

Score and size are separate. **Score** is pellets eaten and only goes up. **Mass** is the physical stat: it sets your size *and the strength of your gravity*, and it drains continuously while you are in contact with a rival. Mass has no ceiling; instead its effect is log-damped (`scale * ln(1 + mass/scale)`), so growth is near-linear while you are small and flattens as you get large. Diminishing returns rather than a wall, and nothing to hit.

## What you are looking at

Bright pellets are where **your client** thinks they are; the faint ones underneath are the **server ghost**, and the gap between them is divergence. A hollow ring shows where the server really has *your* hole, which is the one entity drawn anywhere other than where the authority puts it. Watch that ring open during a grapple and close when you break away: collision separation between holes is deliberately left unpredicted, and the ring is where that residual lives. The ghost is on by default and has a switch, in every role. A host's ring is the server's state now, so its gap is prediction error alone; a joiner's is the newest sample it received, which is received state rather than a privilege, so its gap is that error plus how stale the sample is. The faint *pellets* are the one part a joiner genuinely cannot have, because under field sync pellet positions are never sent at all, which is the entire point of the mode.

**This ghost means something different from [horde's](../horde_playground/), because the two clients have different architectures**, and it is worth knowing rather than flattening. Horde buffers packets and plays them out on a render clock, so its ghost is the *future* it already holds and the gap is the playout delay. This client applies a packet on arrival and predicts forward from it, so its ghost sits *behind* the marker and the gap is prediction error. Same word, opposite side, because one client renders in the past and the other predicts into the present. Each hole is drawn as a wide disk (where the pull begins, and the body rivals collide with) around a dark core (where a pellet is actually swallowed), so you can watch pellets accelerate through the well rather than vanish at the rim.

| Control | What it shows |
|---|---|
| **the field / the particles** | the headline comparison: a few hole states, or every visible pellet |
| **corrections per packet** | under field sync, how much of the budget goes to refreshing pellets |
| **correct the deepest first** | a targeting policy that measurement says is much worse than plain rotation |
| **cull the field by view distance** | the deliberate mistake: gravity is long range, so hiding a distant hole makes local physics wrong |
| **aggregate the far field (angle)** | the third option: distant holes are replaced by one stand-in at their centre of mass, so nothing is deleted, only blurred. Turn the crowd up to 64 first, then compare it against culling |
| **latency / jitter / loss** | real impairment on real connections, in both directions. Delivery stays ordered, because the transport underneath is TCP and an impairment link that reorders invents failures the real one cannot |
| **predict the dash burst** | on by default; turn it off to feel the cost of leaving an ability unpredicted. Two shadow predictors run the same gameplay differing only in this flag, so the readout answers whether it earns its keep without you having to remember how the last run felt |
| **server ghost** | the authoritative pellets and your hole's real position, drawn faintly underneath. On by default, and this is the example that needs it most: the hole is a *forced* entity, so its prediction is the hardest thing here and three separate bugs in it once wore one symptom |

## What it measured

3000-unit arena, 2000 pellets, four players, 10 Hz, 80 ms latency.

**Sending the field is far cheaper, and the typical pellet is no worse.**

| mode | KiB/s | states/packet | median err | p90 | mean |
|---|---|---|---|---|---|
| field (a few holes) | **20.2** | 40 | **40.2 px** | 519 | 199 |
| particles (visible set) | 146.0 | 336 | 74.3 px | **120** | **82** |

Seven times cheaper, with a median almost twice as good. What field sync buys in bandwidth it pays for in the **tail**: p90 of 519 px against 120. A minority of pellets, the ones falling through a core where acceleration is extreme, diverge chaotically. Judge this with percentiles, not a mean; the mean here describes a handful of outliers, not the pellet you are looking at.

**Corrections bound the drift, and the budget is the dial.**

| corrections/packet | every pellet refreshed | median | p90 | KiB/s |
|---|---|---|---|---|
| 0 | never | 49.0 | 157 | 3.8 |
| 40 | 5.0 s | 37.9 | 700 | 20.8 |
| 100 | 2.0 s | 10.1 | 218 | 46.4 |
| 250 | 0.8 s | 2.6 | 129 | 110.1 |

**Coverage beats targeting, which was a surprise.** Spending the budget on the pellets deepest in a well (where divergence is fastest) sounds obviously right and is 2.4x to 3.5x *worse* at every budget (median 92.7 vs 37.9 at 40/packet). Two reasons: a pellet deep in a well is about to be swallowed, and a respawn already resyncs it; and targeting starves every other pellet into drift with no bound at all. Round-robin's guarantee, that everything is refreshed within a bounded sweep, is what actually bounds the error.

**The drift is chaos, not staleness.** Latency barely moves it (182 px at 0 ms, 196 px at 80 ms), so this is genuine divergence between two integrations, not the field being out of date.

**A gentler field is easier to replicate, which was a free lesson.** Replacing the hard mass cap with a log curve was a gameplay change, and it improved the netcode measurably: the median error fell from 69.5 px to 40.2 and p90 from 788 to 519, purely because a less extreme field is less chaotic and diverges more slowly. Physics tuning and replication difficulty are the same dial viewed from two ends.

**Relevance culling is right for rendering and wrong for simulation inputs.** Hiding distant holes from a client makes its local physics wrong, because gravity is long range: every pellet you hold is bent by every hole, including the ones off your screen. *At small crowd sizes*, see below.

**The technique degrades as the field grows, which is the honest limit.** Everything above is measured with a handful of holes. Turn the crowd up (the slider goes to 64) and the premise erodes on three axes at once:

| holes | KiB/s | hole share of traffic | force evals/s per machine | median err | p90 |
|---|---|---|---|---|---|
| 4 | 21.1 | 5% | 0.5 M | 4.7 px | 44 |
| 8 | 45.1 | 10% | 1.0 M | 6.1 px | 146 |
| 16 | 108.0 | 16% | 1.9 M | 32.0 px | 390 |
| 32 | 280.8 | 25% | 3.8 M | 126.1 px | 1169 |
| 64 | 814.0 | 34% | 7.6 M | 166.8 px | 1538 |

Bandwidth grows *quadratically*, because every hole is sent to every player, so the field is 5% of traffic at four holes and 34% at sixty-four: "the field is tiny" stops being true. Compute grows linearly per machine, since every pellet integrates against every hole, and that cost is paid by every client rather than once by a server. And accuracy collapses, because more attractors make a more chaotic field, which diverges faster.

**Which flips the culling verdict at scale.** At 64 holes, culling the field costs accuracy (median 167 → 394 px) and now *saves something real* (814 → 567 KiB/s). At four holes it was a pure mistake; at sixty-four it is a trade you might take. The rule worth carrying is not "never cull the field" but **"culling the inputs to a simulation buys bandwidth by paying in correctness, and only at scale is there enough bandwidth at stake to be worth the payment."**

**And makes room for a third option that is neither.** Culling and sending everything are the two ends of a false choice: both answer "which distant holes does this client get?" when the useful question is "how precisely does it need them?" A distant crowd pulls almost exactly as one body of their combined weight at their centre of mass, and the further away they are the better that gets, so the far field can be *coarsened* rather than deleted. That is `plaza_server_utils::aggregate::AggregateTree`, a Barnes-Hut quadtree walked once per viewer, and the angle slider is its opening criterion. At 64 holes:

| config | KiB/s | of which field | attractors | force evals/s | median err | p90 |
|---|---|---|---|---|---|---|
| full field | 814.0 | 280.0 | 63.0 | 7.6 M | 166.8 px | 1538 |
| culled by view | 566.6 | 32.6 | 6.7 | 0.8 M | 394.0 px | 1753 |
| aggregated, theta 0.3 | 740.5 | 206.5 | 42.7 | 5.1 M | 182.1 px | 1767 |
| aggregated, theta 0.5 | 675.5 | 141.5 | 30.4 | 3.7 M | 237.5 px | 2697 |
| aggregated, theta 0.8 | 622.1 | 88.0 | 18.2 | 2.2 M | 319.5 px | 3657 |
| aggregated, theta 1.2 | 589.2 | 55.2 | 12.9 | 1.5 M | 512.1 px | 4007 |

Three things in that table are worth more than the technique itself.

**It is a compute technique here, not a bandwidth technique.** At `theta = 0.3` it removes a third of the per-machine force evaluations for a 9% accuracy cost, which is the best trade on the table. But total bandwidth barely moves, because the field is only a third of the traffic at 64 holes and pellet corrections are the rest: coarsening the field cannot reach bytes the field does not occupy. That is the same lesson the byte breakdown taught in `horde_playground` and it did not transfer on its own, it had to be measured again. **Check which resource a technique actually spends before believing it addresses the one you care about.**

**It has a safe range, not a monotone dial.** Past roughly `theta = 1.0` it becomes *worse than culling* (512 px against 394 at slightly more bandwidth). The criterion `s / d < theta` starts accepting cells the viewer is sitting close to, so a whole quadrant's mass lands on a single point near the pellets being integrated, and a spurious concentration damages a simulation more than a missing force does. Keeping every gram of the field is necessary but not sufficient: **where** you put it matters as much as how much of it you keep.

**Building the tree over a fitted bounding box was a real bug and a quiet one.** The first version derived the root cell from the current extent of the holes, so one hole drifting outward re-centred the entire subdivision, cluster membership changed for reasons having nothing to do with the holes in it, and the client's field twitched every packet. Pinning the root cell to the arena fixed a 15% median error regression that no test would have caught, because everything still ran and every total still added up. The primitive now offers `build_in` for exactly this and its docs say to prefer it.

## The straggler that carried a fixed bug for weeks

Worth recording here rather than only in [LEARNINGS.md](../LEARNINGS.md), because this example was the straggler. `client_utils::net_sim::LatencyLink` gained ordered delivery, since WebSocket is TCP and cannot reorder, and an impairment link that can produce a failure the real transport cannot manufactures red herrings: a full diagnostic cycle in the horde example went into a reordering hypothesis that was a property of the tooling. Horde was migrated onto the fixed link. This example kept a private copy, and that copy was still the unclamped version.

It was not harmless. At the shipped defaults, 15 ms of jitter against a roughly 16 ms send interval, it could hand its own client an older frame after a newer one, and the pellet stream has no tolerance for that at all because `swallowed` and `spawned` are order-sensitive. **A fix applied to one of two copies is a fix half the codebase does not have**, and the rule is not "extract early" but "extract, then go and find the other copy". The reason the copy existed is its own lesson: `LatencyLink` was not `Clone`, and a plaza state must be `Clone`, so **derives are part of the API contract** and a primitive that cannot sit inside application state will be reimplemented.

## How it is built

Depends on `plaza_client_utils` (for the deterministic `net_sim` link and the prediction bundle) and `plaza_server_utils` (for the aggregation tree, seats and rate meters). No relevance grid, which is itself part of the lesson: you cannot cull the inputs to a simulation the way you cull what you draw, and aggregation is what you reach for instead.

- **The shared step** ([src/sim/types.rs](src/sim/types.rs)): `step_pellet` is the one function both sides run. Semi-implicit Euler at a fixed timestep, with a small softening term so the well stays steep near the core, which is what makes the pull accelerate inward instead of feeling uniform.
- **Server** ([src/sim/server.rs](src/sim/server.rs)): integrates the field, and is authoritative for the two things that are actually *decisions*: what got swallowed, and what happened when two players touched. Pellet motion is not a decision, it is a consequence, so it is not replicated under field sync.
- **Client** ([src/sim/client.rs](src/sim/client.rs)): integrates every pellet locally from the field it was told, in the **same fixed step** as the server. Same rule is not enough; a different timestep is a different simulation. Its field is a flat list of `Attractor`, deliberately: the integrator must not be able to tell a real hole from a stand-in for fifty distant ones, or aggregation would be a second physics path and the two sides would stop running the same rule.

The simulation is headless and is where the tests live (`cargo test -p blackhole_playground`). `cargo run --release -p blackhole_playground --example report` prints the tables above.

**The networked layer wraps that headless sim without changing it** ([src/net/](src/net/)). The server side is `plaza` core (`StateController`, `StateLogic`, `TickDriver`) over `plaza_session` (`ActixWsPlazaSession`); the arena buffers each seat's input and drains it on the tick, exactly the shape the offline `advance_seats` already had. The client side is `plaza_client_utils` (`PredictedPlayer` for your own hole, `CorrectionMonitor` to say whether a correction was abnormal, `ClockSyncEstimator`, `RttEstimator`) over a `plaza_ws::Socket`. The hole is the reason `PredictedPlayer` carries a prediction **context**: it is a *forced* entity, so the client's copy of the rule needs the gravitational field to run, and before the context existed this example was smuggling the whole field through every buffered input. It is also why `set_active` exists, because an eliminated hole is frozen by the server through a respawn delay and a client that keeps integrating into it invents a correction stream entirely of its own making. Cargo features name what you want to build rather than the crates behind them: `client`, `server` (not available on `web`), `native`, `web`, `websocket`. The host keeps every control and readout because it is the server and a client in one process, publishing a `HostView` of the truth each send round for its own omniscient half.

## Notes

- Excluded from `default-members`, so a bare `cargo build` / `test` skips macroquad's dependency tree. Building for wasm needs `--no-default-features --features web`, because the default set includes the native socket and the actix server, neither of which targets the browser; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build artifact and is gitignored. Run `wasm-build.sh` (or the `cargo build --target wasm32-unknown-unknown --features web` it wraps) to produce it before serving a fresh checkout.
- A physics engine (Rapier and friends) would be the wrong tool here: pellets are non-colliding point masses in a force field, so rigid bodies, contacts, and joints go unused, and the gravity loop is still yours to write. The interesting consequence is the other way round, a heavy simulation makes client-side re-integration expensive and cross-platform determinism fragile, which pushes a game away from this technique and toward streaming state with interpolation.

A frame counter sits bottom right: with these entity counts, telling a client-side stall apart from a network effect matters.
