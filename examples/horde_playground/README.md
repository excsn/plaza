# horde_playground

The many-entity case: thousands of enemies, four players standing in different parts of a world far larger than one screen, and a bandwidth budget. A "bullet heaven" (Vampire Survivors shaped) horde, networked.

Where [`netcode_playground`](../netcode_playground/) shows one player's mechanisms up close and [`rollback_playground`](../rollback_playground/) shows the peer-to-peer family, this one is about **scale**: what you send, to whom, how often, and how a client draws things it is barely told about.

You drive your player with **WASD / arrows**; the weapons aim and fire themselves, Vampire Survivors style. Every 4.5 seconds each player emits an **area pulse** that wipes every enemy within 190 units at once (an expanding ring marks it), and reinforcements keep arriving into the slots the dead freed. With four players pulsing together into a converged horde, a single tick can kill several hundred, which is exactly the mass-despawn burst measured below.

## Running it

```sh
./run-native.sh          # or: cargo run -p horde_playground --release
./serve.sh               # browser (wasm), then open http://localhost:8080
```

`serve.sh` handles the whole ceremony: installs the wasm target if missing, builds, copies the artifact next to the page, shrinks it with `wasm-opt` if present, and serves.

## What you are looking at

The **main view** follows your player and draws only what your client was actually sent: the enemies inside its relevance radius (solid), over the server's true positions (faint). The gap between solid and faint is the error your drawing strategy is costing you.

The **minimap** shows the whole arena with every enemy the server is simulating. The dots outside your circle are what relevance is saving you from receiving.

| Control | What it shows |
|---|---|
| **per-player relevance** | turn it off and watch bandwidth explode: every player is sent every entity |
| **server send rate** | drop it to 1 Hz, the rate a shipped horde co-op actually uses, and see which drawing mode survives |
| **how remotes are drawn** | simulate (run the AI rule locally), dead reckon (last velocity), or interpolate (render in the past) |
| **players spread / clustered** | clustered players make the horde converge on one spot, raising local density |
| **ease corrections** | smoothing, with the caveat measured below |
| **weapons, deaths, and waves** | combat off leaves a pure movement horde, for isolating the networking |
| **generational entity handles** | a handle names a slot *and* its occupant; off, a reference to a dead entity would land on whoever recycled its slot |

Press `R` to re-baseline the readouts after changing something. A frame counter sits bottom right: with these entity counts, telling a client-side stall apart from a network effect matters.

## What it measured

The example exists to settle questions by measurement rather than argument, and it corrected several assumptions. Numbers are 3000 enemies, 4 players, 420px view in a 3000px arena.

**Relevance pays, clustered or not.** 91% of the broadcast culled with players spread, **90% clustered** (28.0 vs 325.0 KiB/s spread; 31.5 vs 325.3 clustered). An earlier guess that relevance mostly matters when players separate was wrong: the arena dwarfs the view either way. Clustering actually makes it slightly *worse*, because the horde converges on grouped players and raises local density.

**Which drawing strategy wins depends entirely on the send rate** (mean / worst error, px):

| send rate | simulate | dead reckon | interpolate |
|---|---|---|---|
| 1 Hz | **12 / 86** | 30 / 67 | 57 / 65 |
| 2 Hz | **6 / 27** | 6 / 27 | 30 / 36 |
| 4 Hz | **5 / 14** | 4 / 23 | 16 / 17 |
| 10 Hz | 10 / 11 | **2 / 10** | 10 / 10 |
| 30 Hz | 13 / 14 | **1 / 7** | 7 / 7 |

Running the behaviour rule locally wins decisively at 1 to 4 Hz, which is what makes a very low send rate viable. It is *not* a general upgrade: from 10 Hz up, dead reckoning is better. Interpolation is the most *consistent* (its worst case barely moves) but always renders about one send interval in the past, which at 1 Hz is a second.

