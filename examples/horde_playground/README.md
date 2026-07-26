# horde_playground

The many-entity case: thousands of enemies, a handful of players (four by default, up to 128 on a slider) standing in different parts of a world far larger than one screen, and a bandwidth budget. A "bullet heaven" (Vampire Survivors shaped) horde, networked.

Where [`netcode_playground`](../netcode_playground/) shows one player's mechanisms up close and [`rollback_playground`](../rollback_playground/) shows the peer-to-peer family, this one is about **scale**: what you send, to whom, how often, and how a client draws things it is barely told about.

You drive your player with **WASD / arrows** (or, on a phone, touch and drag anywhere for a floating joystick); the weapons aim and fire themselves, Vampire Survivors style. Every 4.5 seconds each player emits an **area pulse** that wipes every enemy within 190 units at once (an expanding ring marks it), and reinforcements keep arriving into the slots the dead freed.

You are a target, not an invulnerable camera. Enemies pressed against you deal a discrete hit (a red flash, a health bar over your head, and a floating damage number where your shots land), then a brief invulnerability so a whole pile cannot delete you in one instant. Being reduced to zero refills your health and gives a longer shield in place, and a **difficulty** multiplier ramps over the minutes, speeding enemies up and hitting harder, with a notice announced at each step. The difficulty is derived from the clock the same way the repulsor pulse is, so it is a third consumer of clock sync: a client whose clock is off ramps at a slightly different moment and its enemies move at a subtly wrong speed. The health, the shield, and the hit events cross the wire (a few bytes); enemy *health* never does, because a client only needs the outcome, not thousands of running totals. With several players pulsing together into a converged horde, a single tick can kill several hundred, which is exactly the mass-despawn burst measured below.

## Running it

**The host is the server.** A native run hosts by default and its own player is just another client on a real socket, which is what keeps every omniscient readout it shows legitimate. One `--role` argument decides what a process is:

```sh
./run-native.sh                                                  # --role host: play, and serve joiners
./run-native.sh -- --role observer                               # watch and drive the settings, no player of your own
./run-native.sh -- --role client --connect ws://<host>:8080/ws   # join someone else's arena
./serve.sh                                                       # build the browser client and host it; open the printed URL
```

| `--role` | server | window | your own player |
|---|---|---|---|
| `headless` | yes | no | no | the windowless deployable, and what `serve.sh` runs |
| `observer` | yes | yes | no | full control panel, watching; a free camera (drag / WASD / wheel, `C` recenters) |
| `host` | yes | yes | yes | plays and serves. **The default** |
| `client` | no | yes | yes | join only. The only role a browser can take |

A host prints a local URL and a LAN URL; open either in a browser to join, or send the LAN one to a friend. The browser client connects back to whoever served it (over `wss://` if the page was secure), so a `--role headless` deploy behind a TLS terminator works the same way. `serve.sh` builds the wasm and hosts it in one step. The arena seats four by default, up to 128 on the panel's **players** slider; joiners fill the seats and bots drive whatever is empty. Lowering the count never evicts anyone already playing: it applies as people leave, and a joiner who finds no seat is told so rather than left on a black screen.

The pure single-process teaching build, with no networking compiled in at all, is still here and is where the measurements below come from: `cargo run -p horde_playground --no-default-features --features native,client`.

## What you are looking at

The **main view** follows your player and draws only what your client was actually sent: the enemies inside its relevance radius (solid), with a faint **ghost** ahead of each. The ghost is on by default and has a switch.

**The naming is the opposite of what it sounds like, and getting it backwards makes the overlay unreadable.** The solid marker is the **actual** position: the server's resolved state at the instant being drawn, played out of the buffer in order. It is correct, not an approximation of anything. The ghost is the *future*, and it is a future this client already holds, because a packet is applied only once its timestamp is reached and everything newer waits in the queue.

So the gap is **the render delay made visible**, not an error. It is where the marker is about to resolve to. A widening gap means the buffer is filling; an empty ghost means it has run dry and the client is about to have nothing left to play. (Render delay, not the input playout delay: that one sits between your keys and the world, and never appears on screen.)

