# puck_rink

Two-on-two air hockey, built for a case no server-authoritative example had: a body two teams are pushing at once. Every other example predicts what one player owns; a puck's next position depends on inputs you do not have, which is the case rollback is for.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8097/ws
./wasm-serve.sh                          # build the browser client, host it on :8097
cargo run -p puck_rink --bin scripted    # the headless re-simulation audit

PUCK_RINK_FEATURES=rapier ./run-native.sh --physics rapier     # the same rink on a solver
cargo run -p puck_rink --features rapier --bin scripted -- --physics both
```

Every seat always has an actor: bots skate until humans take their paddles, one leaver at a time. WASD or arrows; push the puck through the far mouth. Paddles are solid to each other, each team is fenced to its own half and only the puck crosses the line. A touch reflects the puck (the tangential component survives, topped up to shot speed along the normal), so a puck pinched between two paddles works its way out instead of bouncing between them forever. Bots are tuned to play like humans: they re-decide on a 200ms reaction (twelve ticks, staggered by seat) rather than at 60Hz, coast for the tail of every hold so their sustained speed sits below a held key and only the nearer of a pair chases while the partner minds the net. The cadence is also what keeps every client's repeat-last predictor mostly right about them; a bot chattering between held directions defeats it and the correction smear that causes is visible.

## The topology: rollback under a server

`rollback_playground` is peer-to-peer: two peers exchange inputs and stay identical. Here the topology is the one a deployed game actually has and the server holds two jobs at once:

- **Input orderer.** Every `Frame` echoes the inputs it applied (`world(f) = step(world(f-1), applied(f))`). That echo is what a client's [`RollbackSession`](../../client_utils/API_REFERENCE.md) confirms against: one ordered input stream instead of n² peer exchanges.
- **Authority.** The world in the frame is authoritative; a client that diverged would be corrected by it. The digest confirms that correction never has to happen.

The client runs the same fixed-point `sim::step` the server runs, predicts every unconfirmed input (repeat-last, which a held direction makes mostly right) and rolls back when the echo disproves a guess. `plaza_client_utils::rollback` is consumed as shipped: `StateHistory`, `InputTimeline` and the session loop, first consumer outside its own playground.

## Two ways to draw the puck

The panel draws the puck one of two ways and measures both the same way, recording what was shown each frame and comparing when the authoritative world for that frame arrives:

- **Interpolate**: delayed server frames blended, the standard treatment for anything owned by someone else. Smooth and late by the render delay plus the one-way, which on a contested puck is exactly the window where your paddle visibly passes through it.
- **Rollback**: the re-simulated present. On the beam, corrections arrive as re-simulations instead of position lerps and the panel prices them: corrections count, mean snap size and re-simulated frames, the cost the IDEAS entry asked to see beside the error.

The toggle covers the puck alone. Paddles are always drawn from the predicted present with corrections **eased**: whatever a rollback rewrites stays on screen as an offset that bleeds off over ~100ms, so a wrong guess shows as a small nudge rather than a jump. The whole screen therefore holds one timeline and a bounce lands where the paddles are drawn; drawing the paddles from delayed frames was tried and made the puck bounce off paddles that had not been drawn there yet.

## The digest and fixed point

The simulation is `Fx` throughout (`plaza_client_utils::fixed`); the renderer is the only float consumer. Every frame carries an FNV digest of the world and the client checks its own re-simulation of every confirmed frame against it: the count sits on the panel and **must stay zero on the diverged side**. This is the digest check seed_defense uses, applied to rollback. A contended body diverges quietly: two clients that disagree about one collision by one representational unit both look plausible for a whole rally and only a digest catches it. The scripted run is the same audit headless: re-simulate every broadcast frame from the echoed inputs and assert zero divergence.

Rapier was considered for the puck and declined as the default for this reason: five circles and four walls need no solver and the determinism claim would move from ~300 lines of owned `Fx` into a third-party crate where it holds same-version-only. It is built behind a feature anyway, to test in code whether a real physics engine can run under this setup.

## The other backend

`--features rapier` adds a second simulation alongside the first. Both compile, the server picks at startup and the same scripted trace runs through each so the numbers sit beside each other rather than in two terminals a week apart:

```
   backend    frames  diverged      join bytes
        fx       364         0               0
