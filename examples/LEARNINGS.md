# Learnings from the playgrounds

What the black hole and horde playgrounds taught while they became real listen-servers: principles that prevent whole classes of bug, a record of what broke and how each bug was found and what it all changed in plaza.

In almost every case the cause was somewhere other than where the symptom pointed. The wrong theories were reasonable, so they are written down next to the right one.

The principles come first. Following them costs nothing. The bugs in the catalogue could only have been prevented by writing the code differently rather than by adding a primitive.

## Principles

### Four core principles

Each detailed principle below is one of these four applied at a specific site and each bug in the catalogue violates exactly one of them. They are kept short so the list fits in your head, which makes violations easier to spot. Each was learned from a bug.

1. **Reproducibility.** The same inputs at the same ticks produce the same state everywhere. Concretely: an input is keyed to a tick derived from the shared clock, never from its arrival time; inputs execute in tick order; the rule that consumes them is one function both sides call rather than two implementations of the same idea; and **both sides advance that rule in the same quantum**, so a step is a whole tick on the authority and on every client. To test it, replay a recording: it must reproduce what every player saw, including their own screen.

   The last clause is the least obvious. `bomb_grid` hit it four times: a shared rule and tick-addressed inputs still reproduce nothing if the two sides do not step the same way. See [Four bugs with one shape](#four-bugs-with-one-shape-bomb-grid).

2. **One instant per frame.** The client picks a single instant T and evaluates everything in the frame at T. That covers where entities are drawn and also everything a behaviour rule reads while producing the frame, including aim targets, chase context and force fields. An entity drawn at T that reads its target from the newest packet puts two timelines in one scene and the seam between them is a bug even before it becomes visible.

3. **Simulation never reads presentation.** Smoothed, blended, faded or predicted state is output only. If a rule that both sides run consumes it, the client has a second, divergent world and every packet fights the local one.

4. **The timeline comes from declaration, not arrival.** Transport facts (round trips, jitter, arrival times) may size buffers and admit or refuse connections. They never decide which instant is drawn or when an input executes; those are declared numbers that the server chooses and publishes. If arrival time leaks into the timeline, principle 1 breaks without any visible error. A mechanism that adapts to a fault also hides the evidence of the fault.

### A shared rule must be shared code

This is the strongest correlation in either example. Both examples contain entities that followed it and entities that did not:

| entity | where the movement rule lives | divergence bugs |
|---|---|---|
| horde enemies | `step_enemy` in `sim/types.rs`, called by server, client and world | none |
| black hole pellets | `step_pellet` in `sim/types.rs`, called by both sides | none |
| black hole **hole** | server ran `step` plus `attract_holes` plus collision separation; the client had its own `apply_move` | three, found separately: the unpredicted pull, the frozen dead hole, the unpredicted dash |
| horde **local player** | server integrates a held direction; the client hand-rolled velocity integration | three: a threshold-snap sawtooth, enemies lunging at a predicted position and reversal stiffness. No longer predicted at all |

The entities whose step was one function called by both sides stayed correct as the game changed around them. The two whose step was written twice drifted without any error and each drift took days to find, because the symptom (a jerk, a jump) was far from the cause (a force the client did not model).

For any predicted entity, make the step a single shared pure function and have the client call it rather than reimplement part of it. `PredictedPlayer::apply` is meant to be the server's step function. Where sharing is genuinely impossible (different fidelity, server-only information), write it down as a deliberate exception with a known error budget.

If the client's step needs the world (gravity, wind, platforms) and the prediction API cannot pass the world in, you end up writing a second, lesser rule. That is a deficiency in the API and it is why `PredictedPlayer` gains a prediction context below.

### Predicted state is for presentation; shared rules consume authoritative state

In horde, feeding the locally predicted player position into the client's simulation made enemies chase a point the server was not chasing, so every packet snapped them back and they appeared to lunge whenever the player moved. The fix was to let the prediction drive only the camera and the player's own marker, while the shared rule kept reading the authoritative position, exactly as the offline build does.

Prediction is a rendering concern for one entity. If predicted state feeds a rule that both sides run, there are two divergent worlds and the packets fight each other. It is easy to get wrong because using the freshest local data looks like an improvement. Share the rule, but do not feed the prediction into it.

The same distinction shows up in rendering. The repulsor ring was drawn at the authoritative position while the player marker was drawn at the predicted one, so the ring visibly lagged and stuttered on packet arrival. Choose which position each element is drawn at: effects belonging to the local player follow the prediction and effects describing shared physics follow the authoritative state.

### Never let a correction accumulate to a threshold and snap

Horde's local player pulled toward the server only once the error passed a fixed threshold, then closed the whole gap at once. Holding a direction produced a regular cycle of drift and snap, which the player feels as a rhythmic tug forward. Easing a fraction of the error every packet absorbs the same drift without anything visible.

A hard snap is correct only for a genuine discontinuity such as a spawn, a respawn or a teleport. Those must not be eased, because easing a two thousand pixel jump draws the player sliding across the arena. Choose between the two by cause rather than by size: ease continuous error and snap discontinuities.

### Pair an integrity check with the data it checks

The relevance stream carried a digest, so divergence was detected immediately. The cause still took days to find, because a digest tells you the mirror is wrong but not how. Shipping the server's actual key set behind a debug switch turned the counter into a diff and one run showed the cause: every missing key was generation zero and they were spread across the whole slot range. That pattern means a client joined an arena already in progress rather than drifted.

Pair any checksum with an opt-in mode that ships the data it checksums. It costs a switch and some bandwidth while it is on and it turns a number you cannot act on into a diagnosis. The same applies to what a diagnostic prints. The first version of the correction log reported the server's dash flag, which reads true through a grapple because dashing is how a grapple is fought, so it pointed at a cause that was pure coincidence. Adding the distance to the nearest hole showed the real cause immediately.

### Diagnostic thresholds must adapt

A thirty pixel correction is normal at one send rate and alarming at another. The same goes for different latency settings and for how much contact the game currently has. A fixed pixel threshold only works for the conditions it was tuned under. Tracking a running mean and variance and reporting deviations from it is barely more code and keeps working when conditions change.

### Impairment tooling must be faithful to the transport it stands in for

A full diagnostic cycle went into a reordering hypothesis. WebSocket is TCP and cannot reorder, but the impairment link could, so a failure the real system cannot have looked worth chasing. Impairment tooling should produce only the failures the real transport can produce: delay and loss for an ordered transport and reordering only for a datagram one. Otherwise it produces false leads and hides the fact that the real system has stronger guarantees than the tests assume.

### A listen-server host has asymmetric internal latency

On a host, round trip probes return directly while frames pass through the outbound impairment path, so probes see no delay and frames see the full impairment. Feeding both into one clock estimator made the estimate wobble every probe interval and jerk the world, because the average describes neither path. Any estimator of clock offset, round trip time or jitter must be fed samples from a single path with consistent delay.

### Inputs name a server tick

Applying an input on arrival lets ping affect the game: a 20 ms player's press lands on the next tick and a 200 ms player's lands nine ticks later, so anything decided by who got somewhere first is decided by connection quality. Buffering inputs and executing them at the moment they were pressed plus a fixed playout delay puts everyone on the same footing, as long as the delay covers their latency. The delay adds to how long the world takes to react to you. Prediction was expected to hide that for your own movement and cannot (see the next section).

**Why a tick and not a timestamp.** A timestamp is the client naming a moment and the server then has to judge whether it is plausible. That needs a shared clock. A shared clock is an estimate and a cheating client can hide inside its error. A tick is the client naming a unit of the server's own time, which is either still open or already closed. Both sides compute it from one rule, so two players who pressed at the same instant name the same tick whatever their pings. A 120 Hz client and a 30 Hz one are on equal footing because neither is naming its own frames.

**Out-of-window inputs are rejected rather than clamped.** An input outside the accepting window is dropped. Clamping a wild tick into range executes an input at a moment the client never asked for, which is worse than dropping it and looks like a working system from outside. Sequence numbers cover replays of an input that was legitimate when it was sent.

**Both bounds are settings, because the right values depend on the genre.** A competitive shooter wants tight lateness: a closed tick stays closed and a player who cannot reach the window loses inputs and rubber-bands. A loose bound forgives a jittery link but lets a slightly stale input take effect. Widening it also helps a lag switch, so size it from what honest links actually measure rather than picking a number. The earliness bound has to cover the playout depth, since that is how far ahead an honest client aims.

### Prediction against a server that schedules inputs

Prediction hides your own latency by applying your input at once, assuming the server is about to do the same. A server that executes inputs **on a schedule** does not: your input runs at `press + playout_delay` and until then the prediction is simulating a world the server is not in.

The disagreement does not go away. With the correction switched off, at zero latency, a single reversal leaves a permanent **44 px** offset. The velocity error is temporary, since both sides end up holding the same direction, but nothing pulls back the displacement it already integrated. The correction has to remove it, which drags the player against the direction they are steering and feels like stiffness.

Three fixes were measured and all were worse than the fourth. Predicting the schedule costs the same lag. Aiming at your own delay brings back the ping advantage the schedule was there to remove. Replaying makes the correction exact but does not remove it.

**The fourth fix is to stop predicting.** If the client already draws the world at a delayed instant, it can draw the local player there too, from the played-out stream, like every other entity. The cost is `playout_delay + render_delay` of input lag. Both delays were already being paid, one on the server and one in rendering. Before the change the client drew something else in between.

This is principle 1 again. Prediction is only sound when it predicts what the server will do and when it will do it. A predictor with the right rule and the wrong timing simulates a different world from the server's.

It also means a recording replays to exactly what every player saw, including their own screen, which is impossible with a predicted local player.

### Transport variance must not move the timeline

Latency and jitter describe when bytes arrive, which is a property of one link. The render delay describes which moment is on screen, which applies to the whole world.

Horde sized its render delay from measured arrival jitter, recomputed continuously, so the transport decided what moment each client displayed. That had three consequences, none visible from the code:

- **No two clients agree on "now."** Each picks its own T, so there is no shared instant to reason about.
- **The server cannot say what a client has yet to play**, which makes any question about unresolved state unanswerable.
- **A bad link is hidden rather than reported.** The buffer widens until late packets fit, so that player is shown an older world than everyone else and every readout looks healthy.

Fixing T at `server_now - declared_delay` makes the delay a chosen number and turns the third problem into an **underrun counter**: a packet that arrives after the instant it describes is counted instead of absorbed. A mechanism that silently compensates for a fault also removes the evidence of it. This project found that in several places.

Two consequences to know before adopting it. The declared delay must cover `one_way + jitter + one send interval`, because the newest sample a client holds is already a trip old. The old design hid that term by steering its clock to the packet's timestamp rather than to server-now. The delay also becomes a fairness setting: with one timeline everyone waits for the slowest player, as the input playout buffer does in the other direction.

### The input schedule's unstated latency ceiling

Horde's input schedule always had a ceiling: past `playout_delay + late_window` of one-way delay, every input a player sends lands outside the accepting window and is dropped. The ceiling was not declared, measured or reported anywhere. A player above it was admitted and seated and then could not move.

Because the limit was never checked at admission, it looked like a broken game rather than an unsuitable connection. It was also invisible from inside: the server counted rejections, the client counted nothing and the screen showed a player who would not respond.

The fix has two parts. **Derive the limit from what actually enforces it** rather than restating it as a constant, so the two cannot drift apart. And **check it on admission**, with the server timing its own probes, because a client reporting its own latency can understate it.

Server-measured admission can only be gamed by making your connection look worse. The alternative considered was raising the schedule depth to suit the slowest player. It was rejected because a player could claim a bad connection to slow the whole arena.

### The gap before the first rendered frame

A client whose whole scene is drawn at `server_now - delay` has nothing at all until its timeline has started and a frame has been played out of it, which is one render delay after the first packet at the earliest. Drawing anyway produces a wrong screen: entities at the origin, a camera on the corner of the arena and then everything arriving at once when the first frame lands.

Games that render in the past show a holding screen over that gap and fade in, because there is no real world state to show yet.

Two details. The fade is one full-screen overlay rather than an alpha threaded through every draw call, so it covers everything evenly and nobody adding a new entity type has to remember it. It sits over the world but under the panel, so the readouts that explain why the world is missing stay visible.

A related bug, found by looking at the screen: whatever the camera follows before the first packet needs a sensible default rather than `Default::default()`. Horde's fallback was the origin, which in a world measured from one corner points the camera outside it. This regressed unnoticed when the local player stopped being predicted, because the predictor had been seeded at the arena centre and the array that replaced it was zeroed.

### An entity can join a delayed timeline only if its state is reconstructable at an arbitrary past instant

Rendering remote state in the past has to apply to the whole scene rather than to one entity. Horde had three clocks in one picture (enemies at now, peers in the past, projectiles elsewhere) and every seam between them was a visible bug. Unifying them meant queueing packets on receipt and applying them when the render clock reaches them, so a frame shows one consistent instant. A token type makes any other instant impossible to express.

Doing that exposes which entities can be on the timeline at all. A peer can, because a snapshot buffer keeps its history. A projectile could not: the client held only the newest list and replaced it wholesale each packet, so once the server stopped listing a shot there was nothing left to draw it from and any shot fired and destroyed inside the render delay had never existed at the target. Measured at a 4 Hz player rate, that was every shot: none were drawn at all.

The fix was to send the inputs to the shot's behaviour instead of its output, as elsewhere in this file. A shot became an origin, a velocity and a fire time sent once as an event, so the client can evaluate it at any instant exactly. Shots drawn per frame went from 0.2, 0.1 and 0.0 at 30, 16 and 4 Hz to essentially rate-independent and it costs one message instead of an entry in every packet for the whole flight.

**The change was then reverted, while this file kept describing it for months.** The revert gave two reasons, recorded on the wire type rather than here, which is how the two came to disagree. First, a client is not told when a shot ends, so it flies on through the enemy it killed. Second, the client cannot work that out itself because it draws shots in the past while its enemy mirror holds the present. The first reason is real. The second stopped being true once [the whole scene moved to one render instant](#an-entity-can-join-a-delayed-timeline-only-if-its-state-is-reconstructable-at-an-arbitrary-past-instant) and nobody revisited it. The event form is now back, with an explicit end event carrying only early ends (a hit), because ordinary expiry is derivable from the fire time and a constant both sides already hold.

A decision recorded in two places drifts apart and the copy further from the code is the one that goes wrong. The wire type's comment was right for months while this file was wrong. Record the reasons for a revert next to the reverted code and describe a fix where the fix would be visible.

### Techniques worth reusing

Running two predictors over identical inputs that differ in exactly one variable and comparing their error tells you whether a prediction is worth having. It takes one session instead of two, needs no toggling and does not rely on remembering how the last run felt. This is only cheap because predictors are pure and hold no resources, which is a reason to keep them that way.

Read the shape of a counter rather than its value. A count climbing without bound is systematic, a count that plateaus is transient and self-healing and a regular sawtooth is a threshold effect. Each shape pointed at a different class of cause and was right each time.

## The bug catalogue

### A battle result sent in the same batch as the exit (poketo)

`poketo` ended a battle the moment it was decided, sending the final state and the op that returns the seat to the overworld together. A client applies a batch in order, so it set the finished battle and cleared it inside one loop and the win or loss was never drawn. To the player it looked like pressing a key and being dumped back outside with no explanation. From the server nothing looked wrong: every op was correct and in the right order.

**Do not send an op that says "here is what happened" together with an op that says "you are no longer here to see it"**, because the second removes the screen that would show the first. The same applies to death screens, round summaries, kick reasons, disconnect causes and any "you have left" that arrives beside the reason. There has to be a gap between them. It can be a delay, an acknowledgement or an input.

The fix that needs no structural change is to keep the state and have the client say when it is done with it. Here the decided battle stays in the collection it was already in and the client sends `Dismiss`, so the seat is still in exactly one place, the transcript is still resumable and a drop mid-result still parks.

Every reconnection and ordering test was green throughout, because every op was individually correct; the bug was that two of them arrived together. It was found by playing the game.

### Keeping the terrain rule out of the hashed files (poketo)

The resolver hashes what the ops reach, so a type near the wire moves the version whether or not it is on the wire. poketo's terrain function is a pure rule that nothing sends and it lives in its own `terrain.rs` for that reason: tuning the ground should not invalidate every connected client. See [a protocol hash over the ops misses the types they carry](#a-protocol-hash-over-the-ops-misses-the-types-they-carry-poketo) for the hash itself.

### Rapier's add_force persists until you reset it (cube_yard)

Every "energy from nowhere" symptom in `cube_yard` came from a missing call to `reset_forces`. Rapier's `add_force` and `add_torque` accumulate across timesteps until cleared, so a field applied every tick grows without bound. A roll torque capped at 4.6 rad/s reached 46, cubes were flung two hundred units clean off the floor and the player was thrown across the yard, all from the same cause.

It was expensive to find because each symptom had a plausible local explanation that was also true. Setting a velocity on a body you are standing on really is a lift. Pushing radially away from something hovering above really does drive the cube beneath it into the floor. A per-tick velocity change really is an acceleration of sixty times what you wrote. Three real bugs were fixed before this one was found and none of them explained the absurd numbers.

The magnitudes should have been read sooner. The directions and shapes looked plausible, but the magnitudes were 46 against a cap of 7.5 and positions six hundred units below a floor. A value that large usually means something is being applied repeatedly rather than that a coefficient is mistuned. The first several attempts tuned coefficients.

### Setting a velocity on a body you are standing on is a lift (cube_yard)

A magnet that gathers loose cubes was written the obvious way: cubes within reach get the player's velocity, so they ride along. Jump with it on and the player rises for ever, reaching 58 units in a yard 24 across.

There were two independent faults and the second only became visible because of the first.

The magnet was a positive feedback loop. Cubes underneath inherit the player's upward velocity, push the player higher through contact and next tick copy the new higher velocity. Setting a velocity on a body that is supporting you always does this. It was also free energy: pulling a pile toward you with no reaction on you creates momentum from nothing. Both go away with a damped spring applied as an impulse, **with the equal and opposite impulse applied back to the player**, which also makes carrying cubes feel heavy.

That fix alone did not stop the flying, which exposed the second fault: `grounded` was `vertical speed is small` and vertical speed is small **at the apex of every jump**. Holding the key therefore launched again at the top of each arc. The magnet had only made this obvious by keeping the player's vertical speed near zero more often. The fix is to ask the narrow phase what the player is actually touching and to jump on the rising edge rather than while held.

The narrow-phase version then failed the same way, for a second reason. It asks whether anything is touching below the player's centre and a cube stuck to the **underside** is exactly that, so a gathered clump became its own launchpad and jump could be held down for ever.

A cheap proxy for a physical question ("am I on the ground") works until something else satisfies it for the wrong reason. The thing that exposes it will look like an unrelated feature. That happened twice here and the second time it was the replacement that broke.

### A Hermite spline measured on one cube (cube_yard)

A Hermite spline through two snapshots, leaving along the velocity recorded at each, beat straight-line interpolation by 484x on a smooth curve and by 2.1x on a falling cube pulled out of the solver. Both numbers went into the docs. Across 500 cubes from the same scene it is **39x worse** than the straight line it replaced.

The cause: a chord cannot leave the segment between its two samples and a spline can. Measured here it left that segment on **5% of frames, by up to 2.53 units** on cubes one unit across. The velocity at a sample assumes a smooth path to the next one and a collision breaks that assumption after the packet has already gone. On a smooth path the spline is near-exact; in a pile of colliding bodies it overshoots.

The overshoot rate was itself wrong at first. It read **50%** until the comparison was restricted to frames that two samples actually bracket: past the newest sample there is nothing to interpolate toward, so "interpolate" degenerates into "hold" and those frames were scored for both techniques. Excluding them moved the straight line's win from 3.1x to 8.2x and the spline's loss from 13x to 39x, so the degenerate frames had skewed both arms.

Two habits come out of it. Measure many entities rather than one picked index: the 2.1x figure came from one cube that happened to be falling cleanly and a second index would have contradicted it before the claim reached three documents. And prefer the bounded technique when you cannot guarantee the assumption the unbounded one rests on, just as the rendering hierarchy prefers real data to inference at every step.

The example that motivated building the spline should not use it and says so.

### A delta needs both ends naming the same baseline (cube_yard)

Moving from delta-against-last-sent to delta-against-acknowledged looks like a server-side change: keep what the client confirmed and encode against that. It also needs a client change. The server ends up measuring from a baseline several frames old while the client decodes against everything it has received since and those are different reference points, so every delta lands somewhere wrong. The lossless control caught it at 2.0 units where the correct answer is 0.001.

The fix is that a frame has to **name** the state it was measured from and both ends have to be able to reconstruct that state. Fiedler carries a 5-bit offset identifying the base packet; the same thing here is a per-cube history of `(sequence, value)` on both sides and one shared `view_at(seq)`. Both ends share one implementation because two implementations of "what did we agree on" will eventually disagree.

The measurement then showed where the cost lands. Under a budget the bytes are pinned at the ceiling, so an older baseline cannot cost bandwidth. It costs cubes per tick instead: **333 down to 87**. Measuring bandwidth would have shown no cost at all.

### A harness that modelled the wrong scenario (client_utils)

Chapter 20 recorded that a fixed-duration ease has a correction rate above which it never finishes. Checking it against the shipped `ErrorSmoother` produced a clean table showing exactly that and the table was worthless. The harness called `begin_from` every frame while the logical state advanced smoothly, which is not a correction at all. It measured an ease restarting toward a moving target instead of an ease hiding a discontinuity.

The mistake surfaced only because a second model, written for comparison, returned `inf`: `offset += drawn - logical` where `drawn = logical + offset` doubles the offset every step. Investigating that obviously broken column exposed the problem with the first.

With the harness rebuilt so a correction is an actual jump in the logical state, the chapter's claim held and gained a crossover: below a correction rate equal to the ease duration, duration-based is fine and slightly better on the mean; above it, worst error goes 2.67 to 15.00 while a rate-based ease goes 2.73 to 11.33. So the guidance was right, the API claim beside it was wrong (`new` takes a duration, not a fraction) and the first harness could have settled neither.

Two habits. Check a measurement that confirms what you expected as carefully as one that does not, because nothing reports "modelled the wrong scenario". And write a second, independent implementation of the same idea even when you only need one, because disagreement between them is the only cheap signal that either is wrong.

### Quantising both sides stopped the pile sleeping (cube_yard)

Fiedler names quantising the simulation on both sides as the critical trick of state synchronization: the server simulating at a precision it never transmits means the client is always looking at a rounded copy of a truth that has already moved on. Snapping the yard onto the wire's grid each tick took the settled pile from **901 cubes asleep to 0**.

It is a loop. A resting cube jitters by less than one quantisation step, so it is re-snapped every tick forever; writing a body's position marks it modified; a body that is modified every tick never reaches the solver's sleep threshold. The obvious guard, skipping bodies that are `is_sleeping()`, does not help, because they never get to sleep in the first place.

Snapping only bodies that are moving breaks the loop: a body that is not moving is not drifting, so there is no divergence for snapping to prevent. With that, the pile settles to 901 asleep exactly as it does untouched and the two runs end 0.009 units apart on average.

A technique that perturbs state to keep two machines agreeing can defeat an optimisation that depends on state holding still. Here the optimisation was worth more (a sleeping cube skips its velocity entirely). Check what a correction costs the bodies that were not wrong.

### What a tile world saves (poketo)

A tile world was expected to be a large bandwidth win over a continuous one. Measured, a trainer is **36 bits against 51** for the quantised position every other example in this tree actually sends: 1.4x, which is modest. The 2.9x figure people quote comes from comparing against two raw `f32`s and an angle and nothing here sends those.

The real gain is that a tile is an **index rather than a measurement**. It has no bounds to outgrow, needs no quantiser or precision choice and two machines can compare positions with `==`. cube_yard shipped a bug that cannot exist in a tile world, by widening its floor past the range its quantiser covered and freezing everything that wandered out.

Two more size predictions in the same example were wrong. Ten times the view radius costs **24.5x** the people rather than the 100x its area implies, which is still far from free. Arriving in a populated zone costs **1.00x** an ordinary frame, so there is no join protocol to write: a tile world's steady state already describes everything in view, where cube_yard needed a whole-world dump because a budgeted client would take seconds to learn the yard packet by packet.

When a representation change looks like it should save bytes, measure it against **what you actually send today** rather than against the naive version. Then look at what it lets you delete.

### Repeated state and one-off transcripts (poketo)

poketo holds a real-time overworld and a turn-based battle and the two are sent differently. The overworld goes out **every tick**, because a trainer nobody describes stops moving on screen. A battle goes out **only when something happens**, because nothing in it decays: a client with the latest one is completely up to date however long ago it arrived.

That also explains why a battle needs no prediction, interpolation or relevance and why all of its difficulty is in delivery: an event that is never repeated is lost for good if it is dropped. Two small things handle that, neither of them a mechanism. A choice **names the turn it is for**, so a resend after a dropped connection names a turn that has resolved and is ignored, which covers ordering, deduplication and late arrival in one field. And a **token issued on seating** links a reconnecting client to what it was doing, because a new connection is a new id and nothing in plaza's session layer carries identity across a drop.

Switching between the two moves a seat from one collection to the other rather than setting a flag on the player. A boolean would leave a body standing in the world while its owner is elsewhere and every rule would have to remember to check it.

### Testing server and client together (spacemo)

Every test in spacemo drove one side and the bug that got past all of them was between the two: the server was correct, the client was correct about everything it was told and neither handled **silence**. Missiles are streamed while they exist and simply stop being sent when they end, so a client that never treated absence as an ending kept every one it had ever seen.

The test that catches it runs the logic and a client together for three thousand ticks and compares what the client is **left holding** against what exists. It found the bug it was written for and a second one in the same minute: straight shots that hit something kept flying on the client for the rest of their nominal life, because silence ended a missile and not a bolt. The client held 1513 shots against 235 in the world.

Two notes on writing it. The first version kept **a second copy of what the client does** inside the test meant to catch it, which is the two-derivations problem this file keeps describing: it would have stayed green while `NetClient` drifted away from it. A scripted socket carrying the server's own ops into the real decode path costs nothing and removes the copy. And the ship half originally asserted that the server went quiet, which was never broken; the assertion that matters is that the client lets go.

When two pieces are each correct and the bug is in something neither owns, no test of either piece can see it. Check what one side is left holding rather than whether each side did its job.

### Frozen missiles after bolts were sent once (spacemo)

Sending a straight shot once and letting the client carry it forward is worth 17.3x and it had an unplanned side effect: the shot now ends **by itself**, because the client counts its life down. A homing shot cannot be treated that way, so it is streamed every frame and nothing on the client counts down for it. Nothing announces its end either. It hits, expires or loses its target and simply stops appearing in the frame.

The result was a volume filling with frozen missiles, each drawn for ever at the last place it was seen. The person playing it found it while I was chasing an unrelated number on the panel. The report was "the missiles are frozen" and that was exactly the problem.

The client needed the rule it already had for ships: treat silence as an ending. For something streamed every frame, six quiet frames end it.

Two lessons. An optimisation applied to half a set changes the lifecycle of that half and the untouched half keeps an assumption that now holds nowhere else. And a despawn that works looks the same as one that never fires unless something counts it, which is why the panel reports how many went quiet.

There was a matching leak on the server, guessed correctly from the same symptom: a shot that **expired** freed its allocator slot and a shot that **hit** did not, so the index space climbed for as long as anyone was fighting, toward an id field of twenty bits.

### Sending a straight shot's spawn instead of its path (spacemo)

The two projectiles differ by one field. A bolt flies straight, so its whole future follows from where it started and how fast; a missile turns after its target, so its path depends on where that target goes next and nobody knows that at launch.

Streaming both costs **20.4 shots a frame against 1.2**, a **17.3x** difference made up entirely of paths already implied by their spawn. Telling a client once and letting it carry the shot forward is not prediction in the reconciliation sense, so it is safe: there is nothing to be wrong about and nothing to correct. The homing half cannot be treated that way. Having both on a dial lets the difference be measured.

Two details it forced. A shot needs its remaining life on the wire so that a client told once learns when to stop drawing it. And the per-client record of what has already been announced has to be pruned against the live set so that a **reused slot** is not mistaken for the shot that vacated it.

Before compressing what you send, ask which parts the receiver could work out for itself. Usually that is the parts that never change direction and those are often the most numerous.

### A comparison can pass with one arm missing (spacemo)

The measurement above first read **83x**, which was wrong in a way the test could not see: the scene lined every ship up across the nose, so nothing was ever inside anyone's lock cone, **no missile ever launched** and the streamed side was being compared against an empty homing side rather than against a cheaper one.

With the ships strung out along the axis they actually look down, it is 17.3x. The test now asserts that homing shots were in flight.

The ratio assertion in the relative-encoding entry below is the same kind of bug: a comparison that is arithmetically fine while one of the two things being compared is absent or degenerate. Neither produced an error. When a test contrasts two cases, assert that both cases actually occurred.

### A 2D grid in a 3D volume over-sends (spacemo)

`SpatialGrid` is two-dimensional and every 3D thing built here before spacemo could ignore that, because a yard has a floor and an arena has a plane. The expected failure in a volume was ships going unseen. The actual failure was the opposite: a grid on `(x, z)` returns everything in the **disc**, which is a superset of the sphere, so nothing is ever missed.

That is why it gets past review. Interest management that errs in this direction looks correct from inside the game while spending the bandwidth it was built to save: measured, **51.7 KiB/s against 7.3, a 7.1x over-send**, per client, at 60Hz.

The fix is a height filter on what the query returned. It is exact at identical query cost: the same cells touched, the same candidates examined and a cheaper test per candidate. A real 3D grid could then only win on query cost, where it trades 3x fewer distance tests for 2.5x more cell lookups. So the example recommends the one-line filter and `encode_3d` in `relevance.rs` stays unused.

The control matters as much: at slab thickness the flat grid costs **1.00x**, degrading smoothly as the world gets thicker, so it is right for the flat worlds it was built for. When a structure errs in the cheap direction no symptom reports it, so build the measurement on purpose and pair it with a scene where the structure is right.

### The cost of churn (spacemo)

Every earlier measurement in the tree is steady state: N bodies updating every tick, with every optimisation aimed at freshness. Transient entities are different. A bolt lives about a second, so its cost lands on entry and exit rather than on updates.

Eight ships in one fight: **7.8 ships at 116 bytes a frame against 31.4 bolts at 410**, so bolts are 78% of the packet. A bolt is individually cheaper than a ship, 13.0 bytes against 14.9, but bolts cost 3.5x more in total because there are four times as many.

Two things follow. Give transients no field they can derive: a bolt carries no orientation, because it points where it is going and the client already has the velocity. And an id has to survive slot reuse. Otherwise a client keying on a dense index blends a new entity into the flight path of the one that just vacated the slot, which is what `SlotKey`'s generation is for and the first time anything here has needed it.

A budget built around long-lived entities misses the short-lived ones, which are most numerous in the busiest moments of play.

### A ratio hides which curve is higher (spacemo)

Positions encoded relative to the observer should make error independent of world size, since relevance already bounds every offset by the view radius. The first version quantised the **anchor** over the world, which put the world's size straight back into the error and relative came out very slightly *worse* than absolute at every size.

The test passed. It compared growth **ratios** and relative started higher and grew more slowly, so the check went green while the scheme was strictly worse than the one it replaced. A ratio of growth says nothing about which curve is higher.

Sending the anchor at full width fixes it and 96 bits once a frame amortises to nothing across the entities in it: error is then **0.0254u whether the world is 400 units across or 400000**, against absolute's 12.2 at the far end. The assertion now demands the two figures be *identical* rather than one growing more slowly.

If a comparison normalises away the quantity you care about, it can pass while measuring the wrong thing. When the claim is about a value, assert on the value.

### A bound sized for one way into the frame (spacemo)

Relative encoding bounds an offset by the view radius, on the premise that relevance is the only way into a frame: a frame only carries what the radius reached, so the radius is the widest an offset can be. The premise held until a held weapon lock became a subscription. That is a second way into a frame and a locked ship can be anywhere in the volume. Its offset clamped at the view bound and the ship drew a hundred units from where it flew, under exactly one dial pair, packed and relative together.

No local test could see it. The encoder clamped as designed, the decoder decoded what it was given and the two agreed with each other; only the mirror test that scores the client against the server's own state caught it. That test has now caught two faults that neither half owned. The fix is one bit per ship saying which arm carried it, an offset inside the radius or the absolute bounds past it.

A bound is sized to an invariant that is maintained somewhere else. Whoever adds a new way into the audience has no reason to know the wire sized a field to the old one, so the clamp fails without an error. When a bound assumes "this is the only way in", write the assumption next to the ways in or in a test that drives both ends.

### Yaw and pitch against the wire quaternion (spacemo)

The simulation reasons in yaw and pitch because a flight model does. The wire carries a quaternion because smallest-three is 29 bits against 64. Nothing forces them to agree and they did not: a positive rotation about X takes +Z toward -Y while the flight model calls positive pitch nose up.

**Every position was correct throughout.** Ships would have flown exactly where the server put them but rendered pitched the wrong way, so once a renderer existed the flight model would have looked broken. Positional, packing and relevance tests cannot see it.

What catches it is rotating the forward vector by the wire quaternion and comparing against the simulation's own `facing()`, with the rotation implemented the long way so it shares no code with what it checks. Wherever one fact has two representations, test the conversion between them without using the converter.

### Driving the player's velocity directly (cube_yard)

"Make the roll physical" was taken literally: the player cube was driven by a torque, with friction turning spin into travel, so mass and momentum were real. It was the wrong call and it cost a long sequence of fixes, each of which found a genuine bug that was not the problem.

A torque can only become travel through grip, so friction decides everything. Raise it enough to stop the cube spinning on the spot and it measured **1059N of static friction against a 950N motor**, so the cube simply stopped. Lower it and the cube span without going anywhere. A gathered ball added drag until the player was down to 0.4 units per second, which reads as stuck. Every coefficient tuned moved the problem somewhere else, because the system had no good operating point to tune toward.

A player who presses a key expects to move and expects to stop, whatever the physics would do. Driving the horizontal velocity directly and reading the **roll off the resulting velocity** made the cube behave, made the spin always match the travel and removed the dependency on grip entirely, which is why friction could then drop to almost nothing. Gravity, jumping, collisions and the entire field stayed physical.

Use the solver for the parts of the world nobody is steering. If it drives what the player controls, you end up tuning it for results the player should get directly. The weight you wanted can be a coefficient on the drive instead.

### A filter set on the wrong field is silently inert (cube_yard)

Carried cubes were supposed to stop pushing the player: solid ones make it climb its own ball and the fix was to filter the pair out of the solver. The player collider got `collision_groups(PLAYER_GROUP)`, the carried cube got a solver filter excluding that group and the tests that followed went green.

Nothing was being filtered. `collision_groups` and `solver_groups` are **separate fields** and `solver_groups` defaults to `Group::ALL`. Both sides of a pair have to agree, so the player's default membership of everything matched the cube's filter no matter what that filter said. Every carried cube stayed fully solid for the whole sequence of fixes that followed and each of those fixes was credited with an improvement it had not caused.

Printing the player's contacts while chasing an unrelated bug exposed it: the player was resting at 2.495 on four cubes it was supposed to be passing through, with no floor contact at all. The levitation, the infinite jump and the wedging all had this one cause.

A filter that is not applied looks the same as one that passes everything and a green suite cannot tell them apart. When a mechanism is meant to stop something, assert that the thing stopped rather than that the symptom improved.

### Per-island sleep against per-body rest (cube_yard)

`at_rest` on the wire came straight from `body.is_sleeping()`, which the guide said was all a rest detector needs. A screenshot showed it was not: patches of a hundred-odd cubes drawn as awake, lying flat on the ground with nothing near them.

A solver sleeps an **island**, which is every body in a chain of contacts. One cube still jostling in a scattered heap holds every cube touching it awake and each of those pays a velocity on the wire to hold still. The property the wire cares about is per body and purely local: has *this* body moved recently.

A run of quiet ticks per body, which is what `RestDetector` already models, decoupled the two: 205 cubes reporting awake became 56, against 57 that had actually moved.

A solver's flag is only reusable when its granularity matches yours. Islands suit skipping integration work, but deciding what to transmit needs a per-body answer.

### Waking cubes the field cannot move (cube_yard)

The repulsion field fades to nothing at its rim, but woke every cube inside the radius before deciding how hard to push. Below about eight units the push cannot overcome the cube's own friction, so the outer band was woken, could not move and trailed each player as a halo of awake cubes paying for velocities that were all zero.

Gating the wake did nothing, because a cube woken while the player was close keeps receiving the weak push as the player recedes, so it never gets the run of quiet ticks it needs to sleep again. The condition has to be **motion** rather than sleep state. A cube already moving still receives the weak push, so the field itself has no cliff in it; a still one below the threshold receives nothing.

A force too small to move anything still costs bandwidth, because it keeps the bodies it touches from sleeping.

### The envelope re-encoded the packed bytes (cube_yard, wire)

A hand-packed payload went from 51877 bytes to 10396 and then travelled in **15502**. A `Vec<u8>` field reaches the outer codec through `serialize_seq`, so every byte is re-encoded as its own integer and MessagePack spends two on anything above 127. Declared as *bytes*, the same payload travels in 10411: a fifteen-byte header over the raw layout.

The bits were counted carefully in one function and then inflated by a field declaration two files away. `plaza_wire::Payload` makes the bytes declaration a type, so nobody has to remember it.

### A budget planned with a guessed cost (cube_yard)

The priority accumulator fills a byte budget using a cost function the caller supplies. The first one was written by reading the layout and estimating: 12 bytes for a moving cube, 8 for a sleeping one. The packets came out at **638 bytes against a 533 byte budget**, 20% over the ceiling the whole stage exists to hold.

The fix was to derive the cost from the layout itself (`pack::cube_bits`, a `const fn` over the same widths `write_cube` uses), so changing a field's precision moves the budget with it. A constant written down beside the thing it describes goes stale the first time the thing changes.

The same problem appeared twice more in the same example: a bandwidth meter that only pruned its window when a packet arrived, so a link that went quiet kept quoting its old rate and a test asserting delta-coded indices reward locality that was really measuring how many sleeping cubes each index set happened to select.

### The pulse ring that fired several times per pulse (horde)

**Symptom.** The nova's expanding ring visibly restarted two or three times per pulse, a fraction of a second apart. Reported by a player as "the animation seems to activate multiple times, but dunno if it is just part of the animation". It was not the animation.

**Cause.** The ring is *inferred*: a burst of ten or more deaths in one tick reads as a pulse. Ack-based recovery deliberately repeats an announcement until the acknowledgement for it comes back, which takes a round trip and the entity stream sends every 62 ms, so one nova's death batch arrives two or three times. The mirror absorbs the repeats idempotently, by design and documented ("applying a superset is harmless"). The death *counter* did not: it incremented per announcement, on the wire, so every repeated batch was a fresh ten-plus-death tick and the ring re-fired.

**Fix.** Two stages. First, count the removal rather than the announcement: a death increments the counter only when the mirror actually held the entity and removed it, the same gate the explosion effect already used, so a repeat does nothing. Then the inference was deleted: the packet now carries the pulse's server timestamp and the ring is a pure function of that instant and the frame clock. Nothing triggers or decays, a repeat draws the same ring and a mid-pulse joiner draws the rest of the ring it walked in on, which the inference could not do. This is the same change as the projectile fix: send the event instead of something the client has to infer the event from.

**The general form.** In a protocol that repeats until acknowledged, the number of times you were told something differs from the number of times it happened. Anything derived from such a stream must be as idempotent as the stream: count state transitions, not messages. The mirror did this from the start; the one counter that read the wire instead of the state produced the visible bug.

### A backgrounded tab could not come back (horde)

**Symptom.** Switch away from the browser client for a while, come back and the world does not resume. It never resyncs and the longer it was away the worse it is.

**Cause.** It was not the resync. The client's clock was built by accumulating frame time with a cap: `now_ms += min(frame_time, 100)`. The cap is right for deciding how much *simulation* a frame runs and wrong for a clock. A browser stops running frames for a hidden tab, so a client that adds up the frames it happened to run believes less time passed than did, **permanently**. Its estimate of server time is then wrong by however long it was away, nothing arriving is ever due and the playout queue grows without bound because the clock that drains it had stopped.

**The recovery that existed could not fire.** A client that falls behind is supposed to be rescued by the server: its acknowledged baseline ages out of history and the next packet is a full rebuild. But that only happens once the client resumes applying and acknowledging and a client whose clock is wrong keeps acknowledging old sequences, so the server reads it as slow rather than lost.

**Fix, in three parts.** The clock comes from real time and the cap applies only to the simulation step, which is the actual bug and the one that would have prevented the rest. The queue is bounded, because a buffer fed by a peer and drained by a local clock has to be. And a client that finds itself far past the instant it is drawing treats it as a **discontinuity**: drop the queue, drop the mirror, re-anchor on what just arrived and let the server's next digest check rebuild the world. Counted as `timeline restarts` in the panel, beside the underruns and view fallbacks.

**The general form.** "How much time has passed" and "how much work may I do about it" are different questions and a frame loop that answers both with one number will lose time whenever it is throttled. Cap the simulation work per frame and leave the clock uncapped. The discontinuity rule for position applies to time too: there are no intermediate states between a minute ago and now to ease through, so snap.

### The recovery that was worse than the stall (horde)

**Symptom.** With the timeline restart in place and its unit tests green, a real backgrounded tab was tried and the recovery was shit: a freeze of several seconds on refocus, then a stuttering settle.

**Why the tests missed it.** They tested the *decision* (snap rather than crawl) by feeding packets one at a time. What they never modelled was the *delivery*: the browser's socket keeps receiving while the frame loop is stopped, so the entire stall arrives as one pre-buffered lump that must be handed over and the server had spent the whole stall escalating. Three mechanisms stacked:

1. **The server kept sending full baselines to a client that was not reading.** A silent seat's acknowledged baseline ages out of the 24-packet history in 1.5 s, after which every plan is the full visible set: roughly 25 KB of JSON, 16 times a second, tens of megabytes a minute, none of which the client would ever play.
2. **All of it was parsed in one frame.** The JS-side queue is unbounded (correctly: it is a pipe) and the first poll back drained and `serde_json`-parsed the lot before the frame could render. That is the freeze.
3. **The restart fired on the wrong trigger, repeatedly.** The render clock snaps to the present on the first drained packet, so the 3 s rule never saw the backlog as "ahead"; only the 256-packet queue bound tripped, once per 256 packets, tearing down each partial rebuild the previous trip had paid for. Meanwhile every backlog packet was counted as an underrun, so one stall read as a thousand link faults.

**Fix, at each layer.** The server throttles a seat that has stopped acknowledging (3 s, the same threshold as the client's own discontinuity rule) to about one packet a second, which is enough of a keepalive for a resumed client to find the stream again at a thousandth of the cost. The client drops a resume backlog **before parsing it**, on message lengths alone, keeping only the tail and restarts the timeline once, deliberately; the drop is reported in the panel ("resume drops") and the host panel shows "seats throttled for silence". And underruns only count lateness on the scale jitter produces; past the discontinuity threshold the packet belongs to a lost timeline, which `resyncs` already accounts for.

**The general form.** Flow control is part of reliability. A protocol that keeps sending to a reader that has stopped reading piles the cost onto the reader, which pays it on resume. And a unit test that feeds a component its input one piece at a time has not tested arrival. Put the transport's actual failure shape (a burst, a lump, a reorder) in a test. Otherwise the user will be the first to exercise it.

**Where it lives now.** The whole recovery became framework blocks and horde was retrofitted onto them: `DeltaBaseline::with_flow` (the server-side throttle), `client_utils::PlayoutBuffer` (the bounded queue, the discontinuity rule and both counting rules), `plaza_ws::trim_backlog` (the drop-before-parse) and the resume contract written into the docs of `client_utils` and `server_utils::delta`: a client may discard any stretch of the stream unread provided it also drops its mirror, because an acknowledgement carrying the digest of nothing obligates a full baseline. The same extraction pass took the input schedule (`server_utils::InputSchedule`), the tier hysteresis (`relevance::TierBoundary`) and the arrival measurement (`client_utils::ArrivalMonitor`), which also gave the joiner's panel a measured render-delay budget the host-side sliders could never provide.

### Iffy movement and a churning resume, hunted with a probe (horde)

**Symptom.** After the extraction: "recovery sucks now and movement is iffy". The natural suspect was the extraction, seven commits of it.

**The A/B came first and cleared the suspect.** A probe (`examples/recovery_probe.rs`: one real client against the real server over the simulated link, printing marker continuity and stall recovery as numbers) run at the last praised commit and at HEAD produced **bit-identical output**. Both symptoms predated the extraction; the refactor had kept the bugs along with the features.

**The probe then gave one misleading reading.** It reported the moving marker holding still on 34.6% of frames at every render delay, which read as a structural stutter and produced a wrong theory (the clock's full-strength resync fighting the tick's advance) and a wrong fix, reverted when three tests explained that the clock *tracking arrivals at full strength is the contract* and that `recv` is a smooth estimate anyway. The real cause of the 34.6%: the probe steered its player in a straight line into the arena wall and then measured a player standing against the wall. A movement measurement has to check that the thing measured is moving.

**What was actually wrong.**

1. **Movement: the exact-fit budget.** The defaults shipped as `30 one way + 20 jitter + 100 interval = 150` render delay, margin zero, so every jitter spike at the tail of the distribution put the newest sample behind the interpolation target and the marker held then snapped: worst frame-to-frame jump 8.3 px against a normal step of ~3. At 180 ms the snaps vanish (worst 3.2 px). The default is now 180 and the joiner's measured-budget line exists to catch this for players without access to the sliders.
2. **Resume: a pinned baseline.** After a resume, the client's acknowledgement window spans the silence and the keepalives inside the silence are sparse in the sequence space (the sequence advances for every seat's round), so `contiguous_base` could never cross the holes: the acknowledged baseline pinned at the first keepalive, the server diffed against a state as old as the stall and staleness had to fire a second time to clear it. Measured as ~25 consecutive full baselines over 1.5 s. Two fixes in `DeltaBaseline`: a rebuild clears the sent history (a stale in-flight ack must not resurrect a pre-rebuild baseline) and an ack arriving from a seat flow control knows is stalled is treated as **the resume signal**, opening a fresh epoch: one full baseline, acknowledged contiguously, deltas one round trip later. Measured after: 4 full baselines, all in the first second.

**The general form.** Compare revisions before theorising from the diff: a bit-identical A/B settles more than any amount of reading hunks. And a probe needs the same skepticism as any counter. The two readings that held up (worst-jump, full-baseline count) were checked against a mechanism. Nobody asked what else could produce the one that misled (holds).

### The second-consumer pass and the retrofit that was refused (all three)

Before calling the blocks shippable, each needed a second game and the pass produced one deliberate refusal alongside the adoptions. `ArrivalMonitor` took over netcode's adaptive buffer (which had been sizing itself from *ping* jitter, a proxy that diverges from snapshot-arrival spread exactly when the buffer matters) and `TierBoundary` took over blackhole's per-pellet correction membership (aligning the leave radius with the pellet draw cutoff, so everything on screen keeps its corrections). But `InputSchedule` was **not** forced onto netcode: its commands carry a sequence number and no tick, because apply-on-arrival with a sequence frontier is that demo's model and bolting tick addressing onto it would change the demo's behaviour rather than just its code. The block's docs now carry the two-model table so the choice is made deliberately. Also found in the pass: netcode's input inbox was unbounded (now capped, oldest dropped) and blackhole **cannot** adopt the resume kit at all, because its removals are order-sensitive events with no digest contract, so discarding any stretch of its stream unread is unsafe. That is a cost of the apply-on-arrival architecture and it is now stated where the two architectures are contrasted.

A 128-seat soak (`examples/soak.rs`: ten simulated minutes, five hidden-tab cycles) closed the pass: modelled bandwidth flat at ~2.47 MB/s, about two full rebuilds per stall cycle, no counter trending. It also showed the layers combining in a way nobody planned: the server-side throttle keeps a 12 s backlog under the client's trim trigger, so mid-length stalls recover by ordered replay without a timeline restart at all.

### A bandwidth meter that reported a session mean (horde)

**Symptom.** A player reported, repeatedly, that bandwidth crept upward the longer a session ran and never settled: standing still, moving about, at 128 players and again at 10. Two screenshots three minutes apart showed 127.6 then 143.9 KiB/s while the live enemy count *fell* from 2311 to 1751.

**Three wrong explanations, each defended with a measurement.** That the horde was collapsing and refilling (true earlier, fixed and not this). That it tracked the live enemy count (refuted by the screenshots above: bandwidth up, population down). That it was the nova cycle oscillating (real, but an oscillation is not a trend). Each was checked against a harness that measured **per window** and therefore could not reproduce the symptom at all, which should itself have been the clue.

**Cause.** `RateMeter::per_sec` was `total / elapsed` over the meter's whole life. That is a session mean rather than a rate. A session mean approaching a steady state it has not reached converges **asymptotically**: it climbs by less and less, but keeps climbing for as long as the session runs. Every reading is arithmetically correct and the number goes up every time you look. Anything that raised traffic even briefly, a burst of spawns, a walk through a dense corner, raised the mean permanently, because the denominator only ever grows.

**It also made the panel useless for its stated purpose.** A slider you have just moved is one second of evidence against twenty minutes of history, so the readout barely moves, in a panel meant to show what the sliders do.

**Fix.** The meter keeps a rolling window (sixteen buckets of 500 ms) and reports the recent rate; the lifetime figure is still available as `lifetime_per_sec` for summaries, where a session mean is the right answer. One detail: the newest bucket is normally only part filled, so the divisor is the span the retained buckets actually cover rather than the nominal window, which would otherwise understate the rate by a steady few percent.

**The general form.** A measurement instrument is part of the system under test. This file has four entries where the instrument was the defect: a harness with no acknowledgement loop, a counterfactual that modelled only some fields, a fault check that compared a delayed client against the present and now a rate that was a mean. In all four the number was *plausible*, so it was read as evidence about the game instead of being checked as output of the instrument. When a measurement disagrees with a player watching the thing directly, suspect the measurement.

### A fault readout that was really the render delay, twice (horde)

**Symptom.** After the acknowledgement fix above, `entities held that are dead on the server` went from 15 to 156 and turned red, while every other number improved: churn balanced, the packet became 4% new arrivals instead of 99%, the worst server tick fell from 185 ms to 6 ms.

**Cause.** The check compared what the client draws **at its render instant** against server truth **now**. A client that renders 250 ms in the past is holding every entity that has died since that moment, by construction. At roughly a thousand kills a second, that is a couple of hundred entities and the number got *worse* because the client was no longer being sent the present and was correctly on its own timeline again.

**Fix.** The server keeps a bounded log of recent deaths with their times, capped at the deepest render delay the panel allows and a held entity counts as a phantom only if it was already dead **at the instant being drawn**.

**This is the same mistake as `mean_render_error`**, which is already filed as open work and it went unnoticed in a second metric for as long as nothing exercised it. Any comparison between a delayed client and an authoritative server needs the server's state *at the client's instant* and a server that keeps no history cannot provide that: the alternatives are keeping one (a bounded log here, `HistoricalStateBuffer` in general) or not reporting the comparison.

### The 127 clients that never acknowledged (horde)

**Symptom.** A host running 128 players reported bandwidth climbing slowly over half an hour and `churn: 136.9 spawns / 0.2 despawns per packet` against `sent per packet: 138 entities`. Almost every entity in every packet was arriving as a brand new spawn and almost nothing was ever retracted.

**A harness disagreed.** A harness driving the same arena for twenty simulated minutes, to maximum difficulty, showed bandwidth flat to 2.5% and roughly 17 spawns per packet. The difference was that every client in the harness **acknowledged**, while 127 of the 128 seats in the real arena had nobody to acknowledge for them.

**Cause.** Seats with no connection still have packets built for them, because an empty seat drifts as a bot and is still simulated. Under ack recovery a baseline advances only on acknowledgement, so those seats sat at an empty baseline for ever and every packet built for them was a **full dump of the whole visible set**. Nothing was sent (there is no connection), but the readouts count every packet built, so a full arena was being charged for a defect none of its actual clients had and the spawn count was eight times the truth.

**Fix.** The arena acknowledges on behalf of a seat nobody is connected to. Its client is the server itself, holding exactly what it was sent over a wire that cannot drop anything, so the acknowledgement is accurate.

**The slow climb was the horde refilling.** A separate fix had just raised the wave cap so 128 players could no longer annihilate the population, so the arena spent several minutes growing from a few dozen enemies to three thousand and the per-view cost grew with it. It plateaus.

**The general form.** This is the third time in this file: an unacknowledged delta stream reports full re-sends for ever and the numbers look plausible. It was found in a ghost measurement, then in a bandwidth harness and now in the shipped example. The first two were fixed by writing the lesson down and later by an assertion in the harness; neither of those could reach this one, because here the missing acknowledgement was a property of the *arena* rather than of a test. A readout comparing spawns per packet against entities per packet would have caught it. The panel already showed both numbers and nobody had read them as a ratio.

### An almost empty arena and a negative saving (horde)

**Symptom.** Two at once, both reported by a player at 128 players: bandwidth appearing to climb the longer a session ran and the "saved against UUIDs and `f32`" readout going **negative** for the first time, at -17%.

**The negative saving came from a broken comparison.** `naive_bytes` modelled only the entity lists, while `bytes` counted coins, wallets, hit markers, the digest and the sequence number as well. That was invisible for as long as entities dominated the packet. Once they did not, the ratio compared a full packet against a partial model of itself. Moving player state onto the player stream made it worse, because the real cost still counted that stream while the counterfactual had lost its only player term. The fix is that the baseline models **every field the packet carries**, so it measures the encoding rather than which fields somebody remembered.

**The entities did not dominate because the horde was nearly gone.** The wave spawner filled at most 40 enemies per 500 ms whatever the player count, while the kill rate scales with players: each one carries an auto-firing weapon and a nova that clears a radius every 4.5 seconds. At 128 players that held a 3000-strong horde at **about 40 alive**. Every entity number was therefore a measurement of an almost empty arena, including the ones quoted as evidence that the relevance work had succeeded: 607 KiB/s at 128 players was really 2.1 MiB/s once the world existed.

**Bandwidth was not climbing at all.** Measured in fifteen-second windows it is flat to within 5% across two minutes. What a player sees climbing is a rolling average filling up and the arena repopulating after each nova.

**The general form.** A number from a system in a degenerate state describes the degenerate state. A harness with no acknowledgement loop reported zero samples; a nearly empty arena reports small packets and a negative saving. Both look plausible. In both cases, assert the precondition and put the population on screen next to the bandwidth so nobody reads one without the other.

### The marker that detached from its own timeline (horde)

**Symptom.** At 600 ms render delay, shots visibly left from "a point in the past", well behind the player's marker. Reported by a player; every readout said healthy. It looked like the shots were mis-timed, but the shots, enemies and coins were all correctly at the render instant. The *marker* was the thing off the timeline.

**Cause.** The player history buffer held a constant 8 snapshots, roughly 200 ms at the default send rates, while the render delay slider went to 600. Past what the history covered, the snapshot buffer *clamps to the oldest sample it still holds*, so the marker rode a couple of hundred milliseconds behind now while the scene was drawn at T and the gap scaled with the slider. At the default 150 ms delay the 8 snapshots happened to cover it, which is why it looked fine for as long as it did.

**Why nothing caught it.** The clamp returns `Some`, so the off-timeline counter that exists for exactly this class of failure never fired: the clamp was one layer below the instrumented layer, so it hid the fault from the counter built to catch it.

**Fix.** Two parts, both principles this file already states. The buffer capacity is now derived from the thing it must cover, `RENDER_DELAY_MAX_MS` at the maximum rate of both streams and the slider range is derived from the same constant, so the two cannot disagree again. And the clamp is counted: `RemoteView` exposes the oldest instant it can still reach and a render target older than that increments the same `view fallbacks` readout as an empty view.

**The general form.** A buffer sized by a constant is an accidental limit. Like the input schedule's ceiling above, it degrades a player silently instead of refusing them. Any buffer that serves a *declared* number (a render delay, a playout depth) must derive its capacity from that number's maximum. Otherwise the declared number is false beyond what the constant covers.

### A client mirror that diverged and could never recover (horde)

**Symptom.** The digest mismatch counter climbed to a few hundred within a minute and then stopped, always around the same number. Turning latency, jitter and loss to zero did not help. The offline build reported zero with the same simulation code.

**Theories that were wrong.** Packet reordering (the impairment link could reorder, but WebSocket is TCP and cannot, so the tooling had invented this failure). Frames dropped at the session queue (added a lost-frame counter, which read zero for the whole run). Frames failing to deserialise (round-tripped every frame through JSON in a test, all survived). Acknowledging frames on receipt rather than on apply (checked: acknowledgement already happens after the packet is applied).

**How it was actually found.** The digest reported that the mirror was wrong but never how. Adding a debug mode that ships the server's exact visible key set alongside the digest turned the counter into a diff and one run was enough: every missing entity was generation zero and they were spread evenly across the whole slot range. Drift would cluster. This was a client that had never been told about most of the world.

**Cause.** The arena builds packets for every seat from startup, occupied or not, because an empty seat drifts as a bot and is still simulated. By the time a real client connected, that seat's relevance baseline was already most of the world, so the joiner's first frame was a delta against a baseline it never received. Almost nothing arrived as `entered` and the world trickled in only as parts of it happened to become newly relevant.

**Why it could not self-heal.** Once the server believes a client holds an entity, that entity is only ever sent as a position sample and a client discards samples for entities it does not have. The delta stream had no path back.

**Fix.** Two parts. `Server::reset_seat` clears a seat's baseline when a fresh client takes it, so the first frame is a full dump. And the acknowledgement now carries the client's own digest, so the server can compare it against the digest of the state it believes the client reached and force a clean rebuild when they disagree. The first prevents the common case and the second recovers from any cause.

**General lesson.** Any per-subscriber delta stream needs both: a baseline reset on join and a way for the subscriber to say what it actually holds.

### Enemies lunging whenever the player moved (horde)

**Symptom.** A jump forward while playing as host, most noticeable when moving, absent when standing still.

**Cause.** The client fed its locally predicted player position into the shared enemy rule, while the server aimed enemies at its authoritative position. Every packet snapped the enemies between the two.

**Fix.** The prediction drives only the camera and the player's own marker. The shared rule reads the authoritative position, exactly as the offline build does.

**General lesson.** Prediction is a presentation concern for one entity. Feeding it into a rule both sides run creates a second world.

### A rhythmic forward tug while holding one direction (horde)

**Symptom.** A small jump forward roughly every four hundred milliseconds while moving in a straight line, at any latency including zero.

**How it was found.** A correction log recording magnitude, direction relative to the held input and the one-way estimate. The pattern was clear: corrections were almost exactly the correction threshold in size, always forward and evenly spaced.

**Cause.** The local player pulled toward the server only once the error exceeded a fixed threshold, then closed the entire gap at once. A slow systematic drift therefore became a sawtooth.

**Fix.** Ease a fixed fraction of the error every packet instead, so drift is absorbed continuously. Discontinuities beyond a much larger bound still snap, because a respawn must not be eased.

**General lesson.** Choose snapping or easing by cause rather than size: ease continuous error and snap genuine discontinuities.

### A hole that jerked constantly (black hole)

Three separate causes produced one symptom, which is why it took three rounds. The correction log made each one visible in turn once the previous was removed.

**Cause one: an unpredicted force.** The server moves a hole in three passes: input, gravitational attraction toward every other hole and collision separation. The client predicted only the first. The attraction is capped above walking speed by design, so the unpredicted term was large exactly when it mattered. Fixed by predicting the pull with the same rule the server runs, from the attractor field the client already receives.

**Cause two: predicting a frozen entity.** An eliminated hole is frozen by the server for a two and a half second respawn delay. The client kept integrating input into it and reconciling the difference every packet, generating a correction stream entirely of its own making. Visible in the log as long runs where the authoritative position never changed. Fixed by freezing the prediction while dead.

**Cause three: an unpredicted ability.** The dash was deliberately left unpredicted on the grounds that mispredicting a discrete grant is worse than mispredicting continuous movement. That reasoning predates the client having a reliable local mirror of the server's dash cooldown, which it now uses to light the burst instantly. Predicting the movement too is a toggle, so both behaviours can be compared live.

**General lesson.** Enumerate every way the server can move an entity before deciding what the client predicts. The largest error came from the term nobody listed.

### A repulsor ring that lagged and stuttered (horde)

**Symptom.** The ring trailed the player marker and stepped rather than glided, even at a high send rate.

**Cause.** The ring was drawn at the authoritative position, which only changes when a packet lands, while the marker was drawn at the prediction, which advances every frame, so the picture mixed two clocks.

**Fix.** The local player's own ring follows the prediction. Peers' rings stay on their authoritative positions, which is where their physics is.

**General lesson.** Choose which position each drawn element uses. Mixing them in one scene is visible immediately.

### A player who could not be controlled after a settings change (horde)

**Symptom.** Changing the enemy count left the local player unresponsive. Movement was predicted, so the marker moved and was then pulled back every packet.

**Cause.** Rebuilding the world preserved `clock_ms` (a fix from an earlier bug in this same file) but reset a *separate* tick counter to zero. The client kept naming ticks derived from the real clock and the server rejected every one of them as impossibly far in the future.

**Fix.** The tick is derived from the clock rather than counted alongside it.

**General lesson.** Two representations of one fact will eventually disagree and the fix for the earlier bug is what created the disagreement: preserving one of the pair across a rebuild and resetting the other is worse than resetting both. This is the same cause as the three separate key-packing functions and the two digest folds and it is the most repeated cause in this document.

### An impairment slider that did nothing on the real path (horde, then black hole)

**Symptom.** Dragging packet loss changed nothing over a socket.

**Cause.** Two faults with the same effect. The downstream impairment link took latency and jitter from the panel and a hardcoded zero for loss. The upstream had no impairment at all, so inputs, acknowledgements and purchases crossed a perfect wire however far the slider went. Loss worked in the offline single-process build, which is where every measurement in the report had been taken.

**Why it mattered.** The acknowledgement is what lets the server tell a starved mirror from a healthy one, so the whole loss-recovery mechanism was being exercised only in the one configuration where its input could never be lost. Fixing it made the late-input and rejected-input paths reachable from the panel for the first time.

**General lesson.** A toggle wired to the demo path and not the real one tests a system nobody runs. Control-plane traffic (a version handshake, a ping) is a reasonable exemption; the traffic the mechanism under test depends on is not.

### A joiner that could not move for half a minute after its tab woke up (horde)

**Symptom.** A browser tab suspended and resumed. Frames arrived, the world drew, acknowledgements flowed and the player could not move. It recovered on its own after tens of seconds. Alongside it, the joiner's measured-budget line jumped to 225 ms, climbed past 300, then decayed back.

**Two wrong fixes came first and both were reasonable.** The first: the budget readout is smoothed from declared stamps, a resume feeds it the kept tail's stall-era stamps plus one stall-sized gap, so reset the `ArrivalMonitor` on a timeline restart. It passed a scripted test that reproduced the resume and made things *worse* in the browser. The second: a pong answering a pre-stall ping measures the suspension as a round trip and poisons the clock fit, so rebuild both estimators and refuse cross-stall pongs. It was also tested and shipped and also fixed nothing. Both were reverted.

**What the captured data said.** Panel readouts added for the third attempt, read during a stuck window: acknowledgement lag 4, identical to healthy; input aim **-5 ticks** against a 4-tick accepting window; worst raw pong 1056 ms; round trip smoothed to 313 ms while the link was 21. So the inputs *were* arriving and *were* being refused, for naming ticks the server had already closed. No 40-second pong ever arrives, because pings stop while the frame loop is frozen, which is why the second fix could not have helped: the poisoning it guarded against barely happened.

**Cause.** After a resume the clock fit trails the stream: its window predates the stall, the first pongs back are delayed behind a draining backlog and it refills at one ping a second. Every input names its tick from that fit, so for as long as the fit lags by more than the late window, every input is dropped.

**Fix.** Floor the named tick at `(newest arrived stamp + playout depth) / step`. The server *wrote* that stamp, so server time is provably past it and the floor needs no clock at all; it only ever lifts the aim and never past where a perfect clock would have aimed, because the stamp trails true server time by the one-way delay. When the fit is healthy its estimate exceeds the floor and nothing changes.

**General lesson.** A misbehaving readout is usually downstream of the broken quantity. Both wrong fixes repaired *measurements* of the clock while the clock itself kept naming closed ticks; the second even reset the thing whose lag was the actual fault and then let it lag again. **Where a bound can be derived from something the peer stated, prefer it to an estimate.** An estimate is wrong exactly when it is under stress, while a stamp the server wrote holds at any clock skew.

**A blind spot.** The client's acknowledgement lag *cannot* see this failure: the server acknowledges an input on arrival, before admission, so refused inputs are acknowledged exactly like accepted ones. The verdict exists only on the server, which is why `InputSchedule` now reports rejections split by side with the last margin in ticks and why the host panel shows them per seat.

### Four bugs with one shape (bomb grid)

The lattice example was written in a day and then debugged from four screenshots. The four faults turned out to share one cause, described after them.

**The symptoms, in the order they were reported.** A player who kept walking after the key was released. A player who could not stop during the interval after winning a round. A player who snapped back constantly while running across open ground. And a residual two snaps per hundred frames that survived all three fixes.

**Three of the four looked exactly like network faults** and none of them was. The panel showed corrections and a player not where the server put them, which is what a network fault looks like. Each was the client and the server running the same rule on different clocks.

**1. Inputs trimmed on acknowledgement.** An input names the tick `press + playout` and the server acknowledges it on **arrival**, which on a fast link is a hundred milliseconds before that tick. The pending list was being trimmed on the sequence number alone, so the release you pressed was discarded in flight and never ran locally: `held` kept the old direction for ever. The list is both the replay buffer for corrections and the client's own schedule of inputs whose tick has not come, so trimming it as a replay buffer emptied the schedule. The fix keeps unapplied inputs: `retain(|p| p.seq > seq || !p.applied)`.

**2. The client predicted through the round-over freeze.** The round-over interval freezes every player so the last explosion stays readable and *nothing in a frame said so*: the players simply stop moving, which looks the same as everybody standing still. The client kept predicting through it, so every frame of the interval produced a correction for a rule the client had never been told about. A server that stops simulating has to tell the client. This is the bug `PredictedPlayer::set_active` was added for in the black hole example, turning up again in a different game.

**3. Prediction ran per frame instead of per tick.** The client advanced its player once per rendered frame; the server advanced once per tick. Even at matching rates the two grids are unaligned, so they cross every cell boundary up to a tick apart and any frame arriving in that window is a genuine disagreement. It scales with boundaries crossed, which is why **open ground** made it obvious and a cluttered board hid it. Fixed by making the prediction a function of the tick: catch up to `clock / SIM_STEP_MS`, one step at a time. The `dt` parameter was then deleted from the client's `tick` entirely, because a caller must not be able to influence how fast a prediction runs.

**4. The server did not advance in whole ticks either.** `TickDriver::run` hands over the **measured** elapsed time, which is correct and documented for logic that integrates and breaks logic that is predicted: at 62 Hz it delivers 16, 17, 16, 16, 17, so the simulation's rate becomes a property of the host's scheduler. The client stepped in exact ticks, the server in measured ones and they accumulated a walk at different rates. This was the residual that survived three client-side fixes, at 2.2 snaps per hundred frames with no packet loss, almost no jitter and every input accepted on time.

**The shared cause.** This is the fourth principle at the top of this file in a real case. A shared rule and tick-addressed inputs are not enough for prediction. **The authority and its clients must advance the same simulation in the same quantum, on the same clock, from the same rule** and all four conditions matter. Three of the four bugs above broke the *quantum*, in three different places. Each looked like a network problem because a correction is what a network problem looks like.

**A grid shows these bugs where a continuous game hides them.** Each of these bugs exists in a continuous game too, as a permanent sub-pixel correction that easing hides completely. On a lattice there is no fraction of a cell to ease through, so the same fault becomes a jump the player can see and the panel can count. That makes a grid game a better test rig for prediction than a smooth one.

**What it changed in plaza.** `TickDriver::run_fixed` and `run_fixed_for`, which pace to real time while delivering whole steps of exactly the size asked for, carrying the remainder and dropping a stall rather than repaying it as a burst. Before this there was no way to run live at a cadence with a constant delta: `run` measured and `run_virtual` was fixed but unpaced. `run`'s documentation now names the hazard. Finding it cost four rounds of blaming the network.

### Turn inputs that resolve at a junction (pellet maze)

The maze example was built after `bomb_grid`'s four bugs, so it started with a shared rule, tick-addressed inputs and a matched quantum. The two sides still disagreed, in a way none of that addresses.

**A turn is a request for a place.** Pressing a direction queues a turn that is taken at the next junction where that direction is open. So the two sides can agree exactly about *when* the request was made, run the same function on the same tick and still resolve it at **different junctions**, because a junction is decided by the input history between the request and the corner. Get it wrong by one junction and the sides end up in different corridors rather than one cell apart and the error grows with every step instead of being corrected away.

That is why the counter is separate. A cell snap is bounded to one jump. A wrong junction is unbounded until a frame drags the client back. Averaging them together lets a hundred cheap corrections hide three expensive ones, so the panel reports `wrong junctions: N of M turns, worst K cells apart` above the snap rate.

**Latency alone does not cause one**, at any depth. Only losing the request does. The two are pinned as separate tests, because the intuition that a place-input is "more sensitive to lag" is wrong and would send the next person tuning the wrong parameter.

**The policy has to cross the wire.** How long a queued turn stays alive is a server setting and a client that assumed a different one would predict a turn the server had already dropped and then run down a corridor the server never entered: a wrong junction caused entirely by a disagreement about policy. It is sent in `ServerPolicy` with the playout depth and the send rate.

**Secrecy depends on what the server sends.** The invisibility power-up made the frame **per recipient**: a hidden player is *absent* from everybody else's copy rather than flagged in it. A client handed a position it should not have already knows the secret, whatever it renders. `card_table` applies the same rule to a hand of cards; here the hidden thing moves sixty times a second. Per-recipient frames cost one clone per seat here and nothing cheaper keeps the secret.

**It still leaked, because a frame is not the whole stream.** Hiding the player from every frame shipped and did not hide anything, because the *events* were still broadcast: a pellet vanishing names the exact cell on the exact tick and being an event rather than a frame it is not even rate limited. A power-up taken names a cell. A turn report names the junction and no client ever read another player's. Two frame fields gave it away too: a pellet count that dropped while nothing visible moved and a pickup that disappeared from a cell. The fix is an audience on each event, held for everybody else until the vanish ends and then sent late rather than never, plus the two counts computed per recipient. Secrecy covers the entire outbound stream and the leak tends to be a message you did not think of as a position. The test reads the wire rather than trusting the server's intent: take the hidden player's real cell each tick and check every op every other seat was handed.

**Two bot bugs with one cause, unrelated to the netcode.** Three of four seats are bots, so a bot that looks broken makes the example look broken. Both faults were guards that were true far more often than they looked: `drive_bots` skipped any player mid-step and since a player begins its next step the instant it ends the last, that was nearly always, so a bot only ever chose a direction while already stuck against a wall. And the routing BFS was seeded in every direction including backwards, so a runner ate the cell it stood on, found the nearest remaining pellet behind it, turned round and paced one corridor for the whole round. Fixing both took eating from 36 pellets in 45 seconds to 165. Neither was visible without a number, since both look like "the bot runs around a lot".

**A measured feature that was deleted.** Routing a threatened bot runner toward a nearby energizer sounded obviously right and was written. Measured over a minute it devoured no more pursuers, ate 22 fewer pellets and left six power-ups uncollected, because a runner already crosses every corridor eating and walks over them anyway. It was deleted and the measurement kept in the test that replaced it.

### Where floats break determinism (seed defense)

The determinism example sends a seed instead of a world, so an arithmetic difference between two machines is never corrected: it compounds for the length of a wave. Building it meant writing deliberate ways to break determinism, so that the detector could be shown catching something. **Two of the first three did not break anything.** Why they failed teaches more than the one that worked.

**A float in an accumulator does not diverge if the result is re-quantised.** The first quirk moved enemies with an `f32` multiply and add instead of fixed point. It never changed a single tick, on any seed, over any length of run. The reason is that the result is truncated back to 1/256 of a tile *every tick*, so the float error is discarded before it can accumulate: the sum of a hundred rounded values is the same whether or not each addition was exact. This is the strongest argument for fixed point and people often get it backwards: "we use floats internally but round the positions" really is most of the protection.

**A float in a *constant* breaks it immediately.** A runner covers 4.2 tiles a second, which at a 25 ms tick is 26.88 in 256ths. The integer ratio floors that to 26. Working the same number out in floating point and rounding gives 27. That is four percent, compounding for ever: ten seconds later the two machines' runners are a tile and a half apart and each is being shot at by a different tower. **Audit the constants first.** A rate, a radius or a price computed once at startup is where the machines diverge and nobody re-reads that code because it holds no state.

**A float in a rarely consulted comparison diverges too rarely to matter or to test.** The second failed quirk made a tower's *range* a float, changing the radius by 1/256 of a tile. It is a genuine difference and it is nearly unobservable: it only changes anything during the fraction of a tick an enemy spends inside that band while a tower happens to be off cooldown. It is real but undetectable in a minute of play, so it was useless as a demonstration. When triaging, remember that a determinism bug's *frequency* depends on how often the differing value is consulted rather than on how wrong it is.

**The third failed quirk could not diverge at all.** It was "iterate the towers in hash order instead of placement order", which sounded like the canonical determinism bug and turned out to be impossible in this code: damage is additive and the dead are collected after every tower has fired, so no tower can take another's kill within a tick. The toggle would have sat in the panel implying a detection that could never fire. It was replaced by a *targeting rule* quirk, which does diverge. The loop now carries a comment saying why its order is safe.

**Test a fault injector like anything else.** Each of the three is now covered by a test that asserts it *does* change the world, so the panel cannot claim a detection that no longer happens.

**The first real bug the digest caught was in the server.** The server laid a wave out at the end of the tick it announced the wave on; clients laid it out at the start of the tick the announcement named. One tick apart and every wave began with a mismatch. The wave is now scheduled to a tick exactly like a build op. If two machines must do something at the same moment, name the moment explicitly rather than letting each act when it notices.

### Replaying a recorded run (ghost trials)

The racing example stores a run as the inputs that produced it and replays them to make a ghost, which means a machine has to agree with **a recording made somewhere else, at some other time**. That is `seed_defense`'s problem without a second live machine and everything true there is true here. The difference is that a recording cannot be corrected.

**An event log is small because it records only changes.** One entry per change of input rather than one per tick: a two-lap run is 146 entries over 1208 ticks, 738 bytes against 12,088 bytes of sampled positions. The saving has a limit. The autopilot that drives the tests originally steered on *every tick*, the way a bang-bang controller does and it scored barely three times better than the path. Giving it a deadband so it drove like a person took the ratio to sixteen. **The saving depends on how long the input holds still**, so it is large for human input and much smaller for machine input.

**Verifying by replay is exact and cheap.** The server never watches anybody race: it is handed a log and a claimed time, replays the log and compares. There is no plausibility check, speed cap or statistical model, because the inputs either produce that time or they do not. The cost is small: one trial is about 1200 ticks of integer arithmetic, once, at the end of a run somebody spent half a minute driving.

**The rules belong in the version.** `build.rs` hashes `rules.rs` into the wire version alongside the message shapes, because a change to how a racer handles invalidates every stored log exactly as surely as a change to a message shape does. A log carries the version it was recorded under and one from a different version is refused rather than replayed wrong: replaying it would produce *some* run, but not the one its player drove. This failure (an honest player, a valid log, rules that changed since) is harmless only if something detects it.

**The self check found a real bug on its first run and it was not in the physics.** The client replays its own finished log and compares it to the run it just drove. That should never fail on one machine with one implementation. It failed immediately: `finished_tick` is the *index* of the tick a lap completed on, so the ticks taken is one more than it and the client counted one way while the replay counted the other. The difference was twenty milliseconds and invisible on screen. It would have made **every honest submission** get refused for claiming a time its own log did not produce.

The check does not test the simulation, which other tests cover. It tests the **recorder**, which nothing else checks: a recorder that closes a span one tick early produces a ghost that slowly drifts away from the run it came from, in a way that looks like bad luck. Any system that stores events to rebuild state later should have this check. It is four lines.

**Bot opponents add nothing to the log, like a wave of enemies from a seed.** The race mode puts three CPU drivers on the circuit and the log does not grow by a byte, because a bot's input is a pure function of the world it is in. One player's key presses reproduce a four-way race, every shove and every stolen pickup included. The condition is strict: if a bot reads a clock, a random generator or anything else the log does not carry, the race stops being reproducible. So the mistakes that make the field feel real come from a **hash of the tick and the seat** rather than from a random number generator, since a generator is hidden state that a recording would have to save and restore.

That noise is sampled in chunks of ticks rather than per tick, for two reasons. A driver that changes its mind every tick looks like it is twitching rather than making mistakes. It also produces one log entry per tick, which stops the event log being small. So holding an input still for longer improves both how the bots feel and how big the log is.

**Impairment sliders that were not connected.** The racing example shipped with latency, jitter and loss sliders that were wired only to the offline test harness. On a real host they did nothing. That is worse than it sounds, because the example's main claim is "latency cannot affect your lap": a player drags the slider to 400 ms, sees no change on screen and takes it as proof of something that was not being tested. The impairment now runs on the real path (the verdict and the ghosts go through a per-seat link and a submission can be dropped), so the claim is tested against a genuinely bad link. Check that every switch in a demo can make something fail.

**Latency really is off the path.** Four runs at 0, 80, 250 and 400 ms one way produce *identical* times, because the run happens on the machine driving it. Every other playground here works to make latency cheap; this is the only one where it costs nothing, because of how it is built.

### Touch input in browser builds (all the playgrounds)

Every playground here ships a wasm bundle, so every one of them is one URL away from a phone. Two of them, `bomb_grid` and `pellet_maze`, were driven entirely by `WASD` and had **no pointer input at all**: the page loaded, the game ran at full speed and nothing a finger could do would move anything. That is a worse failure than a crash, because it looks like a working demo.

**macroquad synthesises a left click from a touch by default**, which is why the examples driven by *tapping* were already fine without anybody thinking about it: a menu, a build strip, a tile to place a tower on. `is_mouse_button_pressed` fires from a tap at the same coordinates.

**It does not cover holding or two touches at once.** A synthesised mouse is a single pointer, so "steer left while charging" cannot be expressed through it. Anything holdable has to read `touches()` directly and take the mouse as one extra pointer only when there are no touches. Otherwise one finger reads as two.

Two smaller decisions. **A discrete game gets buttons rather than a stick**: three of these games take one of a few values and thresholding an analogue drag back into them is one more threshold to get wrong. A stick is right for the two that steer continuously. And **the controls stay hidden until the process sees its first touch**, latched rather than sampled, because a thumb pad drawn over a desktop window covers what the player is looking at and one that appeared and vanished between taps would be worse.

### Three bugs that made the example look like it was working (hit_scan)

**Symptom.** None: nothing looked wrong. The rifle was devastating, the bot stood its ground and the movement looked smooth. Each of the three was found by a test that had been written to assert something else.

**The shooter was in their own ray cast.** A ray starting at the centre of a body hits that body at zero distance, so every trigger pull resolved as a hit on the shooter and no shot ever reached anybody else. The first duel test came back `hit == Some(0)`, where 0 was the shooter. Read as a hit rate, it is a weapon that never misses.

**The prediction ran a playout depth early.** `press` set the held direction as well as scheduling it for the tick it named, so the client walked from the keypress and the server walked from the tick and the two took the same route out of step. 240 corrections in eight seconds and on a continuous body every one of them is a few units that easing hides. This is the second item in [bomb_grid's entry](#four-bugs-with-one-shape-bomb-grid), which was written down and cross-referenced and then reintroduced by hand in a different file. Documenting it did not prevent it. Porting bomb_grid's *test* over caught it.

**A bot wedged against a wall for the whole session.** Steering is quantised to eight directions, so a bot pressed against a vertical face while wanting to go a few degrees off due west resolves to due west, which has no vertical component left to slide along. It pushed into the wall for nine and a half seconds of test time without moving. A stationary bot looks plausible, as pellet_maze's broken bots did above: the failure state and the healthy state look the same from outside.

**The general form.** A bug that leaves an example running lasts until somebody distrusts a number. All three of these had a plausible reading and none would have been found by playing. Assert the *premise* as well as the logic: that a shot can miss, that latency alone produces no corrections and that a bot covers ground.

### A bullet curtain that was too thin (curtain_fire)

**Symptom.** Every test passed and every number was wrong by an order of magnitude.

**Cause.** The enemy curtain is a closed-form function of the tick and the emitters were written to release one bullet per period regardless of pattern. That is correct for a spiral and wrong for a ring, which is *defined* by releasing a salvo at once. So the "ring" was a slow spiral, the field peaked at forty bullets and the example's main claim, that a derived half costs a fixed number of bytes where a streamed half costs per bullet, was being demonstrated on a field small enough for the streamed half to be affordable.

**Fix.** Salvo size became a property of the pattern and the emit loop starts at the oldest still-live salvo rather than at zero, so evaluating a wave costs the same at the end of it as at the beginning. The test now asserts density directly, per pattern and separately asserts that the densest one is a real curtain.

**The general form.** Test a demo's premise as well as its logic. A thin curtain and a thick one take the same code path, so nothing failed; the byte comparison was correct for a situation other than the one being claimed. Any example whose point is "at scale, X beats Y" needs an assertion that the scale is present.

### A harness that aimed at the server's positions (hit_scan)

**Symptom.** A skirmish test ran, ships shot each other, every verdict came back `Plain` and the number the panel exists to show read zero.

**Cause.** The harness aimed at the server's position for each target, because that was the position it had to hand. Lag compensation exists to reconcile *the shooter's view* with the present, so a shooter that aims at the present has nothing to compensate: the rewound world and the current world both contain the target, every shot is `Plain` and the granted-by-rewind counter is correctly zero. The test measured a scenario in which the feature does nothing.

**Fix.** The harness reads `client.render()` and aims at what that client is drawing, which is what a player does.

**The general form.** This is the same mistake as [`mean_render_error`](#mean_render_error-and-the-two-send-rates) one level up. It has now appeared twice in this repository: **a measurement taken against the server does not measure what a client experiences.** Wherever a harness uses server state because it is convenient, ask whether the thing being measured is defined by what a client had. If it is, the convenient number answers a different question while looking fine.

### A speed check that credited time per request (gow_3d)

`gow_3d` gives the client authority over its own position, so the server's only defence is asking whether a claimed position was reachable. The obvious form is to measure each claim against the time since the last accepted one. It has a false-positive problem: claims arrive between ticks, so two that bunch up measure zero elapsed against each other and the second is refused **for arriving together**. Jitter alone produced refusals and refusals are the only signal the design has that somebody is cheating.

The obvious repair is to credit a minimum, one tick of clock grain, because the server cannot resolve anything finer. That fixed the false refusals and opened a hole twice as bad as the one it closed: **whatever you credit per request, the caller can claim as often as it likes.** A client sending twice per tick was credited a full tick each time and a 2.0x speed cheat passed at the full 2.00x, where before it had been stopped completely.

Nothing about the code looked wrong in either version and the unit tests for both passed. What caught it was a table printed by a test that had been there the whole time: a row reading `2.0x  achieved 13.44  gain 2.00x` where every previous run had read `1.35x`. **A test that prints a number for a person to read catches different things from one that asserts a bound.** This one caught a regression no assertion in the file was watching for.

The correct shape is a **budget that accrues from the clock and is spent by movement**, rather than an allowance recomputed per request. It cannot be gamed by asking more often, because it accrues from elapsed time however many times it is asked: two bunched packets spend one budget between them and two whole steps in no elapsed time are refused, because that is twice the speed rather than a bunched packet. It also has to be capped so that a disconnection cannot bank enough budget to teleport.

The same applies outside games. Any check of the form "is this request allowed given the time since the last one" has the same two failure modes and the same fix: rate limits, token refills, retry backoffs and quota accounting all want an accruing bucket rather than a per-request allowance.

### A protocol hash over the ops misses the types they carry (poketo)

`plaza_wire::build::emit(&[paths])` hashes the files you list. poketo listed `protocol.rs`, which is where its ops are defined and the ops carry types from two other files: `Overworld` embeds `Trainer`, `BattleState` embeds `Battle`. A creature could gain a field or a tile could change shape without the version moving at all. Two builds that disagreed about the wire would complete the handshake, agree they matched and then mis-decode, which is worse than refusing to connect because it looks like a game bug.

Adding the two files to the list fixes this case only. `Wire::detect()` fixes the whole class: it starts from types tagged `/// plaza-wire: root` and walks their fields transitively, so a payload two files away counts and a type nobody references warns rather than silently drops out. **Nobody has to remember to update a field walk.** A hand-maintained list of what is on the wire goes stale during the very refactor that changes the wire.

This was verified, because a version hash looks right whatever it does: adding a field to `Creature`, two files from the ops, moved the version from 3864428394 to 561229205 and reverting restored it exactly. Under the file list it did not move.

The resolver also caught something unrelated to the hash. gow_3d had **two `Authority` types**, one in `controls.rs` and one in `protocol.rs`, with a `match` converting between them; the resolver refused to build, naming both. That is the two-derivations-of-one-fact problem this catalogue keeps finding and a build step that indexes wire types by name detects it.

### An unplayable demo hid a protocol defect (gow_3d)

The netcode core took an afternoon and was correct but unplayable: an empty tower, a cube and three of four keys that did nothing. One of those keys was a genuine protocol defect rather than an empty zone. It survived because nobody could tell "nothing happened because there is nobody here" from "nothing happened because the frame never told you about yourself". The fix was a `You` block on the frame, because what a player must know about themselves is not a subset of what they are told about anyone else and the client never appears in its own audience list. The plan had ruled out terrain, mobs and jumping on the grounds that adding content would stop it being an example. That was wrong and chapskape's non-goals list is short because of it. Content makes the netcode's defects visible, because an empty zone cannot show you a wrong frame.

### Click-to-move prediction (chapskape)

One op of 13 bytes buys a 22-square journey, 13.3 seconds of walking; the same ground under a held direction at 30Hz is 398 ops and 4774 bytes. The prediction is exact because the rule is shared: terrain and walkability come from one seed and both ends run the same search. Every bug in it was somewhere the plan did not look.

**The divergence check the plan specified was the wrong check.** Comparing the client's square against the server's counts the phase offset as error, because the client acts on a click the instant it happens and the server a tick and a round trip later. The useful check is whether the server walks the squares the client already drew, spent against a queue; it reads zero for a whole session because the route was never in doubt, only its timing. **A shared rule only gives a free prediction from a shared start.** A click taken mid-walk reaches the server a square later than the client acted on it, the server re-routes from where *it* is and the two walk different lines to the same place; a shared rule cannot fix a different starting point. The first version counted every re-click as divergence and corrected to the server's square, which looks on screen like the world undoing an input. Reconcile at rest instead, once the walking is over and the two ends agree anyway.

**Every jump reported as a server correction was the client moving itself.** Three fields describe where a body is (the crossing's start point, the square being stepped into, the step clock) and the first version rewrote all three on every click, teleporting the rest of a crossing and granting a free step that let fast clicking outrun the server. The start point had to stop being a square: a click lands mid-crossing, so the next crossing starts from wherever the body is *drawn*. Worst frame-to-frame movement of the drawn point under click spam went from a full square to 0.118.

**A hash map's iteration order breaks determinism wherever it reaches a shared stream, including outside the obvious algorithms.** The pathfinder was right from the first version, dense arrays and an explicit total order; the bodies were collected out of a `HashMap` and then drew from one shared random stream, so the same tick run twice was not the same tick. `a_tick_is_the_same_tick_when_it_is_run_again` found it on its first run; `client_utils::determinism` says so in its module doc now.

**Decide who an event is for before deciding what it says.** Experience arriving and a level going up are transcript on a private channel; they went out on the shared event list because that is where transcripts lived. Two faults came from one mistake: everyone within sight paid for every body's experience, 89 bytes a frame in a lived-in world; the events carried no seat because on a private channel there would have been nobody else it could be about, so a client announced every passing bot's level as the player's own. On a shared channel, "who" is a field you can forget to send.

**Derived data still costs CPU.** Deriving from the seed saves bytes on the wire but not work in the search: a search settles thousands of squares and asks each whether it is walkable, three octaves of noise and four neighbours per answer, so one click was a million hashes. Both ends build one table from the rule at startup and read it afterwards, which is still derivation and not a payload; the lib tests went from 70 seconds to 1.

**Density depends on the population relative to the map.** A hundred and ninety-six bodies over 36864 squares puts six in a 24-square view; six is indistinguishable from a broken frame, which is the failure gow_3d shipped. Ninety of the world's own bodies rather than twenty-eight; a joiner two minutes in arrives to six people in view and ten things on the ground.

### Smaller bugs

**Three measurements whose scenes could not show the effect (gow_3d).** A tower test comparing a height filter against a volumetric grid, where the tower was 40m tall and the view radius 30m, so the volume grid's vertical reach covered the whole building and excluded nobody: both arms examined all 480 people and the comparison had no contrast. A walk test that started every character with a 12-unit jump onto the axis it was walking them along, so the validator was right to refuse it and the refusal count measured the test rather than the code. A cheat-cap test whose jump was larger than the cap could ever bank, so it landed nothing and "gained no more than an honest runner" passed with **zero gained**. In all three the code was fine, the scene could not produce the effect and each returned a plausible number rather than an error. The fixes are all assertions about the *scene* now (the tower must out-reach the view; the cheat must land jumps before the cap is checked), because a fixture that silently stops exercising the thing is worse than one that breaks.

**A test lane that ran straight through two pillars.** Every ray test in hit_scan's first draft failed, none of them for a reason involving the code: the obvious horizontal line across the arena at y 100 crosses both side pillars. It is a named constant now (`OPEN_LANE_Y`). Check geometry fixtures as carefully as the numbers they produce. This one happened to fail loudly.

**Ctrl-C would not kill the windowed host.** Actix caught the signal for a graceful shutdown while the window kept running and the controller sprayed queue-full errors into dead links. Fixed with `disable_signals`, leaving signal handling to the process.

**Turning coins off crashed a joiner.** The client indexed its own wallet in a list the server stops sending when the feature is off. The first joiner takes seat three, so it indexed three into an empty list. Latent in the offline build too, where seat zero happened to be safe.

**Changing the enemy count made the horde lunge.** Rebuilding the world reset the server clock to zero, so every client's packet-age estimate spiked and projected samples wildly forward. Fixed by preserving the clock across a rebuild.

**The offline build refused to start.** The default role is host, which requires a server and the teaching build deliberately compiles no networking at all. The role check now only runs in builds that have networking.

### A per-recipient view requested with `uniform` (held_fire, check_raise)

Both examples wrote a `SnapshotProvider` that cuts the view per target, poker's hole cards and a fog of war and both requested snapshots with `SnapshotRequest::uniform`, which calls the provider **once with `target_agent: None`**. The provider's fallback for `None` was the spectator, so every client received the whole board: the fog example shipped without fog and the poker table dealt everyone's cards face up, while every unit test stayed green, because the tests exercised `view_for` directly and the request kind lives a layer above them. Found by a human looking at their own hand and seeing card backs. The per-recipient request is `SnapshotRequest::to`. A filtered view has **two** halves: the filter and the plumbing that routes a target into it. A test that pins only the filter says nothing about what a client receives. This is the same problem as pellet_maze's leak one level up: secrecy depends on the whole outbound path, which includes the op stream, the panel's arithmetic, the timing of a window and the snapshot request kind. held_fire closed the first three (audience-filtered steps, masked counters, the decision applied only at the window's close) and still shipped with the fourth open.

### A turn owner that could not be empty (held_fire, last_word)

All four turn probes end at the same client question, "is this mine to act" and they split two and two on whether the view can answer *no one's*. check_raise's `to_act` and turn_gauge's `current` are `Option`s, so a lobby snapshot carries `None` and the predicate cannot claim an owner before one exists. held_fire's `side_to_act` and last_word's `priority` are bare `u8`s whose default is 0, which is a real side, so the first snapshot of an empty lobby already names an owner and every consumer must remember to ask the phase before believing it. last_word remembered twice, once as an early return and once by writing `priority == seat && phase == Dueling` longhand beside the ungated `my_window()` it duplicates; held_fire forgot once and shipped it: the blue commander joined an empty board and got the pulsing YOUR ACTIVATION line and a running activation clock five seconds before the bot seated and any unit existed, while the server, which schedules its real clock only at battle start, would have refused every order with "no battle is on". Found by the person playing it. The phase guard was the first fix and the weaker one: `Option` makes the wrong state unrepresentable while a guard only works where someone remembers it. The two probes that had `Option` by construction never had a call site to get wrong. Both bare fields are `Option`s now, `Some` exactly while the fighting phase holds, which deleted the guard and last_word's longhand duplicate with it.

## The diagnostic playbook

What worked repeatedly, after several rounds of confident wrong guesses:

**Instrument before theorising.** Every hypothesis in this document formed from reading code was wrong and every one formed from captured data was right. The pattern that worked was: add a targeted diagnostic behind a switch, have a human play for a minute, read the output, fix once.

**Print the value that tells causes apart.** The first correction log reported the server's dash flag, which reads true throughout a grapple because dashing is how a grapple is fought, so it pointed at a cause that was pure coincidence. Adding the distance to the nearest hole showed the actual cause immediately.

**Make thresholds adaptive.** A thirty pixel correction means nothing without knowing the send rate, the latency and how much contact is happening. Tracking a running mean and variance and reporting deviations from it keeps working when conditions change, which a constant tuned once does not.

**Read the shape of a counter.** A counter climbing without bound means a systematic cause, one that plateaus means a transient, self-healing one and a regular sawtooth means a threshold. Each shape pointed at a different class of cause and was right each time.

**Compare against the offline build.** Both examples keep a single-process build with the same simulation code. Any counter that reads zero there and non-zero over a socket isolates the fault to the transport or the code wrapping it, which removes most of the search space at no cost.

**Check the harness before believing a zero.** A measurement of the ghost read zero at every setting and the finding was nearly written up as "there is no unresolved state." The harness had no acknowledgement loop, so the server's baseline never advanced, every frame was a full rebuild carrying no samples and the number was zero for a reason with nothing to do with the question. It cost several rounds. Treat a zero from a harness you wrote as a fact about the harness until you have shown it can produce a non-zero.

**Writing that down did not stop it happening again.** A later harness, measuring what a player costs the server, was built the same way and inflated both its bandwidth and its CPU figures, which were quoted in a doc comment before the zero in the sample column was noticed. The prose did not prevent it. The fix is that `examples/players.rs` now *asserts* its sample bytes are non-zero, so the harness fails instead of reporting. Any measurement harness over an acknowledged stream should carry the same assertion, because the failure produces numbers that look entirely plausible.

**An assertion that cannot fail passes for the wrong reason.** Three were written in one sitting and each read as a real check. `error.min(CONST)` and then `assert!(error < CONST)`, which the clamp guarantees. `assert!(gained <= honest)` in a scenario where the cheat's jump exceeded the cap it was testing, so `gained` was **zero** and the bound held vacuously. `assert!(health >= was.saturating_sub(was))`, which is `health >= 0`. None of them can fail, yet each looks like the check it was meant to be.

In each case the setup had stopped producing the effect, so what remained was a comparison that could not lose. The fix is to **assert that the setup produced the effect before asserting anything about it.** The cheat has to land jumps before its average is worth bounding; the first choice has to deal damage before a resend is worth checking; the tower has to out-reach the view before two strategies are worth comparing. Those guards are cheap and fail loudly when a fixture drifts.

**A test that pins a number can be pinning a coincidence.** One assertion required *zero* contested pickups lost. It held at one configuration and broke at every other and measuring across the range showed a steady 1 to 3 that does not improve with a wider buffer, which is the signature of a float tie-break rather than staleness. The assertion was over-fitted; the code was fine. Prefer asserting the shape (rare, bounded, monotone) over asserting a value that happened to come out round.

**Watch a new test fail before trusting it.** Three of the four bomb grid fixes came with a test that passed on the first run for the wrong reason: the player walked into a wall and stopped, so "it stopped" held whether or not the fix worked. Each only became a real test after the assertion was moved somewhere it could fail (mid-walk, short of the wall) and the fix was disabled to confirm it did. That takes a minute.

**Distrust a counter that reads zero right after a fix.** The harness lesson above applies to readouts too. After four fixes the snap counter read zero, which means either the netcode works or the detector is broken. You cannot tell which until loss is dragged up and the number climbs.

**Instrument by elimination when the theories run out.** The residual in bomb grid survived three fixes and every remaining hypothesis was wrong when tested. What found it was adding readouts that could *rule things out*: the client's simulated tick against the newest frame's tick and the server's late-input counter which the panel had been hiding. Once those said "clock healthy, no inputs late, no loss", the only thing left unmeasured was the authority's own step size and that was it.

**Run a shadow A/B instead of toggling.** To answer whether predicting the dash is worth it, run two predictors over identical inputs differing only in that flag and compare their mean error. It takes one session, needs no toggling and does not rely on remembering how the last run felt.

**A test written from the fix's theory only tests the theory.** Two fixes for the resume bug above shipped green: each came with a scripted socket test that reproduced the author's *model* of a resume and proved the fix worked against it. Both were wrong in the browser. A test built on the same assumption as the fix cannot show the assumption is wrong. What worked was instrumenting the real client, playing until it happened and reading numbers nobody had predicted.

**Check that the test tells fixed from broken.** Once a fix is written, disable it and check the test actually fails, then restore. The tick-floor test reads `-6213` with the floor removed and passes with it, so it measures the fix. It takes a minute.

## What this changed in plaza

The principles above are guidance. These are the code changes the bugs led to. All of them shipped and each stays within plaza's north star: one concern, usable alone, generic over application types and additive to the existing primitives. The application still owns its payloads, its physics, its socket and its tick.

**Prediction gets a context.** `PredictedPlayer` gained a context parameter, because a client predicting a *forced* entity had nowhere to put the forces and black hole was smuggling the whole gravitational field through every buffered input. Without a way to pass the world in, people write the second, lesser rule the shared-code principle warns about, so this was a deficiency in the API rather than in the game.

**Pausing and teleporting a predicted entity.** `set_active` and `teleport` were added, because nothing expressed "the server is holding this still" and black hole's client was inventing a correction every packet for a hole frozen through a respawn delay.

**Reconciliation reports what it did.** `reconcile` returns a `Correction` and `CorrectionMonitor` keeps the running statistics both examples had hand-rolled, twice, including the same adaptive outlier test.

**Both server input models are supported.** `HeldInputPredictor` joins `PredictedPlayer`, because replaying discrete inputs against a server that holds a direction and integrates it double counts, which is why horde had abandoned the primitive and hand-rolled a worse one. The two models are now named in the crate docs so the choice is made deliberately.

**Delta bookkeeping for relevance streams.** `server_utils::DeltaBaseline` owns the per-subscriber baselines, the acknowledgement frontier, the staleness rebuild and the digest drift check. Two of the worst bugs here were defects in that bookkeeping rather than in the game and the block never needs to know what a key means.

**The impairment link is transport-faithful.** `net_sim::LatencyLink` defaults to ordered delivery and both private copies of that queue are gone. Black hole kept one for a while after horde was migrated and it was still the unclamped version: at the shipped defaults (15 ms of jitter against a ~16 ms send interval) it could hand its own client an older frame after a newer one, which the pellet stream has no tolerance for because `swallowed` and `spawned` are order-sensitive.

**The client half of the delta stream is a block too.** `client_utils::DeltaMirror` is the exact counterpart of `DeltaBaseline`: it applies the packet, checks generations, counts the sequence gaps, folds the digest and compares it. Shipping only the server half meant anyone adopting `DeltaBaseline` got no help writing the side that has to agree with it, which is where every serious bug here was. It also carries the rule that was previously a comment in one example: **apply every packet whatever baseline it names**, because these deltas carry absolute values and so are idempotent, while a client that discards what it cannot rebase starves its own mirror.

**Both sides now share one definition of the things they must agree about.** `SlotKey` (the `(index, generation)` pair and its `u64` packing) and `SetDigest` (the fold) live in the client crate and `server_utils` re-exports them. Two implementations that agree today will eventually disagree. The failure would show up as a divergence about the *world* rather than about the arithmetic, which makes it hard to trace.

**Seats are a block and freshness is part of their type.** `server_utils::SeatTable::seat` returns a `Seating` rather than an index, so `Fresh` and `Existing` cannot be collapsed. That distinction is exactly "reset this seat's accumulated state" versus "do not" and forgetting it is the warm-arena join bug that opened this whole investigation. The caller has to match on it, so the compiler enforces the distinction.

**The listen-server scaffolding was duplicated too.** `plaza_session::host::Host` owns the HTTP layer: the stamped index, the revalidation headers, the preflight on the served directory, the banner and leaving signals to the process. `plaza_wire::build` owns the version hashing, which had been copied byte for byte between the two examples.

**Role parsing lives under `examples/`, not in the library.** The four roles and their argument parsing had to move out of both playgrounds and they could not move into `plaza_session`, because the browser client needs the same vocabulary and a wasm bundle must not inherit an HTTP server to learn the name of its own role. That forced a dependency-free crate. Publishing it was tempting, but every real application already has its own argument parsing and it would have made a published crate depend on a CLI. It lives under `examples/` as shared scaffolding instead. Deduplicating the code did not require putting it in the library.

**Fixed timestep.** `client_utils::FixedTimestep` and `Periodic` replaced six hand-written accumulators (the filed count said five, so even the count was off). The pattern is five lines, but the copies made three decisions differently: whether to cap the catch-up after a backgrounded tab, whether to carry the remainder or zero it and whether the thing being stepped gets told the step size. The last one causes bugs, because a client integrating by its frame delta against a fixed-step server drifts continuously and it looks exactly like network jitter.

**Two derivations of one rate disagreed by unit truncation.** `FixedTimestep::from_hz` computed `1000 / hz` in integer milliseconds while `TickDriver::from_hz` was exact, so a 60Hz client stepped 62.5 times a second against a driver ticking 60, ran 4.2% fast and was corrected every frame for it, which is why spacemo refused the block and hand-rolled an accumulator with a twenty-line comment explaining why. Both now compute exactly the same expression over integer-nanosecond internals and a test in `plaza` pins the two to each other. The code is duplicated deliberately because a dependency edge would be wrong in both directions: the core crate is tokio-bound and the client crate must stay runtime-free. This is the same problem as `SlotKey` and `SetDigest` above. A rate constant hides it easily, because both sides read the same number and mean different things by it. Two habits come out of the fix. A fixed timestep is two numbers (how often and how much) and nothing checks that they agree. `Steps` already yields the interval, so a body that integrates what it is handed rather than a constant cannot drift; the two examples that did drift derived both numbers from one shared `SIM_HZ` and truncated only on the way to milliseconds, which is what made it look safe. And the usual choice avoids the problem entirely: at 50Hz or 100Hz the rate divides 1000.

**Input coalescing needs a keepalive.** `client_utils::InputCoalescer` carries the keepalive and the reason for it: when sending only on change, a *dropped* direction change leaves a wrong state that persists, because the server holds the last direction it received. The player keeps gliding until they press something else. It is intermittent and looks like the controls sticking rather than packet loss.

**Input rejections are split by side.** `InputSchedule` splits its rejections into closed-tick and too-far-ahead and keeps the last margin in ticks, because a single total says a player cannot act but not why. The two sides have opposite causes and opposite fixes. The client cannot report this itself: an input is acknowledged on arrival, before admission, so a refused input and an applied one look identical from there.

## What the wire work measured

**Most of the byte cost came from how often things were sent rather than from the encoding.** Horde's downstream sat around 70 KiB/s at the defaults and the obvious suspect was the encoding: the server's bandwidth model priced a 3-byte id and a quantised position while the wire actually carried a two-element array per handle and two 5-byte floats per position. Making the wire carry what the model priced (one packed integer per handle, fixed-point `i32` pairs per position, `u8` for every fieldless enum) was worth about **1.5x**. Correcting each visible enemy four times a second instead of in every packet was worth **4x** and it is not an encoding change at all.

**MessagePack writes enum variant names out in full**: every spawn was carrying six bytes of `"Swarm"` and every departure eleven of `"OutOfRange"`. Compact mode drops *struct field* names, not variant names. Where a fieldless enum rides a hot path it wants an explicit `u8` mapping, with the numbers pinned in the conversion so reordering the variants cannot silently renumber the wire.

**Each quantised field has its own reason.** Positions cross at 1/16 of a unit because that is two orders of magnitude under the smallest enemy radius and nothing reconciles against an exact float; handles cross as the packing the digest already uses, so there is not a second packing to disagree with. Both are `#[serde(into/from)]` conversions, so the simulation stays `f32` and the quantisation happens only on the wire.

## Zone cost at large populations (gow_3d)

A sweep of one zone from 8 to 4096 connected clients, in bytes **and** in tick time, because the two have different curves and only one of them hits a limit. Every character connected, which is the worst case: a zone of bots costs nothing per bot, since a frame is built for the agents with sockets and a bot has none. Measured at powermode 2, `cargo run -p gow_3d --release --example zone_scale`.

**The first sweep grew the population without growing the world, so it measured crowding.** The spawn spiral's radius is `10 + 7*sqrt(seat)`, which leaves a 232-unit map at 256 characters and `terrain::footing_near` falls back to the origin when it cannot find standable ground. So every population past 256 piled up on one spot and the numbers read as a scaling wall: 535 characters in view at 1024, 3607 at 4096 and a tick 27x over budget. The code was fine; the map had run out of room. **A population sweep has to hold density constant or it measures crowding instead.** The in-view column showed it: it is supposed to saturate and it did not.

Measured separately, the two axes are:

| axis | what moves | curve |
|---|---|---|
| population, constant density | more people, proportionally more room | linear in clients, because each is one more frame |
| crowding, constant population | same people, less room | quadratic in aggregate, because each of N sees N |

**One zone holds 4096 connected clients in 42% of a 30Hz tick on one core**, with the view saturating at 44 from 64 characters onward and cost per client flat at ~3.4µs. The hard case is crowding rather than population: the same 256 people packed until everyone sees everyone cost 5x the tick and 7x the frame and population headroom does not help, because it is the same people in a smaller field.

**A dense store beat a hashed one by 2.3x on the build.** Characters lived in a `HashMap<Seat, Character>` and seats come from `Roster`, which hands out the lowest free one, so they are dense from zero and the map was hashing an index. The lookup sits at clients times audience: 180,000 of them a tick at 4096 of each. Swapping in a `Vec<Option<Character>>` behind a `HashMap`-shaped surface took the build from 18.0ms to 7.9ms and needed no call site to change. Hashing a dense integer key costs time and buys nothing. It survived because it was written when the zone held eight.

**Bit-packing the hot array saved bandwidth but not CPU, against the prediction.** The expectation was that fewer bytes would mean less encode time. Encode did collapse, 5.2x and bytes fell 3.2x and **the total tick got worse**: quantising three positions and two varints per character costs more arithmetic than MessagePack spends writing the same fields out raw. The cost moved from the codec into the packer and the first attempt landed 12% slower overall while showing a 5x win on the column being watched. What made it a net win was removing the `Vec<Seen>` between the two: writing the audience straight into the bit stream saved a walk and an allocation per client per tick and took the tick below where it started. **Measure the total rather than the column you optimised.** A large win in one column next to a rise in another has to be read as one result.

**Quantising a position that a claim is measured against puts a floor under the claim.** The audience is quantised to 4mm, which is invisible on a body and a third of a percent of a step. It is not invisible on *your own* body, because the example's headline comparison is that client authority cannot disagree with itself, measured as a gap of exactly 0.00 units. Reading your own echo out of the quantised audience made that 0.002 and the assertion caught it. The fix was better than the original on both axes: `You.at` already carries your own position unquantised every frame, so the client reads itself from there and the audience stops carrying a duplicate entry for the viewer. **Ask which numbers are measurements before quantising the field they are taken from.**

**Judge level of detail in pixels.** The crowd column is the expensive one and aggregation is the standard answer, so it was about to be filed as a fidelity trade, on the assumption that coarsening a crowd would obviously show. That assumption needed the conversion: pixels per world unit at distance `d` is `H / (2 d tan(fovy/2))`, which for the camera this example actually builds is `1304/d` at 1080p. The error a player can see **shrinks with distance**, so an error budget may *grow* with it. `cargo run -p gow_3d --release --example crowd_lod` prices four schemes in that unit, at 256 characters:

| scheme | worst px | median px | B/client | bodies missing |
|---|---|---|---|---|
| exact (what ships) | 5.1 | 0.1 | 198 | 0 |
| graded by distance | **0.5** | 0.4 | **178** | 0 |
| merged (`AggregateTree`) | **393.6** | 0.0 | 234 | 0 |
| culled at half radius | 5.1 | 0.2 | 50 | **33** |

**Merging is the wrong tool at this radius, by two orders of magnitude.** `AggregateTree` is a good block and the black hole measured it well, but it pays off only where a distant group covers a few pixels. gow_3d's relevance stops at 46 units and at 46 units a body is still **57 pixels tall**, so a centroid it is not standing on is plainly visible: 394 pixels in the worst case and 145 in the median once the zone is crowded. The zone has no far field for the technique to work in. An approximation whose error scales with distance needs a view radius long enough for distance to make the error small and a radius chosen for relevance may not be long enough.

**Grading precision by distance improved both error and bytes.** The shipped layout spends 18 bits over 1024 units everywhere and that is wrong in *both* directions: it is one pixel of precision on the nearest body, which needs 19 and nine times finer than a pixel at the view edge, which needs 15. Choosing the width from the distance took the worst error from 5.1 px to 0.5 and the frame from 198 bytes to 178. So there was no trade-off: the picture is better and the bits are fewer, because a uniform budget is too coarse near the viewer and too fine far away.

The naive fix, a view cap at half the radius, has no error at all on what it keeps but deletes a third of the crowd, rising to two thirds once the zone is packed. Bodies vanishing is the most visible failure in the table and the only one that cannot be tuned away.

**Publishing per cell instead of per client fits this zone and does best when the zone is most crowded.** `crowd_techniques` prices four of them against the same moving zone, with the zone's own bots walking so that anything keyed on change is measured fairly:

| case | people | in view | scheme | build | B/client | vs base |
|---|---|---|---|---|---|---|
| spread | 1024 | 50 | per-client | 1439µs | 515 | 1.00x |
| spread | 1024 | 50 | **cells** | **550µs** | 922 | 1.79x |
| crowded | 256 | 255 | per-client | 1655µs | 2402 | 1.00x |
| crowded | 256 | 255 | **cells** | **121µs** | 2997 | 1.25x |
| packed | 256 | 256 | per-client | 2351µs | 3295 | 1.00x |
| packed | 256 | 256 | **cells** | **116µs** | 3314 | **1.01x** |

Pack each occupied grid cell once and hand every client the blobs its view touches and the build stops tracking the client count and starts tracking the occupied-cell count: **2.6x on a spread zone and 20x on a packed one**. The cost is that relevance becomes cell-granular, a superset of the disc, so a client is told about more than it strictly needs. That cost disappears in the case the technique is for. In a spread zone the superset is 1.79x the bytes; in a packed one the cells are what everybody could see anyway and it is 1.01x. In a crowd a shared answer is nearly right for everyone in it, which is also why the technique amortises. The measurement also understates it, since a cell-scoped client needs no audience query and the harness charged every arm the same zero for that.

**Both delivery modes shipped and measured end-to-end and the harness that argued for the faster one overstated it by 1.6x.** `publish_costs` put the per-cell fan-out at 2.73x the joined path; through `process_input` it is **1.75x**. Neither number is wrong: the harness timed the *delivery step* and a real tick also runs the simulation, `you_of`, the party's extras and the landing filter for every client, none of which either mode can share. **A ratio measured on one stage of a pipeline is an upper bound on what it does to the pipeline.** The more important result: what actually separates the two modes turned out not to be CPU at all, since the fan-out is a fairly flat 1.6-1.75x everywhere, but **bytes**, which swing from **41% worse spread out to 1% worse packed** (1000 to 1414 per client, against 3300 to 3337). Spread, a client's 49 cells hold about one body each and 49 op envelopes are nearly all framing; packed, the same envelopes carry a crowd apiece and the framing vanishes into the payload. So the choice between them depends on bandwidth and density rather than on the CPU cost the harness was built to measure.

**Shipped and re-measured, both axes are fixed: 4096 connected clients cost 14.2% of a 30Hz tick joined and 8.6% fanned out, against 41.7% before and the crowding column now *falls* as the crowd tightens.** The same 256 people packed from 44 in view to 256 used to run 675µs to 3504µs and now runs 246µs to 163µs joined or 181µs to 124µs fanned out, **up to 28x at the packed end**, with a 6x change in view moving the tick *down* by a third. Getting there took three things and only the first was obvious: publish once per occupied cell, **concatenate** the cells a client's view touches into one self-delimiting byte string rather than one field each (48 of 49 envelope framings) and key the publication by a flat `CellSpace` rather than a hash. The last two are worth 2.70x on their own at 4096, against a harness that predicted 2.73x before either was written. With only the first change the population axis was a *wash*, because `build` fell 1.56x while `encode` rose 4.3x and handed the whole gain back one column to the right.

**The first adoption fixed crowding but not population.** gow_3d's own sweep, re-run on the shipped shape: the same 256 people packed from 44 in view to 256 went from 675µs to 3504µs under a per-client frame and now goes 730µs to 478µs, **7.3x better at the packed end and falling as the crowd tightens**. At population, though, 4096 clients moved only 13912µs to 13201µs, because `build` fell 1.56x while `encode` rose 4.3x and handed the gain straight back. A spread zone holds about three bodies per cell, so there is nothing to share and the sharing machinery costs more than it saves; a frame also carries up to 49 byte strings where it carried one, each paying its own envelope framing and the assembly **clones** each payload per recipient rather than refcounting it, which is the exact copy the technique exists to avoid. **A sharing optimisation pays only where the things being shared overlap.** With the same code and the same tick, the technique is 7.3x on one axis and 1.05x on the other purely because of how much the audiences intersect. Measure on the axis you will deploy on.

**Three cheap changes beat the protocol change, which lost on the axis it was supposed to win.** `publish_costs` prices every way to deliver the same cell payloads, each arm charged for the whole per-client path (deciding which cells a view touches, finding their payloads, assembling, encoding). At 1024 spread clients over 652 occupied cells and at 256 packed:

| scheme | build | encode | total | vs | B/client |
|---|---|---|---|---|---|
| frame now | 1328µs | 1165µs | 2494µs | 1.00x | 1015 |
| joined (payloads concatenated into one self-delimiting string) | 1003µs | 273µs | 1277µs | 1.95x | 930 |
| joined + flat (payloads in a `Vec` indexed by cell, not a `HashMap`) | 646µs | 267µs | 914µs | 2.73x | 930 |
| joined + flat + held (and the cell window cached per client) | 633µs | 267µs | 900µs | 2.77x | 930 |
| per-cell ops, hashed (each payload encoded once, addressed to its viewers) | 777µs | 236µs | 1013µs | 2.46x | 1057 |
| **per-cell ops + flat** (the same, given the index every other arm got) | **97µs** | 229µs | **325µs** | **7.66x** | 1057 |

Publishing itself is 72µs of that and is shared by every arm, so the whole spread is assembly and encoding. Full tables, including every density and the byte columns, are in [gow_3d's README](gow_3d/README.md).

Only the fan-out changes doctrine (a client must hold a cell subscription set, so a missing payload can mean "empty cell" rather than "despawn"). Compare a candidate against the best alternative rather than against what ships, but only once the candidates are implemented equally well. This file got that wrong twice.

**A caveat that was noted and not acted on reversed the result.** Every arm got the flat payload index except the fan-out, which kept hashing for both its lookups and its recipient lists. I noted that twice in writing, called it a caveat that "would narrow the gap" and moved on to write a recommendation on top of it. Fixed, the fan-out's build falls **777µs to 97µs**, an 8x drop. It goes from losing to the doctrine-free stack to beating it **everywhere by 2.7x to 8.9x**:

| clients per occupied cell | 1.5 | 1.6 | 1.7 | 2.9 | 5.9 | 11.3 | 18.3 | 24.4 | 30.3 |
|---|---|---|---|---|---|---|---|---|---|
| handicapped fan-out vs best alternative | 1.14x | 1.13x | 1.12x | 0.94x | 0.87x | 0.67x | 0.70x | 0.45x | 0.31x |
| **fair fan-out vs best alternative** | **0.35x** | **0.36x** | **0.37x** | 0.32x | 0.29x | 0.25x | 0.30x | 0.18x | **0.11x** |

The first row produced a precise and wrong finding: *the crossover is a little under three clients per occupied cell, so the two schemes are for different worlds*. There is no crossover. The fan-out wins at every density measured and by 6.1x to 14.6x against what ships. The unfair comparison gave a sharp wrong answer. A clean monotonic curve with a crossover on it looked like a real law, which is what made it convincing.

What still holds: **population at constant density changes nothing**: 256, 1024 and 4096 clients sit at 2.69x, 2.77x and 2.88x, a 16x range that does not move the verdict. **Occupancy governs the size of the win rather than its direction**, running 2.7x sparse to 8.9x packed, because a per-client scheme pays per client and a per-cell scheme pays per cell. And the fan-out's advantage is mostly that it never copies payload bytes into a per-client buffer at all: its encode is only 1.17x better than `joined + flat`'s and its build is 6.7x better.

The practical reading is that the two are for different worlds rather than in competition. gow_3d as played is 64 characters spread over its map, well under the crossover, so the doctrine-free stack is right for it. A city, a raid or an auction house, which is the case per-cell publication exists for at all, is over the crossover immediately. If a technique is measured only on a case it was not built for, it gets rejected for the case it was built for.

**The first run of that table said 9.4x for the fan-out, because it was not charged for answering "who gets this".** A view query produces client-to-cells and `MessageTarget::Agents` needs cell-to-clients, so the scheme must invert the mapping: the same 49-per-client walk the copying arms spend on copying. Leaving it out dropped that arm's build from 1078µs to 58µs. **Sharing moves work rather than removing it and the moved work lands in the bookkeeping**, which is where a harness written by the scheme's author tends not to look. The clue was that a per-client delivery scheme reported a build cost that did not scale with clients at all.

**The flat index was first rejected on a measurement taken in the wrong place.** The obvious thing to time is the *bucketing*, where the grid is most visibly used and there it wins 5.19x on 18µs of a 2578µs tick, which looks like a rounding error and was written up as a rejection. The path that actually matters is the *lookup*: about 49 cells per client per tick, 50k hashes at 1024 clients and moving those to array indexes is **1.39x on the whole tick** (1303µs to 940µs). The same swap measured in two places differs by three orders of magnitude in significance. **Measure an optimisation where the work is rather than where the structure is most visibly used.** Be most suspicious of a rejection, because it closes the question and nobody looks again.

**Caching the cell window is cheap and gains almost nothing.** A client crossing a 15-unit cell at running speed makes its window stale on **6.8%** of ticks, so 93% are reuses, which sounds like the whole walk disappearing. It is worth **1.04x**, because computing which 49 cells is cheap and *using* them is not: the payloads change every tick, so the lookups and copies happen whether the window was cached or not. Caching the cheap half of a loop only saves that half.

**A payload keyed by a place can describe positions relative to that place** and this is the only saving found here that a per-client frame could not have had. Built and measured, it is **9% of the bytes when packed and nothing at all spread out**, against a harness that predicted 10-12% everywhere. Both halves of the shortfall are the same mistake: the harness priced the *bodies* and not the *scheme*. **A payload written relative to a cell has to name the cell** and that index is a varint per cell against ten bits saved per body, so it breaks even near one body per cell and gow's own density is 1.6. And it is ten bits rather than twelve, because a body may sit slightly outside its own cell so the range needs padding and **12 bits over the padded range is coarser than the 18-bit absolute layout it replaces**. A const assertion caught that, after the doc comment above it had already claimed the opposite in prose. A saving measured on the payload without the envelope that carries it is an upper bound, as the delivery ratio was an upper bound on the pipeline. Both were found in one afternoon.

**The harness now round-trips every packing arm.** Every packing arm reads back what it wrote, using the shipped writer and the shipped reader rather than a local copy of each. An arm that must decode its own output cannot omit what the decoder needs: with the round trip in place, a missing cell index becomes a payload that cannot be decoded rather than a subtle bias. Re-run under that rule, the harness predicts **0.97-0.99x spread and 0.90-0.91x packed**, against a wire that measured **0.98-1.00x and 0.91x**. It now agrees to about a point, where before it was out by the whole of the win. The timing arms cannot round-trip, so for them: state what fraction of the tick the stage is and read every ratio as "at best".

**Four costs came from one mistake.** Every cost in the layer between "publish per cell" and "bytes reach a client" was **keyed by viewer when the information varies only by cell**. Two viewers standing in the same cell touch the same 49 cells, are owed byte-identical bodies, belong to the same audience lists and read every cell in the window at the same width. Nothing there distinguishes them, yet the window walk, the assembly copy, the audience push and the graded distance test were all run per viewer. Re-keying the layer by the viewer's *cell* fixed them together: `Packed` became refcounted so handing one blob to twenty viewers is twenty refcount bumps, the blob is assembled once per occupied viewer-cell and addressing walks cell pairs against a fixed offset mask instead of measuring a distance per listener. Measured: `joined` at 4096 spread **6141µs to 4724µs** and packed **301µs to 166µs**; `cells/grad`, the worst case, **6326µs to 3615µs**. The gain tracks clients per cell, because that is the redundancy factor: 1.37x on build spread, 2.6x packed. **When the same cost appears in four places, look for the key they share rather than fixing each call site.**

**Graded precision was then built and its bookkeeping cost more than it saved.** Grading the width by distance saves a further 3-5 points of bytes over cell-relative (0.94-0.97x spread, 0.87-0.91x packed, against absolute). But **a width cannot be chosen per viewer when the payload is shared** (the same constraint that took the relevance tag off the wire), so the zone publishes both widths and each viewer takes the one its distance earns. Under `Joined` that costs about 3% of the tick for 4-6% of the bytes, which is roughly a wash. Under `Cells` it **doubles the tick** (3101µs to 6284µs at 4096), because addressing now has to partition each cell's listeners into near and far, emit two ops instead of one and compute a distance per listener per cell. The bytes it saves are 3%. Every scheme that shares a payload pays for the metadata that makes it usable. That has now happened four times: the fan-out's recipient lists, the cell index, the graded width tag and the audience partition. Each time the fix was to measure the bookkeeping as well as the idea.

**A shared payload has no per-viewer distance check, so the index alone decides what is sent.** The per-client build re-checked every candidate with `distance(...) <= VIEW` and a filter of that shape cannot exist on bytes shared between viewers: the payload is fixed before anyone's position is consulted. With a correct index that costs only the bounded cell-granular superset already measured. With an index that clamps, the cost is unbounded. Before, a spatial index smaller than its world only wasted query effort, visible in a waste counter. Cell publication ships whole cells, so whatever the quantizer clamped into its boundary cells goes on the wire. gow_3d's own scale harness had exactly this: at constant density the spawn spiral reaches `7·√n` and passes the index's 120-unit origin between 256 and 1024 characters, so at 4096 **56% of the population sat in the border cells and the fullest held 490 bodies against three in a correct one**, inflating every frame 6x. The clue was a column moving that should not move: bytes per client grew 6x while the in-view count stayed pinned at 44. **With shared payloads a bad index sends wrong data, where before it only made the zone slow.** Know that before moving from per-viewer filtering to shared payloads.

**gow_3d ships this now and adopting it found problems the harness could not.** `Zone::publish` packs each occupied cell once (on `SpatialGrid::occupied` and `GridQuantizer::keys_in_radius`, the two exposures the decomposition added to `plaza_server_utils`) and `frame_for` became an assembly: the touched payloads, plus `you`, the party's extras and the landings this viewer can see. It had three consequences a per-client frame never had. **The per-entry relevance tag came off the wire**, because a payload shared by every viewer of a cell cannot say why any one of them is being told; `Because` is now stamped at decode from which channel carried the entry, with a small `party` seat list on the frame upgrading a near member to `BothOfThose`. **The subscription channel's job became narrower**: extras carry exactly the party members with no payload in the touched cells, whether out of view or a corpse the world has let go of, so the two channels are disjoint by construction. **And "out of view" moved**: a body can now be described up to a cell width past the radius, which one mirror test caught because its walk left the 46-unit disc but not the 61-unit cell window and the fix was to walk farther, not to narrow the cells.

**Rest detection is a loss here.** Sending only what moved cost **2x the build** and saved **0.2% of the bytes**: 514 against 515. Nothing is at rest, because everybody in a zone is walking. cube_yard's 8-bits-against-82 is real and came from a yard where 901 of 905 cubes were asleep; a settled physics scene and a populated zone are opposite cases. The per-viewer memory a change-gated stream needs is a real cost paid every tick and it buys nothing when the change rate is 100%. **Check the change rate before using a change-gated channel**, just as you check a stream's share of the packet before optimising its encoding.

**Grading the refresh rate by distance halves the bytes**, 0.44x on the spread zone and 0.61x packed and cuts the build by 40% as well since a character not sent is not packed. It is the only lossy arm, so it carries the pixel column: 29.5 px of staleness worst case, from a body walking `RUN_SPEED` for the three ticks it waits. That figure is deliberately pessimistic, since it assumes the client draws the last position it was given and gow_3d interpolates. The real cost needs the interpolation modelled, which has not been measured yet.

**A figure this file quoted measured the length of a variant name.** The README priced the subscription channel at "6 bytes per party member the distance query missed", from a test that walked four members out of view and into a party in one step. The audience count never changed: four entries left the near channel and the same four arrived on the subscribed one. What moved was that MessagePack spells `Because` as its variant name and `"Subscribed"` is six characters longer than `"Near"`. Packed, the tag is two bits, the difference went to zero and the test failed for the first time. The correct figure is **13 bytes**, from moving the members out of view *before* the baseline. If the two arms of a measurement differ in more than one way, it can measure a difference you did not intend.

## Lessons from building the blocks

These came out of the extraction rather than the debugging. Two of them appeared twice, in different types.

**Cold start is a distinct state that needs its own answer.** `CorrectionMonitor` alarmed loudest at startup, because a baseline initialised to zero says every correction is enormous. `DeltaBaseline` had the naive bug hiding inside its own recovery mode, because recovery diffs against the acknowledged state and there is no acknowledged state before the first acknowledgement, so it silently fell back to the very behaviour it existed to replace. In both cases a mechanism defined in terms of accumulated state had undefined behaviour before that state existed and the accidental fallback was the naive behaviour. For anything that learns, check what it does on sample zero and whether that was decided on purpose.

**Attach an integrity check to the report, not to the state change.** The digest comparison originally ran only when an acknowledgement advanced the frontier. A client re-acknowledging the same sequence still reports what it is holding and a mirror that loses something *without* losing a packet reports it exactly then, so the check skipped the one case it was for.

**A richer key can delete an entire side channel.** Horde carried an out-of-band death announcement with a long comment about why it was unavoidable. It was unavoidable only because the diff was keyed by index. Once the diff moved into `(index, generation)` space, a dead slot retracts the occupant the client was told about and a reused slot reads as despawn-then-spawn on its own and the whole mechanism was deleted.

**Take a function or a value rather than adding a trait bound.** Three places wanted a distance and none of them added a bound: `reconcile` returns two states and lets the caller subtract, `with_teleport` takes the metric as an argument and `DeltaBaseline` takes a digest as a `u64`. A trait bound would cost every user something for the few who want telemetry.

**Derives are part of the API contract.** `LatencyLink` was not `Clone` and that alone is why horde reimplemented it, which is how a transport-faithfulness fix ended up living in one example instead of the library. A primitive that cannot sit inside application state will be reimplemented and a plaza state must be `Clone`.

**Retrofit each block to a second consumer and find every other copy.** Every block here was written against horde and retrofitted to black hole (or the reverse) and each retrofit found something: a divergence in what the two meant by a "fresh" seat, a rate that divided by zero on the first frame and one impairment queue that had simply never received a fix the other one had. After extracting, find the other copy, because the unmigrated copy still carries the bug.

**Name tests after the bug, not the function.** The horde retrofit rewrote the most-debugged code in the repo and shipped on a single test run, because six tests are named for actual historical failures. A test named for the function tells you something broke. A test named for the bug tells you which regression came back.

## Deployment failures

The browser client is a wasm bundle, a gitignored build product and it does **not** rebuild when the server does. A page built before a wire change still loads, still appears to run and only the messages whose shape changed are rejected. That looks like a netcode bug but is a deployment problem and it cost two rounds of diagnosis.

Three things now prevent it and each covers a case the others cannot:

- The host serves `index.html` dynamically with the wasm's modification time stamped into the URL, read per request so rebuilding the client reaches an already running host without restarting it.
- Static assets are served `no-cache`, which is what makes the stamp effective: a cached page would keep quoting the old stamp, which is the trap that makes cache busting look like it does not work.
- A client announces its wire format in `Op::Hello` on connect and a server that speaks a different one replies `Op::Outdated` so the page can say "reload" instead of the server flooding its log with per-message decode warnings. The version is not a constant anyone maintains: `build.rs` hashes the source files that define the messages, so the server and the bundle agree exactly when they were built from the same code. A hand-bumped version tends to be forgotten in exactly the change that needed it.

The handshake sometimes asks for a reload that was not strictly needed, since the hash changes whenever those files change, comments included. That errs in the safe direction: the cost is a page load, while the opposite mistake is the silently half-working session the mechanism exists to prevent. It also cannot help a client older than the handshake itself, which is true of every protocol version check.

## `mean_render_error` and the two send rates

A late finding, from playing rather than from a test. At a 1 Hz send rate the horde *looked* like 1 Hz, which contradicted both the case study and this project's own measurement that running the enemy rule locally beats interpolating at low rates by a wide margin (12 px mean error against 57 px).

Both were right about what they measured, which was not the problem. `mean_render_error` compares an enemy's position against server truth and that number is genuinely good, because every client runs the enemies' own rule. It cannot see **continuity** and it does not cover **players** at all.

Two things were actually running at 1 Hz. Remote players had no smoothing whatsoever, a bare `self.players[p] = pos` per packet, so peers teleported once a second. Worse, `step_enemy` aims at `players[target]`, so the whole horde was gliding smoothly toward a point that jumped once a second: at `PLAYER_SPEED` that point is up to 190 px stale, nearly half a view radius, while a Swarm enemy covers only 62 px in the same second. A *synchronised* heading change across hundreds of entities is far more visible than the same magnitude of error scattered randomly and a positional mean cannot express it at all.

**The fix is two send rates.** Enemy positions may be stale, because they are the behaviour's *output* and every client recomputes it. Player positions may not, because they are the behaviour's *input*. This is the case study's own principle, which was recorded here and then not applied: sync the input to the behaviour, not just its output. Their 1 Hz was enemies only; players are a handful of entities and cheap to send often. Horde had a single global `sync_hz` that starved both together.

Measured on the fixed build: with one shared 1 Hz rate a peer is up to **204 px** behind the truth, matching the predicted 190; splitting the rates (1 Hz entities, 30 Hz players) takes it to **7 px** and the entity stream is untouched.

Remote players are now drawn through `RemoteView`, which is what that block is for, so a peer is interpolated between samples with dead reckoning on starvation. It is kept separate from the authoritative array the *rules* read: interpolation is presentation and the rules keep consuming the authoritative sample, which is the principle that stopped the horde lunging in the first place.

This is the sixth time in this project a metric measured the wrong thing. **Error and smoothness are different measurements and averaging position error hides every discontinuity.** If something looks wrong and the numbers look right, the numbers probably measure something other than what the eye sees.

### The first attempt at the fix made it worse

Three separate defects produced one symptom and the log line that exposed them was nearly deleted for being noisy.

**Interleaving two streams into one position.** Players now arrive on both the entity packet and the player frame and those travel the same delayed link at different rates, so a packet built earlier can arrive *after* a newer player frame. Taking it anyway walked the authoritative position backwards in time and that position is what the enemy rule reads. Samples older than the newest for that player are now rejected.

**A velocity derived from whatever two samples happened to be adjacent.** Across two interleaved streams the gap between samples can be a millisecond or two and a small position difference over a smaller time is a spike. The view then dead reckoned along it. A minimum gap before a velocity is recomputed fixed it.

**A render target computed the wrong way, which is the most general of the three.** The target was `now_ms - one send interval`, where `now_ms` is an estimate of server time *now*. A sample is a link delay old by the time it arrives, so that target sits permanently *ahead* of the newest snapshot and the view never interpolates at all: it extrapolates, hits its cap and holds. On a host it is worse, because pongs return instantly while frames go through the impairment link, so the two disagree by the whole latency. The fix is [`InterpolationClock::resync`], which steers the render clock toward the stream, so the target trails the newest sample without knowing the latency, the jitter or how good the clock sync is.

**The library bug underneath, found by the person playing it rather than by a test.** Past the extrapolation cap, `ExtrapolationBase` returned the *un-extrapolated* state. At the cap an entity has coasted `velocity * max_ms` forward; one millisecond later it was drawn back at the raw sample, a jump of the entire window in the wrong direction, flickering whenever a jittery target crossed the boundary. It now caps the *duration* instead, so the entity coasts to the limit and stops there. **Two tests asserted the old behaviour**, which is the tcp double-encoding lesson again: a test written from the implementation pins the bug rather than the requirement.

**Plaza already had the right technique.** Gambetta's entity interpolation, which plaza implements and `netcode_playground` demonstrates, is *render in the past between two real snapshots and do not extrapolate*. Peers were being rendered with `RenderOpts::default()`, which has `extrapolate: true`. Dead reckoning a **player** is guessing at a human's intention, which nothing on the wire carries, so it overshoots every direction change and snaps back when the truth lands. Peers now interpolate only, trailing by two send intervals so two snapshots always bracket the target.

**Rendering in the past requires a history, not just the newest snapshot.** Putting every remote entity on one delayed timeline is right and it exposed which entities can actually *be* on it. A peer can, because `RemoteView` keeps a snapshot buffer to interpolate within. A projectile could not: the client held only the newest list and replaced it wholesale each packet, so once the server stopped listing a shot (it hit something) there was nothing left to draw it from. Any shot fired and destroyed inside the render delay had therefore never existed at the target and was silently dropped. Measured at a 4 Hz player rate, that was **every** shot: none were drawn at all.

So **an entity can join a delayed timeline only if the client can reconstruct its state at an arbitrary past instant.** That means either buffering its samples or holding enough to compute it.

For a shot the second is nearly free and it is what the wire now carries. A `ProjectileSpawn` is an origin, a velocity and the time it was fired, sent **once** as an event; the client flies it locally and can evaluate it at any instant exactly. That put shots back on the shared timeline and the measurement is clear: the number of shots actually drawn went from 0.2, 0.1 and **0.0** per frame at 30, 16 and 4 Hz to essentially rate-independent. The *held* count still rises as the delay grows, because a shot fired after the instant being rendered is queued until the timeline reaches it, which is the buffer working rather than a loss.

This is the same approach as elsewhere in this file: send the input to the behaviour rather than the behaviour's output. Re-sending a live projectile's position every packet sends the output of an equation both sides can solve. It is cheaper too, since a shot costs one message instead of one entry in every packet for its whole flight.

**What extrapolation is for.** It is the *starvation* fallback: when the render target runs past the newest snapshot there is nothing to interpolate between and the only choices are freeze or coast. It is not a general technique and whether coasting helps depends on the **entity** rather than the game. It works when the next state follows from the current one, which is true of vehicles, projectiles and anything with inertia and a turning limit and is where the term comes from (military simulation, where every entity is a vehicle). It fails for anything steered instantaneously by a person or an AI, because there the velocity only records how the entity was moving and a person or an AI can change it at any time.

That gives four options in order of preference and plaza has all four: run the entity's own rule if you know it and know its inputs (best and what makes a 1 Hz enemy stream playable); interpolate between two real snapshots if you do not (safest and what peers now do); extrapolate from one snapshot and a velocity only if the dynamics are predictable; hold if they are not. Move down the list only when the option above has no data.

**The extrapolation warning stays at `warn`.** It was briefly downgraded to `trace` for being repetitive. It was the only thing announcing all of the above. It is back at `warn` and now says what a *steady* occurrence usually means, which is a render target ahead of the stream rather than a starved link.

**Where it ended up.** Putting peers on the delayed timeline was only a third of it, because everything else (deaths, sparks, claims, health) still applied on arrival, so the scene was being drawn from several clocks at once. The client now queues packets and applies them when its render clock reaches them and a token type makes drawing at any other instant inexpressible. Inputs went the other way at the same time and for the same reason: the server buffers them by tick and executes in tick order rather than on arrival. Both come from the same idea: **one side owns the clock and everything else is scheduled against it**. The two principles above are that idea split in two.

[`InterpolationClock::resync`]: https://docs.rs/plaza_client_utils

## What is deliberately not predicted and why

Listed so nobody "fixes" these:

- **Black hole, collision separation between holes.** Predicting it requires predicting the other holes' motion, which means running the whole field forward rather than one entity. Left as a correction and it is the residual visible during close grapples.
- **Black hole, the dash, when the toggle is off.** Kept selectable so the cost of not predicting an ability can be seen.
- **Horde, the local player, entirely.** Nothing about it is predicted any more. It is drawn from the played-out stream at the same instant as everything else, because against a scheduled server a prediction simulates a different world. Black hole still predicts its own hole, correctly: its server applies input on arrival, so there is no schedule to disagree with.
- **Both, remote players.** They are interpolated or simulated under the shared rule, not predicted, because there are no local inputs to predict from.

## What the host sees that a joiner does not

The host is the server, so its full readouts are legitimate: the truth overlay, bandwidth accounting, digest mismatches, phantom counts. A joiner sees only what a real client can see. The impairment sliders act on real outbound connections so the host can show a joiner what two hundred milliseconds feels like. The host deliberately keeps feeling its own impairment settings, so the effect is symmetric and comparable.