**Every role has one, and they differ only in how far ahead they see.** A joiner's ghost is its queue, one render delay out. A host's is the server's state now, which is the same thing a link delay further ahead, because it is the server. A queued packet is received state, so a joiner needs no privilege to have this and does not get a lesser version of it.

### The ghost is a server permission, not a client setting

**A client cannot draw a future it was not sent**, so whether a ghost exists at all is the server's call, which is why a shipped game exposes it as a server option rather than a graphics one. The panel has both halves: **send unresolved frames** is the server's, **draw the ghost** is the client's, and only the first is load-bearing. Once a frame is in a client's memory a cheat client reads it whether or not the honest renderer draws it.

A host gates its own overlay on the same permission, so it can see what it did to everyone else. An observer stays omniscient; spectating is its job.

**It is declared, not enforced**, which is stated here rather than glossed: an honest client obeys the flag and a cheat client would not. Real enforcement means never sending past a client's render instant, and the obvious implementation, delaying the send, was tried and measurably does nothing. The client's playout clock is derived from the stream, so delaying the whole stream shifts the clock with it and the buffer depth comes out unchanged, to the millisecond. Enforcement means withholding against the declared timeline, which is now well defined but not built.

## The first frames, when there is nothing to draw

Drawing the whole scene at `server_now - render_delay` means there is nothing to draw until the timeline has started and a frame has been played out of it. So the world **fades in** once there is one, and until then the screen says what it is waiting for: connecting, then checking your connection, then the fade.

Without it the first second is not an empty world but a wrong one, and the arena is 3000 units square measured from a corner, so a camera with no player to follow points at the outside of it. Both playgrounds do this now; the offline builds do not need it, because they own both sides and their world exists from the first frame.

## Relevance applies to players too, or it does not scale

The example's whole claim is that per-player relevance beats broadcast, and for a long time it applied that only to the enemies. Player state (positions, health, shields) went to everybody on both streams, and every wallet rode in every packet. That is `O(players^2)`, and at 128 players it was **81% of all downstream traffic** while the three thousand enemies, which do get relevance, were 9%.

Measured by `cargo run -p horde_playground --release --example players --no-default-features --features native,client`, at 3000 enemies:

| players | per-player traffic before | after |
|---|---|---|
| 4 | 3 KiB/s | 1 KiB/s |
| 32 | 198 KiB/s | 12 KiB/s |
| 128 | 3148 KiB/s | 137 KiB/s |

The `O(players^2)` term is gone: what a client is sent about *other players* is now flat in the arena's size. Total downstream at 128 players is 2.1 MiB/s, and almost all of it is the enemies, which is 128 viewers each holding their own slice of a 3000-strong horde.

Four changes, and the first two are the whole of it. **The player stream is per recipient**, carrying only the players you can see or that an enemy you hold is chasing. That second clause is the one that is easy to miss: `step_enemy` aims at a player, so a target you cannot place is a rule you cannot run, and skipping it makes your horde drift from the server's. **The entity packet no longer carries players at all**, because it was sending the same thing the player stream already sends, at a different rate. **Wallets are sent when they change** rather than restated every packet, and only for players who matter to you. **Shots are events**, an origin, a velocity and a fire time, rather than a live set re-sent for the whole 1.4 s flight.

The cost is CPU: computing who is relevant is a pass over the players plus a pass over your visible enemies, and it costs about a tenth more server time per simulated second.

**A caveat about measuring any of this**, learned by getting it wrong here. A fixed cap of 40 spawns per wave could not keep up with 128 players' kill rate, so the horde collapsed to about 40 alive out of 3000 and every measurement above was really a measurement of an empty arena. The cap scales with the player count now. Watch the `alive` readout before trusting a bandwidth number: a world that is not there is cheap to send.