**Correction smoothing can become the error.** Simulate gets worse as the send rate rises (5 → 10 → 13 px) because a 250 ms ease never completes when corrections arrive every 33 ms, so the smoother itself dominates. **Keep the ease shorter than the send interval.**

**Compact ids and quantized positions save a consistent 71%** (3.4x) at every send rate: a 3-byte id (`u16` index + `u8` generation) and a position quantized to two `u16`s, against a 16-byte UUID and two `f32`s.

**The bytes are not where the interesting encoding problem is.** Broken down by part: samples (position corrections) **86.1%**, spawns 11.1%, despawns **1.2%**, everything else 1.6%. Despawn ids are encoded here as sorted varint deltas, which measured 55% cheaper than three-bytes-per-id (and beat both a presence bitmask and run-length encoding on the real burst data), but 55% of 1.2% is 0.7% of the packet. Worth knowing before optimising a stream: check its share first.

**Churn is modest, but it spikes.** Steady movement produces ~23 spawns and ~15 despawns per packet, cheap enough that compressing it would save little. The area pulse is the exception: one wipe despawned **278 entities in a single packet**. That burst, not the steady churn, is the case a range encoding would be for.

**Generational handles were not needed here, which was a surprise.** Across 413 kills with slots actively recycling under 80 ms of latency, the client recorded **zero** stale handle references. Two things make the generation redundant in this configuration: delivery is ordered, and the server announces a death *explicitly* (clearing the visibility bit) before the next diff, so a reused slot reads as despawn-then-spawn rather than silently becoming a different entity. Generational handles are insurance for **unordered** transport, where a stale packet can overtake the despawn that would have invalidated it, not a prerequisite for slot recycling. Filed as a keystone item before this measured it; the measurement says otherwise.

## Recovering a delta stream from packet loss

The relevance stream sends `entered` and `left` and lets each client keep a mirror. That is cheap and it has a failure built into it: the server diffs against **what it last sent**, which silently assumes every packet arrives. This example had no packet-loss slider, so the flaw had never been exercised. Adding one made it plain.

| loss | baseline | mismatches | phantoms | missing | held | resyncs | KiB/s | err px |
|---|---|---|---|---|---|---|---|---|
| 0% | either | 0 | 4 | 107 | 981 | 0 | 33.0 | 7.2 |
| 5% | last sent | 240 | 290 | 101 | 1279 | 0 | 33.0 | 7.2 |
| | last acked | 102 | 11 | 107 | 1024 | 11 | 37.9 | 9.0 |
| 10% | last sent | 227 | 364 | 174 | 1302 | 0 | 33.0 | 19.1 |
| | last acked | 70 | 4 | 107 | 981 | 18 | 40.1 | 6.7 |
| 25% | last sent | 234 | 188 | 313 | 1156 | 0 | 33.0 | 44.7 |
| | last acked | 126 | 7 | 263 | 831 | 24 | 39.7 | 9.2 |

The fix is to diff against **what the client acknowledged**, using `plaza_client_utils::ack::AckWindow` on the return path. Corpses fall from hundreds to single digits and render error from 44.7 px to 9.2 for about 19% more bandwidth. Four details are load-bearing, and three of them were wrong in the first working version.

**The baseline must be the newest *contiguous* acknowledgement, not the newest bit set.** A bitmask answers "what arrived", which is what a *retransmitting* protocol wants: it names the holes to refill. A protocol that *re-derives* needs a state the client provably reached, and receiving packet N+1 after losing N does not put the client in the state N+1 implies. Taking the newest set bit hands the diff a state that never existed, and it made recovery statistically indistinguishable from no recovery.

**The baselines must be keyed by index *and generation*.** This was the corpse leak itself. A retraction re-derived after the slot had been recycled named the slot's *current* occupant, so the client's lookup missed and the entity it actually held was never mentioned again. Keying the baseline the way the digest is keyed makes recovery and verification answer the same question.

