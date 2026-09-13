# blackhole_playground

You are a black hole. Pellets drift toward you, slowly at the rim and faster the deeper they fall and swallowing them makes you bigger. Running into a rival costs you mass.

Thousands of pellets move only because of a handful of black holes. So the server can either send the *field* (a few positions and masses) and let every client integrate the pellets itself or send thousands of pellet positions the conventional way. This example implements both and measures the difference.

This is deliberately a **hard** case for the technique. The [`horde_playground`](../horde_playground/) enemies home toward a target, so prediction errors shrink on their own. Gravity is divergent, so here they grow.

## Running it

**The host is the server.** A native run hosts by default and its own player is just another client on a real socket. The host can show omniscient readouts because it really does own both sides. One `--role` argument decides what a process is, because the combinations are not independent and separate flags would let you ask for contradictions.

```sh
./run-native.sh                                                  # --role host: play and serve joiners
./run-native.sh --role observer                               # watch and drive the settings, no hole of your own
./run-native.sh --role client --connect ws://<host>:8080/ws   # join someone else's arena
./wasm-build.sh                                                  # build the browser client only
./wasm-serve.sh                                                  # build it and host it; open the printed URL
```

| `--role` | server | window | your own hole |
|---|---|---|---|
| `headless` | yes | no | no | the windowless deployable and what `wasm-serve.sh` runs |
| `observer` | yes | yes | no | full control panel, watching; a free camera (drag / WASD / wheel, `C` recenters) |
| `host` | yes | yes | yes | plays and serves. **The default** |
| `client` | no | yes | yes | join only. The only role a browser can take |

A host prints a local URL and a LAN URL; open either in a browser to join, or send the LAN one to a friend. The browser client connects back to whoever served it (over `wss://` if the page was secure), so a `--role headless` deploy behind a TLS terminator works the same way. The impairment sliders (latency, jitter, loss) act on the **real** per-connection link and in **both** directions, so a host can show a joiner what 200 ms and 10% loss feel like.

**WASD / arrows** to move, **space** to dash; on a phone, touch and drag anywhere to steer.

The single-process build, with no networking compiled in, is where the measurements below come from: `cargo run -p blackhole_playground --no-default-features --features native,client`.

Black holes pull each other as well as the pellets, so contact is sticky: drift too close and the attraction closes the rest of the distance and holds it closed. Holes never pass through each other. Whatever overlap the pull would have created is measured as *pressure* and then undone, which leaves them exactly tangent. **Both** holes lose mass the whole time they are pressed together, faster the harder the press, so dashing into someone drains more than drifting into them. Draining shrinks their radii, so they stay in contact while getting smaller. Squeeze a rival to zero and they are **eliminated**; that is what merging means here. Eliminated players return after a few seconds. You cannot walk out of a grapple because the pull at contact is tuned above walking speed. A dash outruns it briefly, then the pull starts closing the gap again at once. Breaking away usually takes a few dashes.

Score and size are separate. **Score** is pellets eaten and only goes up. **Mass** is the physical stat: it sets your size *and the strength of your gravity* and it drains continuously while you are in contact with a rival. Mass has no ceiling; instead its effect is log-damped (`scale * ln(1 + mass/scale)`), so growth is near-linear while you are small and flattens as you get large.

## What you are looking at

Bright pellets are where **your client** thinks they are; the faint ones underneath are the **server ghost** and the gap between them is divergence. A hollow ring shows where the server has *your* hole. Your hole is the only entity drawn somewhere other than where the server puts it. The ring opens during a grapple and closes when you break away, because collision separation between holes is deliberately not predicted and the ring shows that residual. The ghost is on by default and has a switch in every role. A host's ring is the server's current state, so its gap is prediction error alone. A joiner's ring is the newest sample it received, so it needs no omniscient access and its gap is that error plus the sample's age. The faint *pellets* are the one part a joiner cannot draw, because under field sync pellet positions are never sent; that is where field sync saves its bandwidth.