Two consequences worth knowing. A player who has never been sent to you still occupies a slot in every per-player array, holding the arena-centre seed, so the renderer has to know the difference between "at the centre" and "never heard of": drawing the seed puts a peer in the middle of the map who is not there. And a peer who walks out of your relevance stops updating and freezes at their last known position, which is correct (you cannot see them) but means any measurement of peer freshness has to sample only while the peer is actually relevant, or it measures the feature rather than the link.

## One timeline, declared by the server

The render delay is **a property of the world, not of anybody's link**. Every client shows `server_now - render_delay_ms`, the same instant on every screen.

It used to be `send_interval + 2x measured jitter`, recomputed continuously from packet arrivals. That conflates two unrelated things: latency and jitter describe *when bytes show up*, while the render delay describes *which moment is on screen*. Letting the first move the second means no two clients agree on what "now" is, the server cannot say what any of them has yet to play, and a player on a bad link is quietly shown an older world while every readout says fine.

**How wide it has to be: `one_way + jitter + one send interval`.** The latency term is the one that surprises. T is on the server's clock, but the newest sample a client holds is already one trip old when it arrives, so a delay short of the trip puts T ahead of every sample the client has, leaving nothing to interpolate between. Measured at 80 ms latency, 20 ms jitter, a 10 Hz player stream, as the worst single frame a peer moves:

| declared delay | worst step |
|---|---|
| 100 ms | 25.6 px, identical to the raw sample: not interpolating at all |
| 150 ms | 18.5 px |
| 200 ms | 6.0 px |
| 250 ms | 4.0 px |

The old design hid that term by steering its clock to the *packet's timestamp* rather than to server-now, so its estimate was already one trip behind and the trip never had to be declared. It was real either way; now it is a number somebody chose.

**The consequence is a fairness property, not a cost.** One timeline for everybody means the declared delay must cover the *worst* player's latency, or that player cannot render at T at all. Everyone waits for the slowest, which is the same trade the input playout buffer makes in the other direction.

**And a bad link now says so.** A packet that arrives after the instant it describes has already gone past is an **underrun**, counted in the panel. Measured over 600 ticks:

| link jitter | declared delay | ghost | underruns |
|---|---|---|---|
| 0 ms | 30 ms | 0 | 153 |
| 0 ms | 100 ms | 22 | 0 |
| 60 ms | 100 ms | 15 | 126 |
| 60 ms | 200 ms | 22 | 0 |
| 200 ms | 200 ms | 22 | 100 |
| 200 ms | 400 ms | 20 | 0 |

Declare enough and the ghost is full and nothing is late. Declare too little and the ghost empties and the underruns climb, which is the same starvation the old design absorbed silently.

The **minimap** shows the whole arena with every enemy the server is simulating. The dots outside your circle are what relevance is saving you from receiving.

| Control | What it shows |
|---|---|
| **per-player relevance** | turn it off and watch bandwidth explode: every player is sent every entity |
| **entity send rate** | defaults to 16 Hz; drop it to 1 Hz, the rate a shipped horde co-op actually uses, and see which drawing mode survives |
| **player send rate** | the other knob, defaulting to 30 Hz. Collapse it onto the entity rate to see why one rate is not enough |
| **how remotes are drawn** | simulate (run the AI rule locally), dead reckon (last velocity), or interpolate (render in the past) |
| **latency / jitter / loss** | real impairment on real connections, in both directions. The host feels its own settings, so what it shows a joiner is what it is living with |
| **recover from loss** | diff against the last packet the client *acknowledged* instead of the last one sent. Off is the naive stream, and at 25% loss it strands hundreds of corpses |
| **playout delay** | how long the server holds an input before executing it. Off is apply-on-arrival, which decides contested outcomes by ping |
| **crowd level of detail** | the opening angle for summarising the world outside your view radius, instead of knowing nothing about it |
| **players spread / clustered** | clustered players make the horde converge on one spot, raising local density |
| **ease corrections** | smoothing, with the caveat measured below |
| **input playout delay** | how long the server holds an input before executing it, which is what makes a contested pickup independent of ping |
| **render delay** | how far behind the server clock every client shows the world. One number for the whole session; too small and the underrun counter climbs |
| **send unresolved frames** | the server's permission, and what makes a ghost possible at all |
| **draw the ghost** | the client's half. Where each entity is *going* to be; the gap to the solid marker is the playout delay, not an error |
| **weapons, deaths, and waves** | combat off leaves a pure movement horde, for isolating the networking |
| **coins, and predicting your balance** | a discrete, contested event, where a correction is a snap rather than an ease |
| **generational entity handles** | a handle names a slot *and* its occupant; off, a reference to a dead entity would land on whoever recycled its slot |
| **send input only on change** | your upstream. Off is an input every tick (loss-robust); on transmits only when your direction changes plus a keepalive, which the local player being unforced makes safe |
| **debug digest** | ship the server's exact key set, so a mismatch prints which enemies you hold in error and which you are short of, rather than only counting |