rapier:e722ad3b  364         0            4216
```

Both hold zero divergence. The fixed-point backend needs no join bytes at all. A `World` is the whole state, so every frame is already a complete baseline and a joiner has the complete state one tick after arriving; that is why the rink shipped with no snapshot provider. A solver's state is that plus contact manifolds, islands and sleep flags, none of which survive the trip through a `World`, so a client seeded from a frame diverges on its first contact and has to be handed a serialized pipeline instead. That handover is plaza's ordinary `SnapshotProvider`. The fixed-point backend declines it by returning `None`, which is why `create_snapshot` returns an `Option`. `a_view_cannot_seed_a_running_world` tests the divergence.

The seam is `Simulate` in `src/physics/`. A backend owns integration and contact; the rink's rules stay in shared code. Half-fencing, the goal mouth, the shot-speed top-up, the paddle carry, the speed cap, the drag and every bot are shared and both backends read the same constants out of `sim.rs`, so the two differ in physics rather than in tuning. Two things stay outside the solver: kinematic bodies do not depenetrate against each other, so paddle-on-paddle solidity stays an explicit pass and the drag stays a per-tick multiply because `linear_damping` would put an `exp` on the determinism path for a rule that is already exact.

### Wire precision and simulation precision

Glenn Fiedler's [state synchronization](https://gafferongames.com/post/state_synchronization/) article names its critical trick as quantizing the whole simulation state on **both** sides each frame, as if it had been through the network, so client and server extrapolate from identical values instead of one holding digits the other never saw.

`Fx` goes further and was arrived at independently. Rather than quantizing for the wire, it makes the quantized value the only precision there is: the wire carries the exact `i32`s the step runs on, so there is no gap between transmitted and simulated values.

The rapier backend reintroduces that gap. `view()` rounds f32 to `Fx` for the wire while the authority stays f32. The rollback path does not care, because a client re-simulates from its own f32 state and the digest is taken over f32 bits, never over the view. The **Interpolate** mode does: it blends quantised samples of an unquantised world, which is the desync Fiedler describes. Here it is under 1/256 of a unit against a puck moving up to six units per tick, so the render delay swamps it and the error meter will not show it.

His fix does not transfer. Snapping rapier's bodies onto the `Fx` grid after each step would inject error back into the solver's contacts every frame and it would not clean the digest anyway, since velocities and the pipeline's internal state stay f32 regardless. So this backend keeps the gap.

There are other costs beyond the join. The browser client more than doubles, 2.91MB to 6.22MB after `wasm-opt -Oz`. That is the one artifact that must never be served stale. A listen server cannot avoid that cost: the client re-simulates, so the solver ships to the browser or the rollback has nothing to run. Rapier's own `enhanced-determinism` is a second feature, `rapier-determinism`, which is off by default because of the measurement below. The rapier version also affects correctness: determinism holds same-version-only, while `PROTOCOL` hashes type definitions and a dependency bump moves none of them. So the exact build is sent on the wire in `Physics::Rapier { pin }` and a peer compiled against another rapier is refused instead of left to diverge quietly. This has happened in practice: [rapier#910](https://github.com/dimforge/rapier/issues/910) was a restored snapshot taking a different broad-phase code path, found by somebody doing rollback, fixed in parry 0.26.1 and shipped in rapier 0.33. `a_serialised_snapshot_resimulates_to_the_same_world` holds it fixed here.

### Does the browser agree with the server?

A rollback client re-simulates rather than watching, so the rink depends on this. `Fx` exists to make the question unnecessary: `plaza_client_utils::fixed` argues f32 cannot be relied on to match between wasm and native because of FMA contraction, wider intermediates and reassociation. `enhanced-determinism` is rapier's answer to the same worry.

It is checkable without a browser. Compile the same `Body::step` loop to `wasm32-unknown-unknown`, export the digest, run it under node and diff against the native build:

```
ticks   fx                rapier
1       bfdecfc23236253f  e1ad74ec6ab1fcd8
10      80ced7dc701e1d29  c47085a9a0f0afae
60      8081d1c9e2d0da18  544122a4a8581b4f
300     2b5847f1e7cb2dc2  d768af3d9e168a9b
1800    976f93f93b6c7d83  2097019f97b8cc93
```

Byte-identical on both backends, over 1800 ticks of paddles meeting each other, the puck and the boards. The fixed-point column is the control: integer arithmetic cannot disagree, so a difference there means the harness is wrong rather than the physics.

This result has two limits. It compares this machine's native build against wasm32, not one architecture against another. It also runs under one engine, though there is a reasonable argument that it generalises: every float operation here is either an exactly-specified wasm instruction or code compiled into the module, so there is little left for an engine to disagree about.

### Why `rapier-determinism` is off

The same harness measures the feature and this rink does not need it. Native and wasm agree with it off, agree with it on and the digests are identical between the two builds, so the feature is not what produces the agreement above.

`enhanced-determinism` does two things: it forces transcendental math through libm and it swaps joint wake-up and island-join iteration from a hash-set `drain()` to an indexed `drain(..)` so the order is stable. This backend has no joints at all, so those iterators are always empty and its arithmetic is `+ - * /`, `sqrt`, `clamp` and `abs`, with no transcendental anywhere and `sqrt` exactly rounded by IEEE-754 and by wasm. Neither half has anything to act on.

Adding a joint, a motor or anything that reaches for `sin` would change that immediately, which is why the feature stays available. It is also why the feature is folded into `PIN`: it changes what the solver computes, so a build with it and a build without it are two different simulations with the same version number and `PROTOCOL` sees neither cargo features nor dependency bumps.

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`, which takes `PUCK_RINK_FEATURES=rapier` when the rink is running the solver, since the client re-simulates and must carry the same physics the server does); MessagePack with a build-derived protocol version; the session's pong clock is the simulation clock, so input frames are aimed at sim time. Inputs are tick-addressed through `plaza_server_utils::InputSchedule` (level semantics: a held direction repeats until replaced), which is the same input model the client's prediction assumes.
