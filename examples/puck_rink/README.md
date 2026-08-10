# puck_rink

Two-on-two air hockey, built for the one thing no server-authoritative example had: a **body two teams are pushing at once**. Every other example predicts what one player owns; a puck's next position depends on inputs you do not have, which is the case rollback exists for.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8097/ws
./wasm-serve.sh                          # build the browser client, host it on :8097
cargo run -p puck_rink --bin scripted    # the headless re-simulation audit

PUCK_RINK_FEATURES=rapier ./run-native.sh --physics rapier     # the same rink on a solver
cargo run -p puck_rink --features rapier --bin scripted -- --physics both
```

Every seat always has an actor: bots skate until humans take their paddles, one leaver at a time. WASD or arrows; push the puck through the far mouth. Paddles are solid to each other, each team is fenced to its own half, and only the puck crosses the line. A touch reflects the puck (the tangential component survives, topped up to shot speed along the normal), which is what keeps a puck pinched between two paddles walking out instead of shuttling forever. Bots are deliberately human-grade: they re-decide on a 200ms reaction (twelve ticks, staggered by seat) rather than at 60Hz, coast for the tail of every hold so their sustained speed sits below a held key, and only the nearer of a pair chases while the partner minds the net. The cadence is also what keeps every client's repeat-last predictor mostly right about them; a bot chattering between held directions defeats it, and the correction smear that causes is visible.

## The topology: rollback under a server

`rollback_playground` is peer-to-peer: two peers exchange inputs and stay identical. Here the topology is the one a deployed game actually has, and the server holds two jobs at once:

- **Input orderer.** Every `Frame` echoes the inputs it applied (`world(f) = step(world(f-1), applied(f))`). That echo is what a client's [`RollbackSession`](../../client_utils/API_REFERENCE.md) confirms against: one ordered truth instead of n² peer exchanges.
- **Authority.** The world in the frame is not advisory; a client that diverged would be corrected by it. The digest is how we know that correction never has to happen.

The client runs the same fixed-point `sim::step` the server runs, predicts every unconfirmed input (repeat-last, which a held direction makes mostly right), and rolls back when the echo disproves a guess. `plaza_client_utils::rollback` is consumed as shipped: `StateHistory`, `InputTimeline`, and the session loop, first consumer outside its own playground.

## The number: one puck, two treatments

The panel draws the puck one of two ways and measures both the same way, recording what was shown each frame and comparing when the authoritative world for that frame arrives:

- **Interpolate**: delayed server frames blended, the standard treatment for anything owned by someone else. Smooth, and late by the render delay plus the one-way, which on a contested puck is exactly the window where your paddle visibly passes through it.
- **Rollback**: the re-simulated present. On the beam, corrections arrive as re-simulations instead of position lerps, and the panel prices them: corrections count, mean snap size, and re-simulated frames, the cost the IDEAS entry asked to see beside the error.

The toggle covers the puck alone. Paddles are always drawn from the predicted present with corrections **eased**: whatever a rollback rewrites stays on screen as an offset that bleeds off over ~100ms, so a disproof reads as a nudge rather than a teleport. The whole screen therefore holds one timeline, and a bounce lands where the paddles are drawn; splitting the paddles onto delayed frames was tried and made the puck carom off empty ice their drawn past had not reached yet.

## The digest, or why fixed point is not a style choice

The simulation is `Fx` throughout (`plaza_client_utils::fixed`); the renderer is the only float consumer. Every frame carries an FNV digest of the world, and the client checks its own re-simulation of every confirmed frame against it: the count sits on the panel and **must stay zero on the diverged side**. This is seed_defense's discipline reaching rollback: a contended body diverges quietly, two clients disagreeing about one collision by one representational unit both stay plausible for a whole rally, and only a digest says so. The scripted run is the same audit headless: re-simulate every broadcast frame from the echoed inputs and assert zero divergence.

Rapier was considered for the puck and declined **as the default** for exactly this file: five circles and four walls need no solver, and the determinism claim would move from ~300 lines of owned `Fx` into a third-party crate where it holds same-version-only. It is built anyway, behind a feature, because whether a real physics engine can sit under this deserves an answer in code rather than a paragraph.

## The other backend

`--features rapier` does not swap the simulation, it adds one. Both compile, the server picks at startup, and the same scripted trace runs through each so the numbers sit beside each other rather than in two terminals a week apart:

```
   backend    frames  diverged      join bytes
        fx       364         0               0