Press `R` to re-baseline the readouts after changing something. A frame counter sits bottom right: with these entity counts, telling a client-side stall apart from a network effect matters.

## What it measured

The example exists to settle questions by measurement rather than argument, and it corrected several assumptions. Numbers are 3000 enemies, 4 players, 420px view in a 3000px arena.

**Relevance pays, clustered or not.** 91% of the broadcast culled with players spread, **90% clustered** (28.0 vs 325.0 KiB/s spread; 31.5 vs 325.3 clustered). An earlier guess that relevance mostly matters when players separate was wrong: the arena dwarfs the view either way. Clustering actually makes it slightly *worse*, because the horde converges on grouped players and raises local density.

**Running the behaviour rule locally wins at every send rate** (mean / worst error, px):

| send rate | simulate | dead reckon | interpolate |
|---|---|---|---|
| 1 Hz | **12 / 23** | 33 / 99 | 55 / 88 |
| 2 Hz | **11 / 18** | 12 / 34 | 19 / 30 |
| 4 Hz | **10 / 17** | 11 / 30 | 19 / 30 |
| 10 Hz | **9 / 15** | 10 / 19 | 12 / 19 |
| 30 Hz | **9 / 15** | 10 / 17 | 11 / 17 |

It is decisive at 1 Hz, which is what makes a very low send rate viable, and it stays ahead everywhere else. Interpolation is the most *consistent* (its worst case barely moves) but always renders about one send interval in the past, which at 1 Hz is a second.

**This table used to say the opposite above 10 Hz, and the reason is worth more than the numbers.** Simulate used to get *worse* as the rate rose (10, then 16, then 20 px at 4, 10 and 30 Hz), and that was read as a property of the technique: a low-rate tool, not a general upgrade. It was a property of the *correction*. A 250 ms `ErrorSmoother` ease never completes when corrections arrive every 33 ms, so the smoother itself became the dominant error. Moving enemies onto `HeldInputPredictor`, which closes a fraction of the gap per correction and has no duration to outlast, removed the failure entirely. **A measurement of a technique was really a measurement of one tunable inside it**, which is the same shape as the extrapolation clamp that once flattered second-order dead reckoning.

**Correction smoothing can become the error, and the fix is to pick a correction with no duration in it.** A fixed-duration ease has a rate above which it never completes, so corrections pile up and the smoother dominates. Either keep the ease shorter than the send interval, or use a per-correction *fraction* (`HeldInputConfig::blend`) which has no duration to outlast and is what enemies use here. The second is why the table above no longer degrades at high rates.

**Compact ids and quantized positions save a consistent 71%** (3.4x) at every send rate: a 3-byte id (`u16` index + `u8` generation) and a position quantized to two `u16`s, against a 16-byte UUID and two `f32`s.

**The bytes are not where the interesting encoding problem is.** Broken down by part: samples (position corrections) **86.1%**, spawns 11.1%, despawns **1.2%**, everything else 1.6%. Despawn ids are encoded here as sorted varint deltas, which measured 55% cheaper than three-bytes-per-id (and beat both a presence bitmask and run-length encoding on the real burst data), but 55% of 1.2% is 0.7% of the packet. Worth knowing before optimising a stream: check its share first.