This ghost means something different from [horde's](../horde_playground/), because the two clients are built differently. Horde buffers packets and plays them out on a render clock, so its ghost is the *future* it already holds and the gap is the playout delay. This client applies a packet on arrival and predicts forward from it, so its ghost sits *behind* the marker and the gap is prediction error. Each hole is drawn as a wide disk (where the pull begins and the body rivals collide with) around a dark core (where a pellet is actually swallowed), so you can watch pellets accelerate through the well instead of vanishing at the rim.

| Control | What it shows |
|---|---|
| **the field / the particles** | the main comparison: a few hole states or every visible pellet |
| **corrections per packet** | under field sync, how much of the budget goes to refreshing pellets |
| **correct the deepest first** | a targeting policy that measurement says is much worse than plain rotation |
| **cull the field by view distance** | a deliberate mistake: gravity is long range, so hiding a distant hole makes local physics wrong |
| **aggregate the far field (angle)** | a third option: distant holes are replaced by one stand-in at their centre of mass, so their mass is kept and only their positions are approximated. Turn the crowd up to 64 first, then compare it against culling |
| **latency / jitter / loss** | real impairment on real connections, in both directions. Delivery stays ordered, because the transport underneath is TCP and an impairment link that reorders would produce failures the real one cannot |
| **predict the dash burst** | on by default; turn it off to feel the cost of leaving an ability unpredicted. Two shadow predictors run the same gameplay differing only in this flag, so the readout shows whether predicting it helps without comparing runs from memory |
| **server ghost** | the authoritative pellets and your hole's real position, drawn faintly underneath. On by default. The hole is a *forced* entity, so its prediction is the hardest part of this example and three separate bugs in it once produced the same symptom |

## What it measured

3000-unit arena, 2000 pellets, four players, 10 Hz, 80 ms latency.

Sending the field is far cheaper and the typical pellet is no less accurate:

| mode | KiB/s | states/packet | median err | p90 | mean |
|---|---|---|---|---|---|
| field (a few holes) | **20.2** | 40 | **40.2 px** | 519 | 199 |
| particles (visible set) | 146.0 | 336 | 74.3 px | **120** | **82** |

Field sync uses about a seventh of the bandwidth and its median error is a little over half. The cost is in the **tail**: p90 of 519 px against 120. A minority of pellets, the ones falling through a core where acceleration is extreme, diverge chaotically. Use percentiles to judge this; the mean is dominated by a handful of outliers.

Corrections limit the drift and the budget sets the limit:

| corrections/packet | every pellet refreshed | median | p90 | KiB/s |
|---|---|---|---|---|
| 0 | never | 49.0 | 157 | 3.8 |
| 40 | 5.0 s | 37.9 | 700 | 20.8 |
| 100 | 2.0 s | 10.1 | 218 | 46.4 |
| 250 | 0.8 s | 2.6 | 129 | 110.1 |

Refreshing every pellet in rotation works better than targeting the worst ones. Spending the budget on the pellets deepest in a well (where divergence is fastest) sounds right but is 2.4x to 3.5x *worse* at every budget (median 92.7 vs 37.9 at 40/packet). There are two reasons. A pellet deep in a well is about to be swallowed and its respawn resyncs it anyway. And targeting leaves every other pellet to drift without limit. Round-robin refreshes every pellet within a bounded sweep, which is what bounds the error.

Latency barely changes the drift (182 px at 0 ms, 196 px at 80 ms), so it comes from two integrations diverging chaotically and not from the field being out of date.

Replacing the hard mass cap with a log curve was a gameplay change and it also improved the netcode: the median error fell from 69.5 px to 40.2 and p90 from 788 to 519, because a less extreme field is less chaotic and diverges more slowly.

Relevance culling is fine for rendering but breaks simulation inputs. Hiding distant holes from a client makes its local physics wrong, because gravity is long range: every pellet is pulled by every hole, including the ones off screen. That holds at small crowd sizes; see below for large ones.

Field sync gets worse as the number of holes grows. Everything above was measured with a handful of holes. With more holes (the slider goes to 64), bandwidth, compute and accuracy all get worse:

| holes | KiB/s | hole share of traffic | force evals/s per machine | median err | p90 |
|---|---|---|---|---|---|
| 4 | 21.1 | 5% | 0.5 M | 4.7 px | 44 |
| 8 | 45.1 | 10% | 1.0 M | 6.1 px | 146 |
| 16 | 108.0 | 16% | 1.9 M | 32.0 px | 390 |
| 32 | 280.8 | 25% | 3.8 M | 126.1 px | 1169 |
| 64 | 814.0 | 34% | 7.6 M | 166.8 px | 1538 |

Bandwidth grows *quadratically*, because every hole is sent to every player, so the field is 5% of traffic at four holes and 34% at sixty-four and is no longer small. Compute grows linearly per machine, since every pellet integrates against every hole and every client pays that cost, not just the server. Accuracy drops sharply, because more attractors make a more chaotic field, which diverges faster.

At scale the culling result changes. At 64 holes, culling the field still costs accuracy (median 167 → 394 px) but now saves a real amount of bandwidth (814 → 567 KiB/s). At four holes culling was simply a mistake; at sixty-four it is a trade you might take. Culling a simulation's inputs trades correctness for bandwidth and only at scale is enough bandwidth at stake to make that worthwhile.

A third option is to approximate the far field. Culling and sending everything both decide which distant holes a client gets. Aggregation decides how precisely it gets them. A distant crowd pulls almost exactly like one body of their combined weight at their centre of mass. The approximation improves with distance, so the far field can be *coarsened* instead of deleted. That is `plaza_server_utils::aggregate::AggregateTree`, a Barnes-Hut quadtree walked once per viewer; the angle slider is its opening criterion. At 64 holes:

| config | KiB/s | of which field | attractors | force evals/s | median err | p90 |
|---|---|---|---|---|---|---|
| full field | 814.0 | 280.0 | 63.0 | 7.6 M | 166.8 px | 1538 |
| culled by view | 566.6 | 32.6 | 6.7 | 0.8 M | 394.0 px | 1753 |
| aggregated, theta 0.3 | 740.5 | 206.5 | 42.7 | 5.1 M | 182.1 px | 1767 |
| aggregated, theta 0.5 | 675.5 | 141.5 | 30.4 | 3.7 M | 237.5 px | 2697 |
| aggregated, theta 0.8 | 622.1 | 88.0 | 18.2 | 2.2 M | 319.5 px | 3657 |
| aggregated, theta 1.2 | 589.2 | 55.2 | 12.9 | 1.5 M | 512.1 px | 4007 |

Three things stand out in that table.

At this crowd size it saves compute more than bandwidth. At `theta = 0.3` it removes a third of the per-machine force evaluations for a 9% accuracy cost, the best trade in the table. Total bandwidth barely moves, because the field is only a third of the traffic at 64 holes and pellet corrections are the rest. Coarsening the field only shrinks the field's share. The byte breakdown in `horde_playground` showed the same effect and it had to be measured again here.

Past roughly `theta = 1.0` it becomes *worse than culling* (512 px against 394 at slightly more bandwidth). The criterion `s / d < theta` starts accepting cells the viewer is close to, so a whole quadrant's mass lands on a single point near the pellets being integrated. A false concentration of mass damages the simulation more than a missing force does, so keeping all of the mass is not enough: it also has to be in roughly the right place.

Building the tree over a fitted bounding box was a bug that nothing flagged. The first version derived the root cell from the current extent of the holes, so one hole drifting outward re-centred the whole subdivision, clusters changed for reasons unrelated to the holes in them and the client's field jittered every packet. Pinning the root cell to the arena fixed a 15% median error regression that no test would have caught, because everything still ran and every total still added up. The primitive now offers `build_in` for this and its docs say to prefer it.

## The private copy of LatencyLink

`client_utils::net_sim::LatencyLink` gained ordered delivery, since WebSocket runs over TCP and cannot reorder. An impairment link that produces failures the real transport cannot sends debugging the wrong way: a full diagnostic cycle in the horde example chased a reordering bug that only existed in the tooling. Horde was moved onto the fixed link. This example kept a private copy, which was still the unclamped version.

At the shipped defaults, 15 ms of jitter against a roughly 16 ms send interval, that copy could deliver an older frame after a newer one to its own client. The pellet stream cannot handle that, because `swallowed` and `spawned` are order-sensitive. After extracting a shared version, find and replace every other copy. Otherwise the fix only reaches one of them. The copy existed because `LatencyLink` was not `Clone` and a plaza state must be `Clone`. A primitive's derives are part of its API, because one that cannot be stored in application state will get reimplemented.

## How it is built

Depends on `plaza_client_utils` (for the deterministic `net_sim` link and the prediction bundle) and `plaza_server_utils` (for the aggregation tree, seats and rate meters). There is no relevance grid, because you cannot cull the inputs to a simulation the way you cull what you draw. Aggregation is used instead.

- **The shared step** ([src/sim/types.rs](src/sim/types.rs)): `step_pellet` is the one function both sides run. Semi-implicit Euler at a fixed timestep, with a small softening term so the well stays steep near the core, which is what makes the pull accelerate inward instead of feeling uniform.
- **Server** ([src/sim/server.rs](src/sim/server.rs)): integrates the field and is authoritative for the two things that are actually *decisions*: what got swallowed and what happened when two players touched. Pellet motion follows from the field rather than being decided, so it is not replicated under field sync.
- **Client** ([src/sim/client.rs](src/sim/client.rs)): integrates every pellet locally from the field it was told, in the **same fixed step** as the server. Both sides must use the same timestep as well as the same rule to run the same simulation. Its field is a flat list of `Attractor`, deliberately: the integrator must not be able to tell a real hole from a stand-in for fifty distant ones. Otherwise aggregation would be a second physics path and the two sides would stop running the same rule.

The simulation is headless and is where the tests live (`cargo test -p blackhole_playground`). `cargo run --release -p blackhole_playground --example blackhole_report` prints the tables above.

The networked layer ([src/net/](src/net/)) wraps the headless sim without changing it. The server side is `plaza` core (`StateController`, `StateLogic`, `TickDriver`) over `plaza_session` (`ActixWsPlazaSession`); the arena buffers each seat's input and drains it on the tick, exactly the shape the offline `advance_seats` already had. The client side is `plaza_client_utils` (`PredictedPlayer` for your own hole, `CorrectionMonitor` to say whether a correction was abnormal, `ClockSyncEstimator`, `RttEstimator`) over a `plaza_ws::Socket`. The hole is the reason `PredictedPlayer` carries a prediction **context**: it is a *forced* entity, so the client's copy of the rule needs the gravitational field to run and before the context existed this example passed the whole field inside every buffered input. It is also why `set_active` exists, because an eliminated hole is frozen by the server through a respawn delay and a client that keeps integrating it produces a stream of corrections with no cause on the server. Cargo features name what you want to build rather than the crates behind them: `client`, `server` (not available on `web`), `native`, `web`, `websocket`. The host keeps every control and readout because it is the server and a client in one process, publishing a `HostView` of the truth each send round for its own omniscient half.

## Notes

- Excluded from `default-members`, so a bare `cargo build` / `test` skips macroquad's dependency tree. Building for wasm needs `--no-default-features --features web`, because the default set includes the native socket and the actix server, neither of which targets the browser; `wasm-build.sh` does this.
- The compiled `static/*.wasm` is a build artifact and is gitignored. Run `wasm-build.sh` (or the `cargo build --target wasm32-unknown-unknown --features web` it wraps) to produce it before serving a fresh checkout.
- A physics engine (Rapier and friends) would be the wrong tool here: pellets are non-colliding point masses in a force field, so rigid bodies, contacts and joints go unused and the gravity loop is still yours to write. It also works against this technique: a heavy simulation makes client-side re-integration expensive and cross-platform determinism fragile, which pushes a game toward streaming state with interpolation instead.

A frame counter sits bottom right, so at these entity counts you can tell a client-side stall from a network effect.