rapier:e722ad3b  364         0            4216
```

Both hold zero, which is the claim that mattered. The 4216 bytes are the column the fixed-point backend does not have at all. A `World` **is** the whole state, so every frame is already a complete baseline and a joiner is whole one tick after arriving; that is why the rink shipped with no snapshot provider. A solver's state is that plus contact manifolds, islands and sleep flags, none of which survive the trip through a `World`, so a client seeded from a frame diverges on its first contact and has to be handed a serialized pipeline instead. That handover is plaza's ordinary `SnapshotProvider`, and the fixed-point backend declines it by returning `None`, which is the whole reason `create_snapshot` returns an `Option`. `a_view_cannot_seed_a_running_world` asserts the divergence rather than trusting the argument.

The seam is `Simulate` in `src/physics/`. A backend owns integration and contact; the rink's *rules* do not move. Half-fencing, the goal mouth, the shot-speed top-up, the paddle carry, the speed cap, the drag and every bot are shared, and both backends read the same constants out of `sim.rs`, so the two differ in physics rather than in tuning. Two things a solver does not take over, both worth knowing before reaching for one: kinematic bodies do not depenetrate against each other, so paddle-on-paddle solidity stays an explicit pass, and the drag stays a per-tick multiply because `linear_damping` would put an `exp` on the determinism path for a rule that is already exact.

### One precision, or two

Glenn Fiedler's [state synchronization](https://gafferongames.com/post/state_synchronization/) article names its critical trick as quantizing the whole simulation state on **both** sides each frame, as if it had been through the network, so client and server extrapolate from identical values instead of one holding digits the other never saw.

`Fx` is the strongest form of that, arrived at independently and for the harder reason. It is not quantization applied to the wire, it is quantization made the only precision there is: the wire carries the exact `i32`s the step runs on, so there is no transmitted-versus-simulated gap left to close.

**The rapier backend reintroduces exactly that gap**, and it belongs on the record rather than in a commit message. `view()` rounds f32 to `Fx` for the wire while the authority stays f32. The rollback path does not care, because a client re-simulates from its own f32 state and the digest is taken over f32 bits, never over the view. The **Interpolate** mode does: it blends quantised samples of an unquantised truth, which is Fiedler's desync precisely. Here it is under 1/256 of a unit against a puck moving up to six units per tick, so the render delay swamps it and the error meter will not show it. It is still a property the fixed-point backend does not have.

His fix does not transfer. Snapping rapier's bodies onto the `Fx` grid after each step would inject error back into the solver's contacts every frame, and it would not clean the digest anyway, since velocities and the pipeline's internal state stay f32 regardless. The gap is a real cost of the backend, not a bug to be patched out.

What it costs past the join. The browser client more than doubles, 2.91MB to 6.22MB after `wasm-opt -Oz`, which is not nothing for the one artifact that must never be served stale. That is not a cost a listen server can decline, either: the client re-simulates, so the solver ships to the browser or the rollback has nothing to run. Rapier's own `enhanced-determinism` is a second feature, `rapier-determinism`, and it is **off by default** on a measurement rather than a hunch (below). And the version is a correctness input rather than a preference: determinism holds same-version-only, while `PROTOCOL` hashes type definitions and a dependency bump moves none of them. So the exact build rides on the wire in `Physics::Rapier { pin }`, and a peer compiled against another rapier is refused instead of left to diverge quietly. The hazard is not hypothetical: [rapier#910](https://github.com/dimforge/rapier/issues/910) was a restored snapshot taking a different broad-phase code path, found by somebody doing rollback, fixed in parry 0.26.1 and shipped in rapier 0.33. `a_serialised_snapshot_resimulates_to_the_same_world` holds it fixed here.

### Does the browser actually agree with the server?

That is the whole claim, since a rollback client re-simulates rather than watching, and it is the question `Fx` exists to make unnecessary: `plaza_client_utils::fixed` argues f32 cannot be relied on to match between wasm and native because of FMA contraction, wider intermediates and reassociation. `enhanced-determinism` is rapier's answer to the same worry.

It is checkable without a browser. Compile the same `Body::step` loop to `wasm32-unknown-unknown`, export the digest, run it under node, and diff against the native build:

```
ticks   fx                rapier
1       bfdecfc23236253f  e1ad74ec6ab1fcd8
10      80ced7dc701e1d29  c47085a9a0f0afae
60      8081d1c9e2d0da18  544122a4a8581b4f
300     2b5847f1e7cb2dc2  d768af3d9e168a9b
1800    976f93f93b6c7d83  2097019f97b8cc93
```

Byte-identical on both backends, over 1800 ticks of paddles meeting each other, the puck and the boards. The fixed-point column is the control: integer arithmetic cannot disagree, so a difference there means the harness is wrong rather than the physics.

Two honest bounds on that. It compares this machine's native build against wasm32, not one architecture against another. And it runs under one engine, though the argument that it generalises is decent: every float operation here is either an exactly-specified wasm instruction or code compiled into the module, so there is little left for an engine to disagree about.

### Which is why `rapier-determinism` is off

The same harness prices the feature, and the answer is that this rink does not use it. Native and wasm agree with it off, agree with it on, and **the digests are identical between the two builds**, so it is not buying the agreement above.

That is explainable rather than lucky. `enhanced-determinism` does two things: it forces transcendental math through libm, and it swaps joint wake-up and island-join iteration from a hash-set `drain()` to an indexed `drain(..)` so the order is stable. This backend has no joints at all, so those iterators are always empty, and its arithmetic is `+ - * /`, `sqrt`, `clamp` and `abs`, with no transcendental anywhere and `sqrt` exactly rounded by IEEE-754 and by wasm. Neither half has anything to act on.

Add a joint, a motor, or anything reaching for `sin`, and that stops being true immediately, which is the whole reason it stays available. It is also why the feature is folded into `PIN`: it changes what the solver computes, so a build with it and a build without it are two different simulations wearing one version number, and `PROTOCOL` can no more see a cargo feature than it can see a dependency bump.

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client, and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`, which takes `PUCK_RINK_FEATURES=rapier` when the rink is running the solver, since the client re-simulates and must carry the same physics the server does); MessagePack with a build-derived protocol version; the session's pong clock is the simulation clock, so input frames are aimed at sim time. Inputs are tick-addressed through `plaza_server_utils::InputSchedule` (level semantics: a held direction repeats until replaced), which is the same input model the client's prediction assumes.