**Churn is modest, but it spikes.** Steady movement produces ~23 spawns and ~15 despawns per packet, cheap enough that compressing it would save little. The area pulse is the exception: one wipe despawned **278 entities in a single packet**. That burst, not the steady churn, is the case a range encoding would be for.

**Generational handles were not needed here, which was a surprise.** Across 413 kills with slots actively recycling under 80 ms of latency, the client recorded **zero** stale handle references. Two things make the generation redundant in this configuration: delivery is ordered, and the server announces a death *explicitly* (clearing the visibility bit) before the next diff, so a reused slot reads as despawn-then-spawn rather than silently becoming a different entity. Generational handles are insurance for **unordered** transport, where a stale packet can overtake the despawn that would have invalidated it, not a prerequisite for slot recycling. Filed as a keystone item before this measured it; the measurement says otherwise.

## One rate is not enough, and the error metric could not see why

A late finding, from playing rather than from a test. At 1 Hz the horde *looked* like 1 Hz, which contradicts the table above and the case study both.

Both were right and both were measuring the wrong thing. `mean_render_error` compares an enemy's position against server truth, and that number is genuinely good, because every client runs the enemies' own rule. What it cannot see is **continuity**, and what it does not cover at all is **players**. Two things were actually running at 1 Hz. Remote players had no smoothing whatsoever, so peers teleported once a second. And `step_enemy` aims at `players[target]`, so the whole horde was gliding smoothly toward a point that jumped once a second: at player speed that point is up to 190 px stale, nearly half a view radius, while a Swarm enemy covers 62 px in the same time. A *synchronised* heading change across hundreds of entities is far more visible than the same magnitude of error scattered randomly, and a positional mean cannot express it at all.

**The fix is two send rates, because the two streams answer different questions.** Enemy positions may be stale: they are the behaviour's *output*, and every client recomputes it. Player positions may not: they are the behaviour's *input*. Measured on the fixed build, one shared 1 Hz rate leaves a peer up to **204 px** behind the truth, matching the predicted 190; splitting the rates (1 Hz entities, 30 Hz players) takes it to **7 px**, and the entity stream is untouched. Players are a handful of entities, so sending them often is nearly free.

The general lesson, and it is not the first time here: **an error metric and a smoothness metric are different measurements, and averaging position error hides every discontinuity.** If something looks wrong and the numbers look right, suspect the numbers measure a different quantity than the eye does.

## One timeline for everything remote

Peers are now drawn through `RemoteView`, interpolating between two real snapshots and **not** extrapolating, which is Gambetta's entity interpolation as written: dead reckoning a *player* is guessing at a human's intention, which nothing on the wire carries, so it overshoots every direction change and snaps back when the truth lands.

Putting peers on a delayed timeline exposed that they were the only thing on it. Everything else (deaths, sparks, claims, health, projectiles) applied on arrival, so one scene was being drawn from three different clocks and every seam between them was a visible bug. Packets are now queued on receipt and applied when the render clock reaches them, so a frame is one consistent instant, and a `RenderAt` token makes drawing at any other instant inexpressible rather than merely discouraged.

That in turn exposed which entities can actually *be* on a delayed timeline. **An entity can join one only if the client can reconstruct its state at an arbitrary past instant.** A peer can, because `RemoteView` keeps a snapshot buffer. A projectile could not: the client held only the newest list and replaced it wholesale each packet, so once the server stopped listing a shot there was nothing left to draw it from, and any shot fired and destroyed inside the render delay had never existed at the target. At a 4 Hz player rate that was **every** shot: none were drawn at all.

For a shot the reconstruction is nearly free, and it is what the wire now carries. A `ProjectileSpawn` is an origin, a velocity and the time it was fired, sent **once** as an event; the client flies it locally and can evaluate it at any instant exactly. Shots drawn per frame went from 0.2, 0.1 and **0.0** at 30, 16 and 4 Hz to essentially rate-independent. It is also cheaper, one message instead of an entry in every packet for the whole flight, and it is the same move as everything else that has worked here: **send the input to the behaviour, not the behaviour's output.** The *held* count still rises with the delay, because a shot fired after the instant being rendered is queued until the timeline reaches it, which is the buffer working rather than a loss.