**The two halves of the diff need baselines built by opposite operations.** What to *send* must assume the least the client could hold: the acknowledged state **intersected** with every state sent since, because anything a later packet may have retracted might already be gone. What to *retract* must assume the most: the acknowledged state **unioned** with everything announced since, because an entity that entered and left inside that gap appears in neither the baseline nor the current set and a single diff never mentions it. Getting the union right and leaving the other half as the raw acknowledged state simply trades one silent failure for its mirror image, and that is exactly what happened: the corpses became omissions.

**Phantoms alone cannot tell you it worked.** A client that has starved agrees with everything, so a mirror emptying out scores perfectly on corpses, on digest agreement, and on render error, which only averages over entities both sides have. The `missing` column exists for that reason and it immediately found the omission bug above. It never reaches zero, because entities that just became relevant are still in flight; 107 is that floor, and the number to watch is whether it stays there.

## Knowing about the world outside your view radius

Relevance culling answers "which distant entities does this client get?" with "none". That is correct for entities the client must draw at full fidelity and it leaves a hole: past the radius the client knows nothing, so the minimap in this example was drawing the *server's* copy of the arena, which no real client has. The caption said so, which made it honest, not correct.

Aggregation asks the useful question instead, which is how *precisely* the client needs them. A `plaza_server_utils::aggregate::AggregateTree` over the live population, weighted one per enemy so a summary's weight is literally a headcount, gives a stand-in per distant crowd: a position and a count, six bytes, for any number of enemies.

| crowd LOD | summaries | awareness of the distant world | crowd B/s | total KiB/s |
|---|---|---|---|---|
| off (cull only) | 0 | **0%** | 0 | 30.9 |
| theta 0.2 | 204 | 99% | 25048 | 55.4 |
| theta 0.4 | 74 | 99% | 8238 | 39.0 |
| theta 0.8 | 27 | 100% | 2515 | 33.4 |
| theta 1.5 | 12 | 98% | 997 | 31.9 |

At `theta = 1.5` a client is aware of 98% of a 3000-enemy world outside its own view for **twelve summaries and about 3% more bandwidth**. Culling gives 0% of that at any bandwidth, because the only thing it can send instead of nothing is everything.

Turn the slider on and the minimap stops borrowing: it draws the entities the client actually holds, plus a blob per crowd summary, and the caption changes to say so. Turn it back to zero and the arena is the server's again.