## Nothing about your own player is predicted either

The obvious exception to one timeline was the local player, drawn predicted at *now* while the world around it was drawn at T. That is the same seam as the three clocks, kept for the usual reason: prediction is how you hide latency on your own input.

It does not survive contact with a **scheduled** server. Your input executes at the tick you named, `press + playout_delay`, and a prediction that applies it immediately is simulating a different world for that whole window. Measured with the correction switched off entirely, at zero latency, a single reversal banks a permanent **44 px** offset, because the velocity disagreement heals when the schedule catches up and the displacement it already integrated does not. Something has to give that back, and the giving back is what you feel as stiffness on every change of direction.

So the local player is now drawn from the played-out stream at `RenderAt`, like everything else. There is no prediction, therefore no correction, therefore no stiffness: the mechanism is gone rather than tuned. `HeldInputPredictor`, the correction monitor, and the reconcile path are all deleted from the client, and the renderer lost its special cases (your repulsor ring used to be pinned to the predicted marker while peers' rings sat on their authoritative positions).

**What it costs is one number, and the panel prints it:** `playout_delay + render_delay`, 250 ms at the defaults. That is not new latency. The server already refused to turn you for 100 ms and the world was already drawn 150 ms back; the client was drawing something else in the meantime and then being dragged off it. Both delays now have sliders, next to each other, with the sum stated, because each was justified separately and nobody had costed the total against the hand.

**What it buys, beyond the fix:** a recording replays to exactly what every player saw, *including their own screen*. A predicted local player can never give you that, because what you saw was never what happened.

`cargo run --release -p horde_playground --example reversal` is the measurement, including the strategies that were tried and rejected.

## More arenas, and a placement rather than a door slam

An arena that schedules inputs ahead can only carry a connection whose delay fits the schedule, so **one** arena means one budget and everybody past it is turned away. `--rooms <n>` runs more, in the order they are worth adding:

| `--rooms` | room | playout delay | carries a link up to |
|---|---|---|---|
| 1 (default) | standard | 100 ms | 164 ms one way |
| 2 | relaxed | 300 ms | 364 ms one way |
| 3 | sharp | 50 ms | 114 ms one way |

**One by default, and that is not timidity.** Each room is a whole simulation of thousands of enemies at 60 Hz, so a local run would pay three times over for a spread of latency that one player on one machine does not have. Extra arenas earn their cost the moment real connections arrive, which is a deployment decision rather than a property of the example: `--rooms 3` is what you would publish with.

The table is deliberately **not** sorted by depth, which would read better and mislead. The first is the arena that used to be the only one, so a default run is exactly what it always was. The second worth adding is the *deeper* one, because it is the one that rescues links that would otherwise be refused; a sharper room is a nicety for players who already had somewhere to play. The whole set is one trade written three times: a deeper schedule carries a worse connection and costs everybody in that room more input lag.

Connect anywhere, get measured, and if this arena cannot carry you the server replies with the one that can and the client reconnects. With a single room there is nowhere to send anybody, so placement degrades to exactly the refusal it started as. **Refusal is what is left when nothing fits**, not the primary behaviour, and that is the whole reason the decision wants a lobby: a room can only say yes or no, a lobby can say *where*. The matching rule itself is [`plaza_lobby::routing::best_for`](../../lobby/src/routing.rs), which orders tightest-schedule-first so a fast link is not sent to the room built for slow ones and made to pay its delay.

The placement names a **path**, never a whole address. The arena does not know what hostname a client reached it by, and inventing one is how a redirect sends somebody to a machine they cannot route to.

## Admission: measured at the door, not seated and then silently broken

The schedule has a ceiling, and it used to be enforced by accident. An input is named for `press + playout_delay` and rejected once it lands more than `input_max_late_ticks` past it, so above about **164 ms one way (330 ms round trip)** every input a player sent was dropped. They were welcomed, given a seat, and could not move, with nothing on screen to say why.

So the server measures before it seats. The **transport** times its own WebSocket ping frames, so this costs the game's wire format nothing at all, and the arena admits once eight samples exist and the minimum round trip halves to inside `playout_delay + late_window`. The mean has to fit the schedule and the spread has to fit the tolerance, which is the honest reading of what the arena can carry: **the late window is the jitter allowance**, so admission asks whether your jitter fits in the window that already exists rather than inventing a second number.

Three details are deliberate:

- **The budget is derived, never declared.** It is exactly the condition that would break a player, so it moves with the sliders instead of drifting out of step with them and admitting people who then cannot play.
- **The server times its own probe**, at the transport layer. A client reporting its own ping could understate it, and this is the check that gates entry. Timing the probe is spoof-proof in the direction that matters: a client can only make itself look *worse*. It also means admission needed no new message: `plaza_session` exposes `agent_rtt` and the arena asks.
- **An arena that cannot measure admits nobody.** Failing closed is the point: guessing that an unmeasured connection is fine is how the silent exclusion happened.
- **No seat is held while measuring**, or a slow joiner parks one for a second and a full arena refuses somebody who would have got in.

There is no exemption for the host. Its loopback ping is near zero so it passes on merit, which is the point: a host that admitted itself by special case would stop being just another client on a real socket, and that is what makes its omniscient readouts trustworthy.

The client shows "checking your connection" while it runs and, if refused, what was measured against what the arena allows. A refusal is a statement, not a silence.

**Admission is a snapshot**, so it says nothing about a connection that degrades later. That case still shows up as rejected inputs on the server's counters.

## The server owns time

Inputs used to be applied on arrival, which quietly makes ping an input to the game. A 20 ms player's press lands on the next tick and a 200 ms player's lands nine ticks later, so any outcome decided by who was where first is decided by connection quality. The panel's **playout delay** switches between the two.

A client now names the **tick** an input is for, not a timestamp, and the difference is authority. A timestamp is the client naming a moment, which the server then has to judge plausible; judging it needs a shared clock, a shared clock is an estimate, and the estimate's error is the slack a liar hides in. A tick is the client naming *the server's own unit of time*, which is either still open or is not. Both sides compute it from the same rule, so two players who pressed at the same instant name the same tick however far apart their pings are. The server buffers by tick and executes in tick order, which also makes the rate a client runs at irrelevant: a 120 Hz client and a 30 Hz one both name ticks.

The server takes that tick as an intention, never as a fact. Outside the accepting window an input is **rejected, not clamped**: clamping a wild tick into range executes an input the client never asked for at that moment, which is worse than dropping it and is indistinguishable from a working system. Both bounds are settings rather than constants, because they are genre decisions. `input_max_late_ticks` tight is what a competitive shooter wants, where a closed tick stays closed and a player who cannot reach the window loses inputs and rubber-bands; loose forgives a jittery link at the cost of letting a slightly stale input take effect, and widening it is also what a lag switch wants, so it should be sized from what honest links actually do rather than picked. `input_max_early_ticks` has to cover the playout depth, since that is exactly how far ahead an honest client aims; beyond it a client is parking inputs in the future. Sequence numbers stop a replay of an input that was legitimate when it was sent.

The panel counts accepted, rejected and late inputs, and accepted is the denominator: rejection and lateness counts mean nothing without knowing how many arrived.

**One bug in here is worth keeping.** Changing the enemy count made the player uncontrollable. Rebuilding the world preserved `clock_ms` but reset a separate tick counter to zero, so every input was rejected as impossibly far ahead. The tick is now *derived* from the clock rather than counted alongside it, which is the same shape as every other fix in this project: two representations of one fact will eventually disagree.

## Packet loss, on every path

The loss slider had worked in the offline `World`, which is where every measurement above was taken, and did nothing at all on the path a host or joiner actually uses: the impairment link took latency and jitter from the panel and a hardcoded zero for loss. The upstream was worse, because there was none. Inputs, acknowledgements and purchases went over a real socket unimpaired, so the return path was perfect however far the slider was dragged, and that is exactly the traffic the recovery machinery exists for: an acknowledgement is what lets the server tell a starved mirror from a healthy one, and it could never be lost.

Loss now applies in both directions. Inbound drops happen at the server, because that is where the controls live and a joiner has no reason to sabotage its own outbound. `Hello` and `Ping` are exempt: they are control plane, and dropping the version handshake makes a diagnostic flaky without teaching anything.

The general form is the one this project keeps relearning: **a toggle that is wired to the demo path and not the real one reports on a system nobody runs.**

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

All four details now live in [`plaza_server_utils::DeltaBaseline`](../../server_utils/src/delta.rs) and its client-side counterpart [`DeltaMirror`](../../client_utils/src/mirror.rs), which is what this example's own reliability layer was replaced by. The six regression tests here, all named after the failures above rather than after the functions they exercise, are what made that swap safe on a single run.

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

The headless sim (`src/sim/`) is the pure netcode layer and depends on `plaza_server_utils` and `plaza_client_utils` only. It is unchanged by the networking, so every measurement above still holds.

- **Server** ([src/sim/server.rs](src/sim/server.rs)): owns every enemy and simulates them at 60 Hz, then sends the entity stream at `sync_hz` and the player stream at `player_sync_hz`. Relevance is [`SpatialGrid`](../../server_utils/src/relevance.rs) rebuilt each send tick plus a [`VisibilitySet`](../../server_utils/src/relevance.rs) per player, whose diff feeds a [`DeltaBaseline`](../../server_utils/src/delta.rs) per seat: that owns the acknowledgement frontier, the rebuild and the digest drift check. Inputs arrive keyed by tick and are executed in tick order, not on arrival. Enemy *targets* are sent only when they change, and a projectile is sent once as a spawn event: the intent, not the output it produces.
- **Client** ([src/sim/client.rs](src/sim/client.rs)): holds only what it was sent, in a [`DeltaMirror`](../../client_utils/src/mirror.rs), and queues arriving packets to apply when its render clock reaches them, so a frame is one consistent instant. Enemies draw by one of the three strategies; in simulate mode it runs the same `step_enemy` rule the server runs. Peers go through `RemoteView`, interpolating only. Corrections ease through `ErrorSmoother`.
- **The shared rule** ([src/sim/types.rs](src/sim/types.rs)): `step_enemy` is the one function both sides run. Client-side behaviour simulation is only possible because it exists and is cheap.

The simulation is headless and is where the tests live (`cargo test -p horde_playground`); the renderer only reads its results.

**The networked layer wraps that sim without touching it** ([src/net/](src/net/)). The server side is `plaza` core (`StateController`, `StateLogic`, `TickDriver`) over `plaza_session`; the arena seats joiners, buffers each seat's movement, and routes the entity-stream acknowledgement and the one purchase request straight to the authoritative server, so the whole loss-recovery and currency machinery now runs over a real socket. The client side is a `plaza_ws::Socket` plus clock/RTT estimation; the local player is a `HeldInputPredictor` (the server holds a direction and integrates it, so replaying discrete inputs would double count), measured by a `CorrectionMonitor` and throttled by an `InputCoalescer`. The player is unforced, so the local integration is exact, which is what makes "send on change" safe. Cargo features name what you want to build: `client`, `server` (not on `web`), `native`, `web`, `websocket`.

## Notes

- Excluded from `default-members`, so a bare `cargo build` / `test` skips macroquad's dependency tree. `cargo <cmd> --workspace` includes it. Building for wasm needs `--no-default-features --features web`, because the default set pulls in the native socket and the actix server; `serve.sh` does this.
- The compiled `static/*.wasm` is a build artifact and is gitignored. Run `serve.sh` before serving a fresh checkout.