Note what is different about this use from [the black hole example's](../blackhole_playground/), where the same primitive summarises a gravity field. There the summaries feed a *simulation*, so the approximation shows up as accumulating physics error and the opening angle has a hard safe range past which it is worse than culling. Here they feed a *drawing*, nothing integrates them, and the failure mode is only that a crowd blob is in slightly the wrong place. The same block, and the tolerance for coarseness is set entirely by what consumes the output.

## Coins: predicting a discrete, contested event

Everything else in this example predicts *continuous* state, where a correction can be eased: a position is nudged toward the truth over a few frames and nobody sees the seam. Coins are the opposite, and they are here for that reason.

They are currency rather than score, which is the part that matters. A score is monotonic and write-only, so a client that briefly believes the wrong number is harmlessly corrected. A balance you **spend** has neither property: drift up and a purchase that looked affordable fails, drift down and the player is denied money they earned, and neither resolves on its own because the error only surfaces at the transaction.

A coin drops where an enemy died and goes to **whoever is nearest inside the pickup radius**. That rule is deterministic on the server and merely *probable* on a client, because a client judges "am I nearest?" against remote player positions that are a latency out of date. Thirty seconds, four players sharing a corner:

| config | coins | pickups taken back | refused purchases | wrong-rule packets | balance error |
|---|---|---|---|---|---|
| confirmed, 80 ms | 552 | 0 | 0 | 0 | -13 |
| confirmed, 250 ms | 548 | 0 | 0 | 0 | -10 |
| predicted, 80 ms | 551 | 4 | 1 | 1 | -1 |
| predicted, 250 ms | 547 | **15** | 1 | **18** | 6 |

**The cost of predicting a discrete event is a snap, and it scales with latency.** Fifteen times in thirty seconds at 250 ms, a coin was shown collected and then taken back. There is no `ErrorSmoother` for this: no continuous path exists between "you have this coin" and "you do not". That is the whole argument for the default being off, and for what most games actually ship, which is to vanish the coin immediately as a local cosmetic and let the number arrive a round trip later. Nobody is frame-sensitive about a counter.

**A predicted balance must be an offset on confirmed state, never its own counter.** The first version maintained a local running total and drifted **115 coins** over a run, because it modelled income and not spending: every purchase the server approved decremented the authoritative balance and left the local one untouched. Deriving it as `confirmed + outstanding predictions - pending purchases` cannot drift, because there is nothing to drift from, and it absorbs anything the server does that the client never modelled. It is the same shape as replaying unacknowledged inputs over an authoritative snapshot, which is what `PredictedPlayer` does for position, and the residual error in the table is just what is genuinely in flight.

**The repulsor is a pulse, and it started as a bug that looked like a rendering artifact.** The first version was a permanent aura with a hard sign flip at a fixed radius: flee inside it, chase outside it, both at chase speed. That is a stable equilibrium, so every enemy converged on exactly that radius and stopped, leaving a motionless ring the player could not be reached through. Two things worth extracting from it. Weakening the push would not have helped, because the equilibrium comes from the sign flip and not the magnitude; only making the repulsion *intermittent* removes the radius at which net motion is zero. And the ring quietly flattered every accuracy readout, because stationary entities are trivially easy to predict, so a rule bug was making the netcode look better than it was.

It now fires for 700 ms every 7 seconds, at a radius chosen per pulse between 28 and 95 px, pushing at 60% of chase speed. The radius is **derived rather than sampled**: both sides run the same hash of the pulse index, because a random parameter drawn from a local generator would give the server and each client a different answer and diverge the simulation for a reason no correction stream could explain. It also makes the pulse a second consumer of clock sync, since the phase depends on agreeing what time it is.

**An optimistic purchase makes the client simulate the wrong world.** `Repulsor` makes nearby enemies flee its owner, and it is an input to `step_enemy`, the rule every client runs locally. So a purchase shown before it is confirmed is not a wrong number, it is a wrong *rule*: for as long as the refusal takes to arrive, the client has enemies fleeing a player the server has them chasing. Eighteen packets' worth at 250 ms, and zero when waiting for confirmation. Nothing else in these examples has that shape; every divergence measured until now came from latency or from chaos, never from a wrong boolean upstream of the physics.

**Coins are drawn in by everyone, not just by the magnet upgrade, and that started as a tuning bug.** The nova kills out to 190 px and drops coins where enemies died, but the pickup rule only reached 46, a source four times wider than its sink. Measured, **35% of all coins expired where they fell** and an average of 112 sat on the ground at any moment, which quietly made the magnet compulsory rather than optional. Two numbers had to change, and only the first was obvious:

| config | claimed | expired | mean coins on the ground |
|---|---|---|---|
| before, no upgrades | 648 | 354 (35%) | 111.8 |
| after base pull, no upgrades | 1064 | 0 | 16.6 |
| after base pull, with magnet | 1065 | 0 | 8.7 |

Widening the reach was not enough on its own. The first attempt pulled at 115 px/s against a player speed of 190, so a moving player **outran their own attraction**: the coin fell behind, left the radius, and stopped where it was abandoned. A pull has to beat the thing it is pulling toward. The magnet is now a reach upgrade on a rule that already works, rather than the rescue for one that does not.

**Nothing announced any of this, which made it unreadable.** An upgrade changed the wallet and changed enemy behaviour, and the only signal was a number quietly going down, which is indistinguishable from a bug. There is now a persistent banner of coins and owned upgrades, and a short-lived notice when one is acquired or refused. Both are derived **client-side by diffing the wallet**, not sent by the server: the fact was already communicated by the state change, and spending wire bytes to say it again in words would be paying twice for one event.

**A collected coin flies to its winner over a fixed time, and that is deliberately not a shared rule.** Constant speed would give a fixed *speed*, so a coin taken from the rim of the pickup radius would trail one taken from underfoot; a fixed duration against a target that is itself running means re-interpolating every frame rather than following a path computed once at claim time. It runs for 900 ms with `plaza_client_utils::ease_in_quad`, so the pull grows as it closes.

The first attempt used cubic over 320 ms and looked like nothing at all, which is worth recording because the cause is not obvious from the code. Cubic ease-in covers 1.6% of the distance at a quarter of the time and 12.5% at half; over nineteen frames that means the coin holds still for ten of them and then crosses the gap in nine. **An acceleration nobody can see is indistinguishable from a teleport.** Quadratic covers 25% by half time and still finishes fast, and lengthening the flight gives it room to be watched.

The more interesting half is which side runs it. Magnet drift **must** be a shared rule, because it changes which player ends up nearest and therefore decides the authoritative outcome. The flight happens *after* the claim is settled, so nothing about it can change the result, and it is client-side presentation only. Running it on the server would spend bandwidth and coupling on an animation, and would create a third state between "on the field" and "banked" that the loss-recovery machinery would then have to reason about. As presentation, a lost packet costs an animation rather than a currency inconsistency. That is the counterexample to this example's own thesis: clients should run the shared rule locally, *and* determinism buys nothing once the decision is made.

**Refused purchases are almost never the problem, which was not the prediction.** Going in, the expected failure was a client believing it could afford something it could not. Measured, it happens once per run. The conservative derivation above is why: subtracting pending purchases means the client under-commits rather than over-commits. The currency hazard is real but it is designed away by getting the balance model right, and what is left is entirely the un-collectable coin.

Three measurement placements had to be fixed before any of this read correctly, all the same shape as earlier mistakes in this project. Sampling `upgrade_disagreements` at the end of a run reports zero for a transient that has since resolved, so it had to become a cumulative packet count. Counting that window *before* the belief is settled measures a one-packet lag rather than a misprediction, and showed a wrong-rule window even with prediction off. And `wants_to_buy` optimistically claimed the upgrade as well as recording the request, which made the belief run ahead in the control configuration; the belief is now derived in exactly one place.

## How it is built

Depends on `plaza_server_utils` and `plaza_client_utils` only, no server framework, because everything here is the pure netcode layer.

- **Server** ([src/sim/server.rs](src/sim/server.rs)): owns every enemy and simulates them at 60 Hz, then sends at `sync_hz`. Relevance is [`SpatialGrid`](../../server_utils/src/relevance.rs) rebuilt each send tick plus a [`VisibilitySet`](../../server_utils/src/relevance.rs) per player, whose diff is the spawn/despawn stream. Enemy *targets* are sent only when they change: the intent, not the output it produces.
- **Client** ([src/sim/client.rs](src/sim/client.rs)): holds only what it was sent, and draws it by one of the three strategies. In simulate mode it runs the same `step_enemy` rule the server runs, and forward-projects each arriving sample by its own age before correcting, so the correction targets *now* rather than where the enemy was a trip ago. Corrections ease through `ErrorSmoother`.
- **The shared rule** ([src/sim/types.rs](src/sim/types.rs)): `step_enemy` is the one function both sides run. Client-side behaviour simulation is only possible because it exists and is cheap.

The simulation is headless and is where the tests live (`cargo test -p horde_playground`); the renderer only reads its results.

## Notes

- Excluded from `default-members`, so a bare `cargo build` / `test` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it.
- The client loop here is hand-rolled on purpose. It is the consumer that should shape a `client_utils` bundle for behaviour-simulated remotes, rather than the bundle being guessed at first.
