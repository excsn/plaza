# Learnings from the playgrounds

Everything the black hole and horde playgrounds taught while they became real listen-servers: the principles that prevent whole classes of bug, the record of what actually broke and how each was found, and what all of it changed in plaza itself.

It is kept because in almost every case the cause was somewhere other than where the symptom pointed, and the wrong theories were reasonable enough to be worth writing down next to the right one.

The principles come first because they are free. No primitive prevents the bugs below; only writing the code differently does.

## Principles

### The four everything else is an instance of

Every detailed principle below is one of these caught at a specific site, and every bug in the catalogue is a violation of exactly one. They are short on purpose: a violation is easiest to spot against a list you can hold in your head, and each of these was learned by paying for it.

1. **Reproducibility.** The same inputs at the same ticks produce the same state everywhere. Concretely: an input is keyed to a tick derived from the shared clock, never from its arrival time; inputs execute in tick order; the rule that consumes them is one function both sides call, not two implementations of the same idea; and **both sides advance that rule in the same quantum**, so a step is a whole tick on the authority and on every client. The test is a recording: it must replay to what every player saw, including their own screen.

   The last clause is the one `bomb_grid` had to pay for, four times over, and it is the least obvious: a shared rule and tick-addressed inputs still reproduce nothing if the *stepping* is not shared. See [Four bugs with one shape](#four-bugs-with-one-shape-bomb-grid).

2. **One instant per frame.** The client picks a single instant T and *everything* in the frame is evaluated at T: not only where entities are drawn, but everything a behaviour rule reads while producing the frame, aim targets and chase context and force fields included. An entity standing at T while reading a target from the newest packet is two timelines in one scene, and the seam between them is a bug whether or not it is visible yet.

3. **Simulation never reads presentation.** Smoothed, blended, faded or predicted state is output only. The moment a rule both sides run consumes it, there is a second divergent world and every packet fights the local one. The dependency points one way: presentation reads simulation, never back.

4. **The timeline comes from declaration, not arrival.** Transport facts (round trips, jitter, arrival times) may size buffers and admit or refuse connections. They never decide which instant is drawn or when an input executes; those are declared numbers, chosen and published by the server. The moment arrival time leaks into the timeline, principle 1 dies silently, and a mechanism that adapts to a fault has destroyed the evidence of it.

### A shared rule must be shared code, not code written twice

This is the strongest correlation in either example, and it is close to a controlled experiment because both examples contain both outcomes.

| entity | where the movement rule lives | divergence bugs |
|---|---|---|
| horde enemies | `step_enemy` in `sim/types.rs`, called by server, client and world | none |
| black hole pellets | `step_pellet` in `sim/types.rs`, called by both sides | none |
| black hole **hole** | server ran `step` plus `attract_holes` plus collision separation; the client had its own `apply_move` | three, found separately: the unpredicted pull, the frozen dead hole, the unpredicted dash |
| horde **local player** | server integrates a held direction; the client hand-rolled velocity integration | three: a threshold-snap sawtooth, enemies lunging at a predicted position, and reversal stiffness. No longer predicted at all |

Every entity whose simulation step was one function both sides call was correct and stayed correct while the game changed around it. Every entity whose step was written twice drifted, silently, and each drift cost days to find because the symptom (a jerk, a jump) was far from the cause (a force the client did not model).

The practice: for any predicted entity, the step is a single shared pure function, and the client calls it rather than reimplementing a subset of it. `PredictedPlayer::apply` is meant to *be* the server's step function. Where sharing is genuinely impossible (different fidelity, server-only information), that is a deliberate exception with a known error budget, written down, not an accident of two people writing similar code months apart.

The corollary that makes this practical: if the client's step needs the world (gravity, wind, platforms) and your prediction API cannot pass the world in, you will be pushed into writing a second, lesser rule. That is an API deficiency, and it is why `PredictedPlayer` gains a prediction context below.

### Predicted state is for presentation; shared rules consume authoritative state

In horde, feeding the locally predicted player position into the client's simulation made enemies chase a point the server was not chasing, so every packet snapped them back and they appeared to lunge whenever the player moved. The fix was to let the prediction drive only the camera and the player's own marker, while the shared rule kept reading the authoritative position, exactly as the offline build does.

Generalised: prediction is a rendering concern for one entity. The moment predicted state feeds a rule that both sides run, there are two divergent worlds and the packets fight each other. This is subtle because using the freshest local data looks like an improvement, and it is the exact mirror of the principle above: share the rule, but do not share the prediction into it.

The same distinction shows up in rendering. The repulsor ring was drawn at the authoritative position while the player marker was drawn at the predicted one, so the ring visibly lagged and stuttered on packet arrival. Which position each element is drawn at is a deliberate choice: effects belonging to the local player follow the prediction, effects describing shared physics follow the authoritative state.

### Never let a correction accumulate to a threshold and snap

Horde's local player pulled toward the server only once the error passed a fixed threshold, then closed the whole gap at once. Holding a direction produced a metronomic drift, snap, drift, snap, which the player feels as a rhythmic tug forward. Continuous easing of a fraction of the error every packet absorbs the same drift invisibly.

A hard snap is correct only for a genuine discontinuity: a spawn, a respawn, a teleport. Those must not be eased, because easing a two thousand pixel jump draws the player smoothly across the arena. So there are exactly two behaviours, chosen by cause and not by magnitude: ease continuous error, snap discontinuities.

### An integrity check that only detects is half a tool

The relevance stream carried a digest, so divergence was detected immediately and honestly. It still took days to find, because a digest says *that* the mirror is wrong and never *how*. Shipping the server's actual key set behind a debug switch turned the counter into a diff, and the cause was obvious in a single run: every missing key was generation zero and spread across the whole slot range, which is the signature of a client that joined an arena already in progress, not of a drift.

So: pair any checksum with an opt-in mode that ships the ground truth it is a checksum of. The cost is a switch and some bandwidth while it is on, and it converts an unactionable number into a diagnosis. The same lesson applies to what a diagnostic prints: the first version of the correction log reported the server's dash flag, which reads true through a grapple because dashing is how a grapple is fought, so it implied a cause that was pure coincidence. Adding the distance to the nearest hole made the real story readable at a glance.

### Diagnostic thresholds must adapt, because there is no fixed normal

A correction of thirty pixels is unremarkable at one send rate and alarming at another, and the same is true across latency settings and across how much contact the game is currently in. A fixed pixel threshold reports whatever the constant happened to be tuned against. Tracking a running mean and variance of the quantity and reporting deviations from it is barely more code and stays meaningful when conditions change under it.

### Impairment tooling must be faithful to the transport it stands in for

A full diagnostic cycle went into a reordering hypothesis. WebSocket is TCP and cannot reorder, but the impairment link could, which made a phantom failure mode credible enough to chase. Chaos tooling should be able to produce only the failures the real transport can produce: delay and loss for an ordered transport, reordering only for a datagram one. Otherwise it manufactures red herrings, and worse, it hides the fact that the real system has stronger guarantees than the tests assume.

### A listen-server host has asymmetric internal latency

On a host, round trip probes return directly while frames pass through the outbound impairment path. Feeding both into one clock estimator made the estimate wobble every probe interval and jerk the world. Any estimator, whether of clock offset or round trip time or jitter, must be fed samples from a single path with consistent delay characteristics. A listen-server host is simultaneously the most and least privileged client in the session, and averaging across that asymmetry produces a number describing neither.

### The server owns time, and a client names it rather than asserting it

Applying an input on arrival makes ping an input to the game: a 20 ms player's press lands on the next tick and a 200 ms player's lands nine ticks later, so anything decided by who was where first is decided by connection quality. Buffering inputs and executing them at the moment they were *pressed*, plus a fixed playout delay, puts everyone on the same footing as long as the delay covers their latency. It is not free: it is added to how long the world takes to react to you. Prediction was expected to hide that for your own movement and cannot, which is a separate principle below.

**A tick, not a timestamp**, and the difference is authority. A timestamp is the client naming a moment, which the server then has to judge plausible; judging it needs a shared clock, a shared clock is an estimate, and the estimate's error is the slack a liar hides in. A tick is the client naming *the server's own unit of time*, which is either still open or is not. Both sides compute it from one rule, so two players who pressed at the same instant name the same tick however far apart their pings are, and a 120 Hz client and a 30 Hz one are on equal footing because neither is naming its own frames.

**Reject, do not clamp.** Outside the accepting window an input is dropped. Clamping a wild tick into range executes an input the client never asked for at that moment, which is worse than dropping it and is indistinguishable from a working system from the outside. Sequence numbers cover the other half, which is replaying an input that was legitimate when it was sent.

**Both bounds are settings, because they are genre decisions.** Tight lateness is what a competitive shooter wants: a closed tick stays closed, and a player who cannot reach the window loses inputs and rubber-bands. Loose forgives a jittery link at the cost of letting a slightly stale input take effect, and widening it is also exactly what a lag switch wants, so it should be sized from what honest links actually measure rather than picked. The earliness bound has to cover the playout depth, since that is how far ahead an honest client aims.

### A scheduled server and a predicted local player cannot both be right

Prediction hides your own latency by applying your input at once, on the understanding that the server is about to do the same. Against a server that executes inputs **on a schedule**, it isn't: your input runs at `press + playout_delay`, and until then the prediction is simulating a world the authority is not in.

The disagreement does not heal. Measured with the correction switched off, at zero latency, a single reversal banks a permanent **44 px** offset. The *velocity* error is temporary, since both sides end up holding the same direction, but the displacement it already integrated has no restoring force. Something has to give it back, and giving it back is the correction being dragged against the direction you are steering, which is what a hand reads as stiffness.

Three fixes were measured and all were worse than the fourth: predicting the schedule costs the same lag honestly, aiming at your own delay restores the ping advantage the schedule existed to remove, and replaying only makes the correction exact rather than absent.

**The fourth is to stop predicting.** If the client already draws the world at a delayed instant, the local player can be drawn there too, from the played-out stream, like every other entity. Prediction and authority cannot disagree when there is no prediction. The cost is `playout_delay + render_delay` of input lag, and it is not new: both delays were already being paid, one in authority and one in rendering, and the client was drawing something else in between.

The general form, and it is this project's first principle from a new angle: **prediction is only sound when it predicts the same thing the authority will do, including *when*.** A predictor that has the rule right and the timing wrong is not an approximation of the server, it is a different world.

Worth noting what it buys beyond the fix: a recording replays to exactly what every player saw, *including their own screen*. A predicted local player can never give you that.

### Transport variance and the timeline are different things, and one must not move the other

Latency and jitter describe **when bytes show up**. The render delay describes **which moment is on screen**. The first is a property of one link; the second is a property of the world.

Horde sized its render delay from measured arrival jitter, recomputed continuously, which lets the transport decide what moment a client is displaying. Three things follow, and none is visible from the code:

- **No two clients agree on "now."** Each picks its own T, so there is no shared instant to reason about.
- **The server cannot say what a client has yet to play**, which makes any question about unresolved state unanswerable.
- **A bad link is hidden rather than reported.** The buffer widens until late packets fit, so that player is quietly shown an older world than everyone else and every readout says fine.

Fixing T at `server_now - declared_delay` makes the delay a chosen number and turns the third into an **underrun counter**: a packet that arrives after the instant it describes has already passed is counted, not absorbed. The general form is that **a mechanism that silently compensates for a fault destroys the evidence of it**, and this project keeps finding that in new places.

Two consequences worth knowing before adopting it. The declared delay must cover `one_way + jitter + one send interval`, because the newest sample a client holds is already a trip old, and the old design hid that term by steering its clock to the packet's timestamp rather than to server-now. And it becomes a fairness property: one timeline means everyone waits for the slowest player, exactly as the input playout buffer does in the other direction.

### A limit that is enforced by accident excludes people silently

Horde's input schedule always had a ceiling: past `playout_delay + late_window` of one-way delay, every input a player sends lands outside the accepting window and is dropped. Nothing declared that, nothing measured it, and nothing told anybody. A player above the line was welcomed, seated, and simply could not move.

The failure mode is the point. An accidental limit does not refuse you, it admits you and then breaks, which reads as a broken game rather than an unsuitable connection. And it was invisible from inside: the server counted rejections, the client counted nothing, and the screen showed a player who would not respond.

Two things fix it and they are different. **Derive the limit from the thing that actually enforces it** rather than restating it as a constant, so it cannot drift out of step. And **check it at the door**, with the server timing its own probes, because a client reporting its own latency can understate it and this is the check that decides entry.

Worth noting which direction the spoofing runs. Server-measured admission can only be gamed by making yourself look worse. The alternative that was considered, raising the schedule depth to suit the slowest player, can be gamed by claiming a bad connection to slow the whole arena, which is why it lost.

### Rendering in the past means there is a moment when there is nothing to render

A client whose whole scene is drawn at `server_now - delay` has nothing at all until its timeline has started and a frame has been played out of it, which is one render delay after the first packet at the earliest. Drawing anyway does not produce an empty screen, it produces a **wrong** one: entities at the origin, a camera on the corner of the arena, and then everything arriving at once when the first frame lands.

Every game that renders in the past holds a screen over that gap and fades in. It is not a cosmetic nicety; it is the only honest thing to show, because the alternative is a picture of a world that does not exist yet.

Two details worth having. The fade is **one full-screen overlay** rather than an alpha threaded through every draw call, so it masks uniformly and cannot be forgotten by whoever adds the next entity type. And it goes over the world but **under the panel**, since a readout that faded with the world would be hiding the numbers that say why the world is not there.

The related trap, found by looking: whatever a camera follows before the first packet needs a **sensible default**, not `Default::default()`. Horde's fallback was the origin, which in a world measured from one corner is a view of the outside of it. That regressed silently when the local player stopped being predicted, because the predictor had been seeded at the arena centre and the array that replaced it was zeroed.

### An entity can join a delayed timeline only if its state is reconstructable at an arbitrary past instant

Rendering remote state in the past is right, and it is a property of the whole scene rather than of one entity. Horde had three clocks in one picture (enemies at now, peers in the past, projectiles elsewhere) and every seam between them was a visible bug. Unifying them meant queueing packets on receipt and applying them when the render clock reaches them, so a frame is one consistent instant, and making any other instant inexpressible with a token type rather than merely discouraged.

Doing that exposes which entities can be on the timeline at all. A peer can, because a snapshot buffer keeps its history. A projectile could not: the client held only the newest list and replaced it wholesale each packet, so once the server stopped listing a shot there was nothing left to draw it from, and any shot fired and destroyed inside the render delay had never existed at the target. Measured at a 4 Hz player rate, that was every shot: none were drawn at all.

The fix is the same move that has worked everywhere else here, one level down: **send the input to the behaviour, not the behaviour's output.** A shot became an origin, a velocity and a fire time sent once as an event, so the client can evaluate it at any instant exactly. Shots drawn per frame went from 0.2, 0.1 and 0.0 at 30, 16 and 4 Hz to essentially rate-independent, and it costs one message instead of an entry in every packet for the whole flight.

**It was then reverted, and this file went on claiming it for months.** The revert had two stated reasons, recorded on the wire type rather than here, which is how the two came to disagree: a client is not told when a shot *ends*, so it flies on through the enemy it killed, and it cannot decide that for itself because it draws shots in the past while its enemy mirror holds the present. The first is real. The second stopped being true the moment [the whole scene moved to one render instant](#an-entity-can-join-a-delayed-timeline-only-if-its-state-is-reconstructable-at-an-arbitrary-past-instant), and nobody revisited it. The event form is now back, with an explicit end event carrying only *early* ends (a hit), because ordinary expiry is derivable from the fire time and a constant both sides already hold.

**The lesson is not about shots.** A decision recorded in two places will disagree, and the copy that is wrong is the one further from the code. The wire type's comment was right for months while this file, the document whose entire purpose is to be trusted, was wrong. Reasons for a revert belong next to the thing reverted, and a claim of a fix belongs where the fix would be visible if it were there.

### Techniques worth reusing

Running two predictors over identical inputs that differ in exactly one variable, and comparing their error, answers "is this prediction earning its keep" in one session instead of two, with no toggling and no memory of how the last run felt. It is only cheap because predictors are pure and hold no resources, which is an argument for keeping them that way.

Reading the shape of a counter rather than its value: a count climbing without bound is systematic, a count that plateaus is transient and self-healing, and a regular sawtooth is a threshold effect. Each shape pointed at a different class of cause, and each time it was right.

## The bug catalogue

### A result and the removal of its audience cannot ride in one batch (poketo)

`poketo` ended a battle the moment it was decided, sending the final state and the op that returns the seat to the overworld together. A client applies a batch in order, so it set the finished battle and cleared it inside one loop, and the win or loss was never drawn for a single frame. From the chair it looked like pressing a key and being dumped back outside with no explanation, and nothing about it looked wrong from the server, where every op was correct and in the right order.

The general form: **an op that says "here is what happened" and an op that says "you are no longer here to see it" must not be sent together**, because the second destroys the audience for the first. It generalises past this game to death screens, round summaries, kick reasons, disconnect causes and any "you have left" that arrives beside the reason. Whether the gap is a delay, an acknowledgement or an input is a design choice; that there has to be a gap is not.

The fix that does not cost anything structurally is to let the state persist and have the client say when it is done with it. Here the decided battle stays in the collection it was already in and the client sends `Dismiss`, so the seat is still in exactly one place, the transcript is still resumable, and a drop mid-result still parks.

This is also a reminder about what tests cannot see. Every reconnection and ordering test was green throughout, because every op was individually correct; what was wrong was that two of them arrived together. Only playing it found it.

### A rule that is not a payload does not belong in a hashed file (poketo)

The resolver hashes what the ops reach, so a type near the wire moves the version whether or not it is on the wire. poketo's terrain function is a pure rule that nothing sends, and it lives in its own `terrain.rs` for that reason: tuning the ground should not invalidate every connected client. See [a protocol hash over the ops misses the types they carry](#a-protocol-hash-over-the-ops-misses-the-types-they-carry-poketo) for the hash itself.

### Rapier's add_force persists until you reset it (cube_yard)

Every "energy from nowhere" symptom in `cube_yard` traced back to one line that was never written: `reset_forces`. Rapier's `add_force` and `add_torque` accumulate across timesteps until cleared, so a field applied every tick is not a force, it is a force that grows without bound. A roll torque capped at 4.6 rad/s reached 46, cubes were flung two hundred units clean off the floor, and the player was thrown across the yard, all from the same cause.

What made it expensive to find is that each symptom had a plausible local explanation, and each of those explanations was individually true. Setting a velocity on a body you are standing on really is a lift. Pushing radially away from something hovering above really does drive the cube beneath it into the floor. A per-tick velocity change really is an acceleration of sixty times what you wrote. Three real bugs were fixed on the way to the one underneath, and none of them was the reason the numbers were absurd.

The signal that should have been read sooner: absurd *magnitudes*. Not a wrong direction or a wrong shape, but 46 against a cap of 7.5 and positions six hundred units below a floor. A quantity that has no business being that large usually means something is being applied repeatedly, not that a coefficient is mistuned. Tuning coefficients is exactly what the first several attempts did.

### Setting a velocity on a body you are standing on is a lift (cube_yard)

A magnet that gathers loose cubes was written the obvious way: cubes within reach get the player's velocity, so they ride along. Jump with it on and the player rises for ever, reaching 58 units in a yard 24 across.

Two independent faults, and the second only became visible because of the first.

The magnet was a positive feedback loop. Cubes underneath inherit the player's upward velocity, push the player higher through contact, and next tick copy the new higher velocity. Setting a velocity on a body that is supporting you is always this. It was also free energy: pulling a pile toward you with no reaction on you creates momentum from nothing. Both go away with a damped spring applied as an impulse, **with the equal and opposite impulse applied back to the player**, which also makes carrying cubes feel heavy, as it should.

That fix alone did not stop the flying, which is what exposed the real one: `grounded` was `vertical speed is small`, and vertical speed is small **at the apex of every jump**. Holding the key therefore launched again at the top of each arc. The magnet had merely kept the player jostled near zero often enough to make it obvious. The fix is to ask the narrow phase what the player is actually touching, and to jump on the rising edge rather than while held.

The narrow-phase version then failed the same way, for a second reason. It asks whether anything is touching below the player's centre, and a cube stuck to the **underside** is exactly that, so a gathered clump became its own launchpad and jump could be held down for ever. The proxy had been made physical and was still a proxy.

The general shape: a cheap proxy for a physical question ("am I on the ground") is fine until something else starts satisfying the proxy for the wrong reason, and the thing that exposes it will look like an unrelated feature. Twice here, and the second time the replacement was the thing that broke.

### One entity is not a sample, and a spline is not bounded (cube_yard)

A Hermite spline through two snapshots, leaving along the velocity recorded at each, beat straight-line interpolation by 484x on a smooth curve and by 2.1x on a falling cube pulled out of the solver. Both numbers went into the docs. Across 500 cubes from the same scene it is **39x worse** than the straight line it replaced.

The mechanism is the part worth keeping: a chord cannot leave the segment between its two samples, and a spline can. Measured here it left that segment on **5% of frames, by up to 2.53 units** on cubes one unit across. Velocity at a sample is a promise about the path to the next one, and a collision breaks that promise after the packet has already gone. On a smooth path the promise holds and the spline is near-exact; in a pile of colliding bodies it is a licence to overshoot.

The overshoot rate carries its own correction. It read **50%** until the comparison was restricted to frames that two samples actually bracket: past the newest sample there is nothing to interpolate toward, so "interpolate" degenerates into "hold" and those frames were scored for both techniques. Excluding them moved the straight line's win from 3.1x to 8.2x and the spline's loss from 13x to 39x, in opposite directions, which is what a comparison contaminated by a degenerate case does to both arms at once.

Two habits. Picking an index is not sampling: the 2.1x figure came from one cube that happened to be falling cleanly, and a second index would have contradicted it before the claim reached three documents. And prefer the bounded technique when you cannot guarantee the assumption the unbounded one rests on, which is the same instinct as the rendering hierarchy preferring real data to inference at every rung.

The example that motivated building the spline is the one that should not use it, and says so.

### A delta needs both ends naming the same baseline, not just having one (cube_yard)

Moving from delta-against-last-sent to delta-against-acknowledged looks like a server-side change: keep what the client confirmed, encode against that instead. It is not. The server ends up measuring from a baseline several frames old while the client decodes against everything it has received since, and those are different reference points, so every delta lands somewhere wrong. The lossless control caught it at 2.0 units where the correct answer is 0.001.

The fix is that a frame has to **name** the state it was measured from, and both ends have to be able to reconstruct that state. Fiedler carries a 5-bit offset identifying the base packet; the same thing here is a per-cube history of `(sequence, value)` on both sides and one shared `view_at(seq)`. The symmetry is the point: two implementations of "what did we agree on" is a disagreement waiting to happen.

Worth pairing with what the measurement then showed. Under a budget, bytes are pinned by the ceiling, so an older baseline cannot cost bandwidth; it costs **cubes per tick**, 333 down to 87. A premium quoted in bandwidth would have been invisible here.

### A harness that models the wrong thing measures it beautifully (client_utils)

Chapter 20 recorded that a fixed-duration ease has a correction rate above which it never finishes. Checking it against the shipped `ErrorSmoother` produced a clean table showing exactly that, and the table was worthless: the harness called `begin_from` every frame while the logical state advanced *smoothly*, which is not a correction at all. It was measuring an ease being told to restart toward a moving target, not a discontinuity being hidden.

The mistake surfaced only because a second model, written to compare against, returned `inf`. `offset += drawn - logical` where `drawn = logical + offset` doubles the offset every step. An obviously broken number in the second column is what exposed the quietly broken numbers in the first.

Rebuilt so a correction is an actual jump in the logical state, the lesson held and gained a crossover: below a correction rate equal to the ease duration, duration-based is fine and slightly better on the mean; above it, worst error goes 2.67 to 15.00 while a rate-based ease goes 2.73 to 11.33. So the guidance was right, the API claim beside it was wrong (`new` takes a duration, not a fraction), and neither would have been settled by the first harness.

Two habits fall out. A measurement that confirms what you expected deserves the same scrutiny as one that does not, because there is no error message for "modelled the wrong scenario". And a second, independent implementation of the same idea is worth writing even when you only want one of them, because disagreement between them is the only cheap signal that either is wrong.

### Quantising both sides destroyed the thing it was meant to help (cube_yard)

Fiedler names quantising the simulation on both sides as the critical trick of state synchronization: the server simulating at a precision it never transmits means the client is always looking at a rounded copy of a truth that has already moved on. Snapping the yard onto the wire's grid each tick took the settled pile from **901 cubes asleep to 0**.

The mechanism is a circle. A resting cube jitters by less than one quantisation step, so it is re-snapped every tick forever; writing a body's position marks it modified; a body that is modified every tick never reaches the solver's sleep threshold. The obvious guard, skipping bodies that are `is_sleeping()`, does not help, because sleeping is precisely the state they can no longer get into.

Keying on **motion** breaks the circle, and the rule left over is the one that was always right: a body that is not moving is not drifting, so there is no divergence for snapping to prevent. With that, the pile settles to 901 asleep exactly as it does untouched, and the two runs end 0.009 units apart on average.

The general shape: a technique that perturbs state to keep two machines agreeing can collide with an optimisation that rewards state for holding still, and the optimisation was worth more here (a sleeping cube skips its velocity entirely). Check what a correction costs the things that were not wrong.

### Discreteness does not make things cheap, it makes apparatus unnecessary (poketo)

A tile world was expected to be a large bandwidth win over a continuous one. Measured, a trainer is **36 bits against 51** for the quantised position every other example in this tree actually sends: 1.4x, which is modest. The 2.9x figure people quote comes from comparing against two raw `f32`s and an angle, and nothing here sends those.

What a tile actually buys is that it is an **index rather than a measurement**. No bounds to outgrow, no quantiser, no precision to argue about, and two machines can compare positions with `==`. cube_yard shipped a bug that cannot exist in a tile world, by widening its floor past the range its quantiser covered and freezing everything that wandered out.

The same correction landed twice more in the same example. Ten times the view radius is **24.5x** the people rather than the 100x its area implies, but nowhere near free. And arriving in a populated zone costs **1.00x** an ordinary frame, so there is no join protocol to write at all: a tile world's steady state is already a complete description of what is in view, where cube_yard needed a whole-world dump because a budgeted client would take seconds to learn the yard packet by packet.

Three predictions about size, three measurements saying the interesting thing is elsewhere. The general shape: when a representation change looks like it should save bytes, measure it against **what you actually send today** rather than against the naive version, and then look at what it lets you delete instead.

### A state has to be repeated to stay true and a transcript does not (poketo)

One game holding a real-time overworld and a turn-based battle, and the difference between them is not a budget. The overworld goes out **every tick**, because a trainer nobody describes stops moving on screen. A battle goes out **only when something happens**, because nothing in it decays: a client with the latest one is completely up to date however long ago it arrived.

That sentence decides more than the send rate. It is why a battle needs no prediction, no interpolation and no relevance, and why all of its difficulty is in delivery instead: an event that is never repeated is one whose loss is permanent. Two things handle that, and neither is a mechanism. A choice **names the turn it is for**, so a resend after a dropped connection names a turn that has resolved and is ignored, which is ordering, deduplication and late arrival in one field. And a **token issued on seating** links a reconnecting client to what it was doing, because a new connection is a new id and nothing in plaza's session layer carries identity across a drop.

It also decides the switch between the two: a seat is in one collection or the other, never a flag on a player. A boolean leaves a body standing in the world while its owner is elsewhere, and every rule has to remember to check it.

### Both halves running is a different test from either half being right (spacemo)

Every test in spacemo drove one side, and the bug that got past all of them lived on the seam: the server was correct, the client was correct about everything it was told, and what neither owned was **what to do about silence**. Missiles are streamed while they exist and simply stop being sent when they end, so a client that never treated absence as an ending kept every one it had ever seen.

The test that catches it runs the logic and a client together for three thousand ticks and compares what the client is **left holding** against what exists. It found the bug it was written for and a second one in the same minute: straight shots that hit something kept flying on the client for the rest of their nominal life, because silence ended a missile and not a bolt. The client held 1513 shots against 235 in the world.

Two things about writing it are worth keeping. The first version kept **a second copy of what the client does**, which is the same pair-of-derivations trap the rest of this file is about, committed inside the test meant to catch it: it would have stayed green while `NetClient` drifted away from it. A scripted socket carrying the server's own ops into the real decode path costs nothing and removes the copy. And the ship half of it originally asserted that the *server* went quiet, which is the half that was never broken; asserting the client lets go is the whole point.

The general shape: when two pieces are each correct and the bug is in what neither of them owns, no test of either can see it, and the question to ask a seam is not "did each side do its job" but "what is one side left holding".

### An optimisation that makes one thing self-terminating leaves the other with no ending (spacemo)

Sending a straight shot once and letting the client carry it forward is worth 17.3x, and it has a second effect nobody planned: the shot now ends **by itself**, because the client counts its life down. A homing shot cannot be treated that way, so it is streamed every frame, so nothing on the client counts anything down for it. And nothing announces the end of one either. It hits, or expires, or loses its target, and simply stops being in the frame.

The result was a volume filling with frozen missiles, each drawn at the last place it was seen, for ever. Found by the person playing it, while I was chasing an unrelated number on the panel: **the report was "the missiles are frozen" and the diagnosis was exactly that.**

The rule the client needed is the one it already had for ships, applied to a thing that looked like it did not need it: **absence is the message**, so treat silence as an ending. Six quiet frames for something streamed every frame.

Two general shapes. An optimisation applied to half a set changes the *lifecycle* of that half, and the untouched half inherits an assumption that no longer holds anywhere else. And a despawn that works is indistinguishable from one that never fires unless something counts it, which is why the panel reports how many went quiet.

There was a matching leak on the server, guessed at correctly from the same symptom: a shot that **expired** freed its allocator slot and a shot that **hit** did not, so the index space climbed for as long as anyone was fighting, toward an id field of twenty bits. Two exits from one collection, one of them tidying up.

### Send the spawn, not the path (spacemo)

Two projectiles, one field apart. A bolt flies straight, so its whole future follows from where it started and how fast; a missile turns after its target, so its path depends on where that target goes next and nobody knows that at launch.

Streaming both costs **20.4 shots a frame against 1.2**, a **17.3x** difference that is entirely paths already implied by their own spawn. Telling a client once and letting it carry the shot forward is not prediction in the reconciliation sense, which is what makes it safe: there is nothing to be wrong about and nothing to correct against. The homing half cannot be treated that way at all, and having both live on a dial is what makes the distinction a measurement instead of an argument.

Two details it forced. A shot needs its remaining life on the wire, or a client told once never learns when to stop drawing it. And the per-client record of what has already been announced has to be pruned against the live set, or a **reused slot** is mistaken for the shot that vacated it.

The general shape: before compressing what you send, ask which of it the receiver could have worked out. The answer is usually "the parts that never change direction", and those are often the numerous ones.

### A comparison can pass with one arm missing (spacemo)

The measurement above first read **83x**, which was wrong in a way the test could not see: the scene lined every ship up across the nose, so nothing was ever inside anyone's lock cone, **no missile ever launched**, and the streamed side was being compared against an empty homing side rather than against a cheaper one.

Strung out along the axis ships actually look down, it is 17.3x. The test now asserts homing shots were genuinely in flight, so the failure cannot recur silently.

Same family as the ratio assertion in the relative-encoding entry below: a comparison that is *arithmetically* fine while one of the two things being compared is absent or degenerate. Neither produced an error, both produced a number, and the number was the wrong shape. When a test contrasts two cases, assert that both cases actually occurred.

### A dropped axis over-returns rather than missing, which is why it survives (spacemo)

`SpatialGrid` is two-dimensional, and every 3D thing built here before spacemo could ignore that, because a yard has a floor and an arena has a plane. The expected failure in a volume was ships going unseen. It is the opposite: a grid on `(x, z)` returns everything in the **disc**, which is a superset of the sphere, so nothing is ever missed.

That is exactly why it survives review. Interest management that is wrong in this direction looks completely correct from inside the game, and quietly funds the bandwidth it was built to save: measured, **51.7 KiB/s against 7.3, a 7.1x over-send**, per client, at 60Hz.

The fix is a height filter on what the query returned, and it is exact at *identical* query cost: same cells touched, same candidates examined, a cheaper test per candidate. Which leaves a real 3D grid nothing to win on but query cost, where it trades 3x fewer distance tests for 2.5x more cell lookups. So the example's recommendation is the one-line filter, and `encode_3d` in `relevance.rs` stays unused.

The control is half the finding: at slab thickness the flat grid costs **1.00x**, degrading smoothly as the world gains thickness. It is right for what it was built for. The general shape: when a structure is wrong in the cheap direction, no symptom will report it, so the measurement has to be built deliberately and paired with the scene where it is right.

### Churn is the other half of the bill (spacemo)

Every measurement in the tree before this one is steady state: N bodies updating every tick, and every optimisation aimed at freshness. Transient entities invert it. A bolt lives about a second, so the cost lands on entry and exit rather than on updates.

Eight ships in one fight: **7.8 ships at 116 bytes a frame against 31.4 bolts at 410**, so the transient half is 78% of the packet while the standing half sits still. A bolt is individually *cheaper* than a ship, 13.0 bytes against 14.9, and collectively 3.5x more expensive because there are four times as many.

Two things follow. Give transients no field they can derive: a bolt carries no orientation, because it points where it is going and the client already has the velocity. And an id has to survive slot reuse, or a client keying on a dense index blends a new entity into the flight path of the one that just vacated the slot, which is what `SlotKey`'s generation is for and the first time anything here has needed it.

The general shape: budget for the entities you can name and you will miss the ones that come and go, because they are numerous exactly when the game is interesting.

### A ratio hides which curve is higher (spacemo)

Positions encoded relative to the observer should make error independent of world size, since relevance already bounds every offset by the view radius. The first version quantised the **anchor** over the world, which put the world's size straight back into the error, and relative came out very slightly *worse* than absolute at every size.

The test passed. It compared growth **ratios**, and relative started higher and grew more slowly, so the check went green while the scheme was strictly worse than the one it replaced. A ratio is a statement about slope and says nothing about which curve is underneath.

Sending the anchor at full width fixes it, and 96 bits once a frame amortises to nothing across the entities in it: error is then **0.0254u whether the world is 400 units across or 400000**, against absolute's 12.2 at the far end. The assertion now demands the two figures be *identical* rather than one growing more slowly.

The general shape: normalising away the quantity you care about is how a comparison passes while measuring the wrong thing. Assert on the value when the value is the claim.

### Two representations of one thing agree by luck until something checks (spacemo)

The simulation reasons in yaw and pitch because a flight model does. The wire carries a quaternion because smallest-three is 29 bits against 64. Nothing forces them to agree, and they did not: a positive rotation about X takes +Z toward -Y while the flight model calls positive pitch nose up.

**Every position was correct throughout.** Ships would have flown exactly where the server put them and simply rendered pitched the wrong way, and once a renderer existed the flight model is what would have looked broken. No positional test, no packing test and no relevance test can see it.

What catches it is rotating the forward vector by the wire quaternion and comparing against the simulation's own `facing()`, with the rotation implemented the long way so it shares no code with what it checks. The general shape: wherever one fact has two representations, the test that matters is the one that converts between them, and it must not be written using the converter.

### Simulate the world, drive the player (cube_yard)

"Make the roll physical" was taken literally: the player cube was driven by a torque, with friction turning spin into travel, so mass and momentum were real. It was the wrong call and it cost a long sequence of fixes, each of which found a genuine bug that was not the problem.

A torque can only become travel through grip, so friction becomes the arbiter of everything. Raise it enough to stop the cube spinning on the spot and it measured **1059N of static friction against a 950N motor**, so the cube simply stopped. Lower it and the cube span without going anywhere. A gathered ball added drag until the player was down to 0.4 units per second, which reads as stuck. Every coefficient tuned moved the problem somewhere else, because the system had no good operating point to tune toward.

The fantasy was never realistic anyway. A player who presses a key expects to move and expects to stop; that is intent, not dynamics. Driving the horizontal velocity directly and reading the **roll off the resulting velocity** made the cube behave, made the spin always match the travel, and removed the dependency on grip entirely, which is why friction could then drop to almost nothing. Gravity, jumping, collisions and the entire field stayed physical.

The general shape: a solver is for the parts of the world nobody is steering. Handing it the thing a player controls means negotiating with it for outcomes the player is entitled to have directly, and the weight you wanted can be a coefficient on the drive instead.

### A filter set on the wrong field is silently inert (cube_yard)

Carried cubes were supposed to stop pushing the player: solid ones make it climb its own ball, and the fix was to filter the pair out of the solver. The player collider got `collision_groups(PLAYER_GROUP)`, the carried cube got a solver filter excluding that group, and the tests that followed went green.

Nothing was being filtered. `collision_groups` and `solver_groups` are **separate fields**, and `solver_groups` defaults to `Group::ALL`. Both sides of a pair have to agree, so the player's default membership of everything matched the cube's filter no matter what that filter said. Every carried cube stayed fully solid for the whole sequence of fixes that followed, and each of those fixes was credited with an improvement it had not caused.

What exposed it was not a test. It was printing the player's contacts while chasing an unrelated bug: resting at 2.495 on four cubes it was supposed to be passing through, with no floor contact at all. The levitation, the infinite jump and the wedging were one cause.

The general shape: a filter that is not applied looks exactly like a filter that passes everything, and a green suite cannot tell them apart. When a mechanism is meant to *stop* something, assert the thing stopped, not the symptom you hoped would improve.

### Rapier sleeps per island; the wire wants per body (cube_yard)

`at_rest` on the wire came straight from `body.is_sleeping()`, which the guide called the whole input a rest detector needs. It is not, and a screenshot showed why: patches of a hundred-odd cubes drawn as awake, lying flat on the ground with nothing near them.

A solver sleeps an **island**, which is every body in a chain of contacts. One cube still jostling in a scattered heap holds every cube touching it awake, and each of those pays a velocity on the wire to hold still. The property the wire cares about is per body and purely local: has *this* body moved recently.

A run of quiet ticks per body, which is what `RestDetector` already models, decoupled the two: 205 cubes reporting awake became 56, against 57 that had actually moved.

The general shape: reusing a solver's flag is free until its granularity is not yours. Islands are the right unit for skipping integration work and the wrong unit for deciding what to transmit.

### Waking what you cannot move is pure bandwidth (cube_yard)

The repulsion field fades to nothing at its rim, but woke every cube inside the radius before deciding how hard to push. Below about eight units the push cannot overcome the cube's own friction, so the outer band was woken, could not move, and trailed each player as a halo of awake cubes paying for velocities that were all zero.

Gating the *wake* achieved nothing, and the reason is worth keeping: a cube woken while the player was close keeps receiving the weak push as the player recedes, so it never gets the run of quiet ticks it needs to sleep again. The condition has to be **motion**, not sleep state. A cube already moving still receives the weak push, so the field itself has no cliff in it; a still one below the threshold receives nothing.

The general shape: an at-rest optimisation is only worth what your own code lets it earn, and a force too small to move anything is not free, it is the cost of the optimisation you just disabled.

### Half the packing win was handed back at the envelope (cube_yard, wire)

A hand-packed payload went from 51877 bytes to 10396, and then travelled in **15502**. A `Vec<u8>` field reaches the outer codec through `serialize_seq`, so every byte is re-encoded as its own integer and MessagePack spends two on anything above 127. Declared as *bytes*, the same payload travels in 10411: a fifteen-byte header over the raw layout.

Worth knowing because the packing work is visible and the envelope is not: the bits are counted carefully in one function and then silently inflated by a field declaration two files away. `plaza_wire::Payload` exists so this is a type rather than a thing to remember.

### A budget planned with a guessed number is not a budget (cube_yard)

The priority accumulator fills a byte budget using a cost function the caller supplies. The first one was written by reading the layout and estimating: 12 bytes for a moving cube, 8 for a sleeping one. The packets came out at **638 bytes against a 533 byte budget**, 20% over the ceiling the whole stage exists to hold.

The fix was not a better estimate. The cost is now derived from the layout itself (`pack::cube_bits`, a `const fn` over the same widths `write_cube` uses), so changing a field's precision moves the budget with it. A constant written down beside the thing it describes is a constant that drifts the first time the thing changes, and a budget that drifts is indistinguishable from not having one.

The same shape appeared twice more in the same example: a bandwidth meter that only pruned its window when a packet arrived, so a link that went quiet kept quoting its old rate, and a test asserting delta-coded indices reward locality that was really measuring how many sleeping cubes each index set happened to select. Numbers about a system should be computed from the system.

### The pulse ring that fired several times per pulse (horde)

**Symptom.** The nova's expanding ring visibly restarted two or three times per pulse, a fraction of a second apart. Reported by a player as "the animation seems to activate multiple times, but dunno if it is just part of the animation". It was not the animation.

**Cause.** The ring is *inferred*: a burst of ten or more deaths in one tick reads as a pulse. Ack-based recovery deliberately repeats an announcement until the acknowledgement for it comes back, which takes a round trip, and the entity stream sends every 62 ms, so one nova's death batch arrives two or three times. The mirror absorbs the repeats idempotently, by design and documented ("applying a superset is harmless"). The death *counter* did not: it incremented per announcement, on the wire, so every repeated batch was a fresh ten-plus-death tick and the ring re-fired.

**Fix.** Two stages. First, count the removal, not the announcement: a death increments the counter only when the mirror actually held the entity and removed it, the same gate the explosion effect already used, so a repeat is a no-op all the way down. Then the inference was deleted outright: the packet now carries the pulse's server timestamp, and the ring is a pure function of that instant and the frame clock. Nothing triggers, nothing decays, a repeat is the same ring by construction, and a mid-pulse joiner draws the remainder of the ring it walked in on, which the inference could never give it. The same move as the projectile lesson one section down: send the event, not a thing to guess the event from.

**The general form.** In a protocol that repeats until acknowledged, "how many times was I told" and "how many times did it happen" diverge by design. Anything derived from such a stream must be exactly as idempotent as the stream itself: count state transitions, never messages. The mirror got this right from the start; the one counter that read the wire instead of the state is the one that produced a visible artifact.

### A backgrounded tab could not come back (horde)

**Symptom.** Switch away from the browser client for a while, come back, and the world does not resume. It never resyncs, and the longer it was away the worse it is.

**Cause, and it was not the resync.** The client's clock was built by accumulating frame time with a cap: `now_ms += min(frame_time, 100)`. The cap is right for deciding how much *simulation* a frame runs, and wrong as a clock. A browser stops running frames for a hidden tab, so a client that adds up the frames it happened to run believes less time passed than did, **permanently**. Its estimate of server time is then wrong by however long it was away, nothing arriving is ever due, and the playout queue grows without bound because the queue was drained by a clock that had stopped.

**The recovery that existed could not fire.** A client that falls behind is supposed to be rescued by the server: its acknowledged baseline ages out of history and the next packet is a full rebuild. But that only happens once the client resumes applying and acknowledging, and a client whose clock is wrong keeps acknowledging old sequences, so the server reads it as slow rather than lost.

**Fix, in three parts.** The clock comes from real time and the cap applies only to the simulation step, which is the actual bug and the one that would have prevented the rest. The queue is bounded, because a buffer fed by a peer and drained by a local clock has to be. And a client that finds itself far past the instant it is drawing treats it as a **discontinuity**: drop the queue, drop the mirror, re-anchor on what just arrived, and let the server's next digest check rebuild the world. Counted as `timeline restarts` in the panel, beside the underruns and view fallbacks.

**The general form.** "How much time has passed" and "how much work may I do about it" are different questions, and a frame loop that answers both with one number will lose time whenever it is throttled. Cap the work, never the clock. And the discontinuity rule already established for position applies to time: there are no intermediate states between a minute ago and now to ease through, so snap.

### The recovery that was worse than the stall (horde)

**Symptom.** With the timeline restart in place and its unit tests green, a real backgrounded tab was tried, and the recovery was shit: a freeze of several seconds on refocus, then a stuttering settle.

**Why the tests missed it.** They tested the *decision* (snap rather than crawl) by feeding packets one at a time. What they never modelled was the *delivery*: the browser's socket keeps receiving while the frame loop is stopped, so the entire stall arrives as one pre-buffered lump that must be handed over, and the server had spent the whole stall escalating. Three mechanisms stacked:

1. **The server shouted full baselines at a deaf client.** A silent seat's acknowledged baseline ages out of the 24-packet history in 1.5 s, after which every plan is the full visible set: roughly 25 KB of JSON, 16 times a second, tens of megabytes a minute, none of which the client would ever play.
2. **All of it was parsed in one frame.** The JS-side queue is unbounded (correctly: it is a pipe), and the first poll back drained and `serde_json`-parsed the lot before the frame could render. That is the freeze.
3. **The restart fired on the wrong trigger, repeatedly.** The render clock snaps to the present on the first drained packet, so the 3 s rule never saw the backlog as "ahead"; only the 256-packet queue bound tripped, once per 256 packets, tearing down each partial rebuild the previous trip had paid for. Meanwhile every backlog packet was counted as an underrun, so one stall read as a thousand link faults.

**Fix, at each layer.** The server throttles a seat that has stopped acknowledging (3 s, the same threshold as the client's own discontinuity rule) to about one packet a second: the keepalive that lets a resumed client rediscover the stream, at a thousandth of the cost. The client drops a resume backlog **before parsing it**, on message lengths alone, keeping only the tail, and restarts the timeline once, deliberately; the drop is reported in the panel ("resume drops"), and the host panel shows "seats throttled for silence". And underruns only count lateness on the scale jitter produces; past the discontinuity threshold the packet belongs to a lost timeline, which `resyncs` already accounts for.

**The general form.** Flow control is part of reliability, not an optimization: a protocol that keeps sending to a reader that has provably stopped reading is choosing where the damage lands, and it lands on the reader at the worst moment, resume. And a unit test that feeds a component its input one piece at a time has not tested arrival; the transport's actual failure shape (a burst, a lump, a reorder) has to appear in a test or the first real exercise of it is the user's.

**Where it lives now.** The whole recovery became framework blocks, with horde retrofitted onto them as the proof: `DeltaBaseline::with_flow` (the server-side throttle), `client_utils::PlayoutBuffer` (the bounded queue, the discontinuity rule, and both counting rules), `plaza_ws::trim_backlog` (the drop-before-parse), and the resume contract written into the docs of `client_utils` and `server_utils::delta`: a client may discard any stretch of the stream unread provided it also drops its mirror, because an acknowledgement carrying the digest of nothing obligates a full baseline. The same extraction pass took the input schedule (`server_utils::InputSchedule`), the tier hysteresis (`relevance::TierBoundary`) and the arrival measurement (`client_utils::ArrivalMonitor`), which also gave the joiner's panel a measured render-delay budget the host-side sliders could never provide.

### Iffy movement and a churning resume, hunted with a probe (horde)

**Symptom.** After the extraction: "recovery sucks now and movement is iffy". The natural suspect was the extraction, seven commits of it.

**The A/B came first, and cleared the suspect.** A probe (`examples/recovery_probe.rs`: one real client against the real server over the simulated link, printing marker continuity and stall recovery as numbers) run at the last praised commit and at HEAD produced **bit-identical output**. Both symptoms predated the extraction; the refactor was faithful to bugs as well as to features.

**The probe then lied once, instructively.** It reported the moving marker holding still on 34.6% of frames at every render delay, which read as a structural stutter and produced a wrong theory (the clock's full-strength resync fighting the tick's advance) and a wrong fix, reverted when three tests explained that the clock *tracking arrivals at full strength is the contract*, and that `recv` is a smooth estimate anyway. The real cause of the 34.6%: the probe steered its player in a straight line into the arena wall and then measured the wall. A measurement of movement has to guarantee the thing measured is moving.

**What was actually wrong, twice.**

1. **Movement: the exact-fit budget.** The defaults shipped as `30 one way + 20 jitter + 100 interval = 150` render delay, margin zero, so every jitter spike at the tail of the distribution put the newest sample behind the interpolation target and the marker held then snapped: worst frame-to-frame jump 8.3 px against a normal step of ~3. At 180 ms the snaps vanish (worst 3.2 px). The default is now 180, and the joiner's measured-budget line exists precisely to catch this class without access to the sliders.
2. **Resume: a pinned baseline.** After a resume, the client's acknowledgement window spans the silence, and the keepalives inside the silence are sparse in the sequence space (the sequence advances for every seat's round), so `contiguous_base` could never cross the holes: the acknowledged baseline pinned at the first keepalive, the server diffed against a state as old as the stall, and staleness had to fire a second time to clear it. Measured as ~25 consecutive full baselines over 1.5 s. Two fixes in `DeltaBaseline`: a rebuild clears the sent history (a stale in-flight ack must not resurrect a pre-rebuild baseline), and an ack arriving from a seat flow control knows is stalled is treated as **the resume signal**, opening a fresh epoch: one full baseline, acknowledged contiguously, deltas one round trip later. Measured after: 4 full baselines, all in the first second.

**The general form.** Ablate by revision before theorizing by diff: a bit-identical A/B is worth any amount of hunk-reading. And a probe is an instrument, so it needs the same skepticism as any counter: the two readings that survived (worst-jump, full-baseline count) were the ones checked against a mechanism, and the one that misled (holds) was the one nobody asked "what else could produce this number".

### The second-consumer pass, and the retrofit that was refused (all three)

Before calling the blocks shippable, each needed a second game, and the pass produced one deliberate refusal alongside the adoptions. `ArrivalMonitor` took over netcode's adaptive buffer (which had been sizing itself from *ping* jitter, a proxy that diverges from snapshot-arrival spread exactly when the buffer matters) and `TierBoundary` took over blackhole's per-pellet correction membership (aligning the leave radius with the pellet draw cutoff, so everything on screen keeps its corrections). But `InputSchedule` was **not** forced onto netcode: its commands carry a sequence number and no tick, because apply-on-arrival with a sequence frontier is that demo's model, and bolting tick addressing onto it would change what the server means, not how it is written. The block's docs now carry the two-model table so the choice is made deliberately. Also found in the pass: netcode's input inbox was unbounded (now capped, oldest dropped), and blackhole **cannot** adopt the resume kit at all, because its removals are order-sensitive events with no digest contract, so discarding any stretch of its stream unread is unsafe by design; that is the cost of the apply-on-arrival architecture, now stated where the two architectures are contrasted.

A 128-seat soak (`examples/soak.rs`: ten simulated minutes, five hidden-tab cycles) closed the pass: modelled bandwidth flat at ~2.47 MB/s, about two full rebuilds per stall cycle, no counter trending. It also showed the layers composing unplanned: the server-side throttle keeps a 12 s backlog under the client's trim trigger, so mid-length stalls recover by ordered replay without a timeline restart at all.

### Bandwidth that climbed for ever, and was the meter (horde)

**Symptom.** A player reported, repeatedly, that bandwidth crept upward the longer a session ran and never settled: standing still, moving about, at 128 players and again at 10. Two screenshots three minutes apart showed 127.6 then 143.9 KiB/s while the live enemy count *fell* from 2311 to 1751.

**Three wrong explanations, each defended with a measurement.** That the horde was collapsing and refilling (true earlier, fixed, and not this). That it tracked the live enemy count (refuted by the screenshots above: bandwidth up, population down). That it was the nova cycle oscillating (real, but an oscillation is not a trend). Each was checked against a harness that measured **per window** and therefore could not reproduce the symptom at all, which should itself have been the clue.

**Cause.** `RateMeter::per_sec` was `total / elapsed` over the meter's whole life. That is a session mean, not a rate. A session mean chasing a steady state it has not reached converges **asymptotically**: it climbs by less and less, but it climbs, for as long as the session runs. Every reading is arithmetically correct and the number goes up every time you look. Anything that raised traffic even briefly, a burst of spawns, a walk through a dense corner, raised the mean permanently, because the denominator only ever grows.

**It also made the panel useless for its stated purpose.** A slider you have just moved is one second of evidence against twenty minutes of history, so the readout barely twitches, in a panel whose entire job is showing what the sliders do.

**Fix.** The meter keeps a rolling window (sixteen buckets of 500 ms) and reports the recent rate; the lifetime figure is still available as `lifetime_per_sec` for summaries, where a session mean is the right answer. One subtlety worth keeping: the newest bucket is normally only part filled, so the divisor is the span the retained buckets actually cover rather than the nominal window, which is otherwise a steady few percent of understatement.

**The general form.** A measurement instrument is part of the system under test, and this file now has four entries where the instrument was the defect: a harness with no acknowledgement loop, a counterfactual that modelled only some fields, a fault check that compared a delayed client against the present, and now a rate that was a mean. The pattern in all four is the same: the number was *plausible*, so it was read as evidence about the game rather than as a claim about the instrument. When a measurement disagrees with a player who is watching the thing directly, suspect the measurement.

### A fault readout that was really the render delay, twice (horde)

**Symptom.** After the acknowledgement fix above, `entities held that are dead on the server` went from 15 to 156 and turned red, while every other number improved: churn balanced, the packet became 4% new arrivals instead of 99%, the worst server tick fell from 185 ms to 6 ms.

**Cause.** The check compared what the client draws **at its render instant** against server truth **now**. A client that renders 250 ms in the past is holding every entity that has died since that moment, by construction. At roughly a thousand kills a second, that is a couple of hundred entities, and the number got *worse* precisely because the client had stopped being force-fed the present and was correctly on its own timeline again.

**Fix.** The server keeps a bounded log of recent deaths with their times, capped at the deepest render delay the panel allows, and a held entity counts as a phantom only if it was already dead **at the instant being drawn**.

**This is the same mistake as `mean_render_error`**, which is already filed as open work, and it went unnoticed in a second metric for as long as nothing exercised it. Any comparison between a delayed client and an authoritative server needs the server's state *at the client's instant*, and a server that keeps no history cannot answer that: the alternatives are keeping one (a bounded log here, `HistoricalStateBuffer` in general) or not making the claim.

### The 127 clients that never acknowledged (horde)

**Symptom.** A host running 128 players reported bandwidth climbing slowly over half an hour, and `churn: 136.9 spawns / 0.2 despawns per packet` against `sent per packet: 138 entities`. Almost every entity in every packet was arriving as a brand new spawn, and almost nothing was ever retracted.

**The measured world disagreed.** A harness driving the same arena for twenty simulated minutes, to maximum difficulty, showed bandwidth flat to 2.5% and roughly 17 spawns per packet. The difference between the two was not the game: it was that every client in the harness **acknowledged**, and 127 of the 128 seats in the real arena had nobody to acknowledge for them.

**Cause.** Seats with no connection still have packets built for them, because an empty seat drifts as a bot and is still simulated. Under ack recovery a baseline advances only on acknowledgement, so those seats sat at an empty baseline for ever and every packet built for them was a **full dump of the whole visible set**. Nothing was sent (there is no connection), but the readouts count every packet built, so a full arena was being charged for a defect none of its actual clients had, and the spawn count was eight times the truth.

**Fix.** The arena acknowledges on behalf of a seat nobody is connected to. Its client is the server itself, holding exactly what it was sent over a wire that cannot drop anything, so the acknowledgement is not a convenient lie: it is the only accurate thing to say.

**And the slow climb was the horde refilling.** A separate fix had just raised the wave cap so 128 players could no longer annihilate the population, so the arena spent several minutes growing from a few dozen enemies to three thousand, and the per-view cost grew with it. It plateaus.

**The general form, for the third time in this file.** An unacknowledged delta stream reports full re-sends for ever, and the numbers it produces look entirely plausible. It was found in a ghost measurement, then in a bandwidth harness, and now in the shipped example itself. The first two were fixed by writing the lesson down and later by an assertion in the harness; neither of those could reach this one, because here the missing acknowledgement was a property of the *arena* rather than of a test. What would have caught it is a readout comparing spawns per packet against entities per packet, which is exactly the pair the panel was already showing and nobody had thought to read as a ratio.

### A world that was not there, and the saving that went negative (horde)

**Symptom.** Two at once, both reported by a player at 128 players: bandwidth appearing to climb the longer a session ran, and the "saved against UUIDs and `f32`" readout going **negative** for the first time, at -17%.

**The negative saving was a broken comparison, not a regression.** `naive_bytes` modelled only the entity lists, while `bytes` counted coins, wallets, hit markers, the digest and the sequence number as well. That was invisible for as long as entities dominated the packet, and the moment they did not, the ratio was comparing a full packet against a partial model of itself. Moving player state onto the player stream made it worse, because the real cost still counted that stream while the counterfactual had lost its only player term. The fix is that the baseline models **every field the packet carries**, so it measures the encoding rather than which fields somebody remembered.

**And the entities did not dominate, because the horde was gone.** The wave spawner filled at most 40 enemies per 500 ms whatever the player count, while the kill rate scales with players: each one carries an auto-firing weapon and a nova that clears a radius every 4.5 seconds. At 128 players that held a 3000-strong horde at **about 40 alive**. Every entity number was therefore a measurement of an almost empty arena, including the ones quoted as evidence that the relevance work had succeeded: 607 KiB/s at 128 players was really 2.1 MiB/s once the world existed.

**Bandwidth was not climbing at all.** Measured in fifteen-second windows it is flat to within 5% across two minutes. What a player sees climbing is a rolling average filling up, and the arena repopulating after each nova.

**The general form, and it is the third time this file has arrived at it.** A number from a system in a degenerate state describes the degeneracy, not the system. A harness with no acknowledgement loop reported zero samples; a nearly empty arena reports small packets and a negative saving. Both look plausible. The defence is the same in both cases: assert the precondition, and put the population on screen next to the bandwidth so the reading cannot be believed without it.

### The marker that detached from its own timeline (horde)

**Symptom.** At 600 ms render delay, shots visibly left from "a point in the past", well behind the player's marker. Reported by a player; every readout said healthy. The natural reading of the symptom, that the shots were mis-timed, is backwards: the shots, enemies and coins were all faithfully at the render instant. The *marker* was the thing off the timeline.

**Cause.** The player history buffer held a constant 8 snapshots, roughly 200 ms at the default send rates, while the render delay slider went to 600. Past what the history covered, the snapshot buffer *clamps to the oldest sample it still holds*, so the marker rode a couple of hundred milliseconds behind now while the scene was drawn at T, and the gap scaled with the slider. At the default 150 ms delay the 8 snapshots happened to cover it, which is why it looked fine for as long as it did.

**Why nothing caught it.** The clamp returns `Some`, so the off-timeline counter that exists for exactly this class of failure never fired: the silent compensation lived one layer below the layer that was instrumented. A mechanism that silently compensates for a fault destroys the evidence of it, and this time it also destroyed the evidence gathered *about* it.

**Fix.** Two parts, both principles this file already states. The buffer capacity is now derived from the thing it must cover, `RENDER_DELAY_MAX_MS` at the maximum rate of both streams, and the slider range is derived from the same constant, so the two cannot disagree again. And the clamp is counted: `RemoteView` exposes the oldest instant it can still reach, and a render target older than that increments the same `view fallbacks` readout as an empty view.

**The general form.** A buffer sized by a constant is an accidental limit, and an accidental limit does not refuse you, it degrades you silently, exactly as the admission entry found one layer up. Any buffer that exists to serve a *declared* number (a render delay, a playout depth) must have its capacity derived from that number's maximum, or the declaration is a lie past the point the constant happens to cover.

### A client mirror that diverged and could never recover (horde)

**Symptom.** The digest mismatch counter climbed to a few hundred within a minute and then stopped, always around the same number. Turning latency, jitter and loss to zero did not help. The offline build reported zero with the same simulation code.

**Theories that were wrong.** Packet reordering (the impairment link could reorder, but WebSocket is TCP and cannot, so this was a phantom the tooling invented). Frames dropped at the session queue (added a lost-frame counter, which read zero for the whole run). Frames failing to deserialise (round-tripped every frame through JSON in a test, all survived). Acknowledging frames on receipt rather than on apply (checked: acknowledgement already happens after the packet is applied).

**How it was actually found.** The digest reported that the mirror was wrong but never how. Adding a debug mode that ships the server's exact visible key set alongside the digest turned the counter into a diff, and one run was enough: every missing entity was generation zero, and they were spread evenly across the whole slot range. That is not drift, which would cluster. That is a client that was never told about most of the world.

**Cause.** The arena builds packets for every seat from startup, occupied or not, because an empty seat drifts as a bot and is still simulated. By the time a real client connected, that seat's relevance baseline was already most of the world, so the joiner's first frame was a delta against a baseline it never received. Almost nothing arrived as `entered`, and the world trickled in only as parts of it happened to become newly relevant.

**Why it could not self-heal.** Once the server believes a client holds an entity, that entity is only ever sent as a position sample, and a client discards samples for entities it does not have. The delta stream had no path back.

**Fix.** Two parts. `Server::reset_seat` clears a seat's baseline when a fresh client takes it, so the first frame is a full dump. And the acknowledgement now carries the client's own digest, so the server can compare it against the digest of the state it believes the client reached and force a clean rebuild when they disagree. The first prevents the common case, the second recovers from any cause at all.

**General lesson.** Any per-subscriber delta stream needs both: a baseline reset on join, and a way for the subscriber to say what it actually holds. Detecting divergence is not the same as being able to fix it.

### Enemies lunging whenever the player moved (horde)

**Symptom.** A jump forward while playing as host, most noticeable when moving, absent when standing still.

**Cause.** The client fed its locally predicted player position into the shared enemy rule, while the server aimed enemies at its authoritative position. Every packet snapped the enemies between the two.

**Fix.** The prediction drives only the camera and the player's own marker. The shared rule reads the authoritative position, exactly as the offline build does.

**General lesson.** Prediction is a presentation concern for one entity. Feeding it into a rule both sides run creates a second world.

### A rhythmic forward tug while holding one direction (horde)

**Symptom.** A small jump forward roughly every four hundred milliseconds while moving in a straight line, at any latency including zero.

**How it was found.** A correction log recording magnitude, direction relative to the held input, and the one-way estimate. The pattern was unmistakable: corrections were almost exactly the correction threshold in size, always forward, and evenly spaced.

**Cause.** The local player pulled toward the server only once the error exceeded a fixed threshold, then closed the entire gap at once. A slow systematic drift therefore became a sawtooth.

**Fix.** Ease a fixed fraction of the error every packet instead, so drift is absorbed continuously. Discontinuities beyond a much larger bound still snap, because a respawn must not be eased.

**General lesson.** Choose snapping or easing by cause, not by magnitude: ease continuous error, snap genuine discontinuities.

### A hole that jerked constantly (black hole)

Three separate causes wearing one symptom, which is why it took three rounds. The correction log made each one visible in turn once the previous was removed.

**Cause one: an unpredicted force.** The server moves a hole in three passes: input, gravitational attraction toward every other hole, and collision separation. The client predicted only the first. The attraction is capped above walking speed by design, so the unpredicted term was large exactly when it mattered. Fixed by predicting the pull with the same rule the server runs, from the attractor field the client already receives.

**Cause two: predicting a frozen entity.** An eliminated hole is frozen by the server for a two and a half second respawn delay. The client kept integrating input into it and reconciling the difference every packet, generating a correction stream entirely of its own making. Visible in the log as long runs where the authoritative position never changed. Fixed by freezing the prediction while dead.

**Cause three: an unpredicted ability.** The dash was deliberately left unpredicted on the grounds that mispredicting a discrete grant is worse than mispredicting continuous movement. That reasoning predates the client having a reliable local mirror of the server's dash cooldown, which it now uses to light the burst instantly. Predicting the movement too is a toggle, so both behaviours can be compared live.

**General lesson.** Enumerate every way the server can move an entity before deciding what the client predicts. The largest error is always the term nobody listed.

### A repulsor ring that lagged and stuttered (horde)

**Symptom.** The ring trailed the player marker and stepped rather than glided, even at a high send rate.

**Cause.** The ring was drawn at the authoritative position, which only changes when a packet lands, while the marker was drawn at the prediction, which advances every frame. Two different clocks in the same picture.

**Fix.** The local player's own ring follows the prediction. Peers' rings stay on their authoritative positions, which is where their physics is.

**General lesson.** Which position each drawn element uses is a deliberate choice, and mixing them in one scene is visible immediately.

### A player who could not be controlled after a settings change (horde)

**Symptom.** Changing the enemy count left the local player unresponsive. Movement was predicted, so the marker moved and was then pulled back every packet.

**Cause.** Rebuilding the world preserved `clock_ms` (a fix from an earlier bug in this same file) but reset a *separate* tick counter to zero. The client kept naming ticks derived from the real clock, and the server rejected every one of them as impossibly far in the future.

**Fix.** The tick is derived from the clock rather than counted alongside it.

**General lesson.** Two representations of one fact will eventually disagree, and the fix for the earlier bug is what created the disagreement: preserving one of the pair across a rebuild and resetting the other is worse than resetting both. This is the same shape as the three separate key-packing functions and the two digest folds, and it is the most repeated cause in this document.

### An impairment slider that did nothing on the real path (horde, then black hole)

**Symptom.** Dragging packet loss changed nothing over a socket.

**Cause.** Two, in the same direction. The downstream impairment link took latency and jitter from the panel and a hardcoded zero for loss. The upstream had no impairment at all, so inputs, acknowledgements and purchases crossed a perfect wire however far the slider went. Loss worked in the offline single-process build, which is where every measurement in the report had been taken.

**Why it mattered more than it looks.** The acknowledgement is what lets the server tell a starved mirror from a healthy one, so the whole loss-recovery mechanism was being exercised only in the one configuration where its input could never be lost. Fixing it made the late-input and rejected-input paths reachable from the panel for the first time.

**General lesson.** **A toggle wired to the demo path and not the real one reports on a system nobody runs.** Control-plane traffic (a version handshake, a ping) is a reasonable exemption; the traffic the mechanism under test depends on is not.

### A joiner that could not move for half a minute after its tab woke up (horde)

**Symptom.** A browser tab suspended and resumed. Frames arrived, the world drew, acknowledgements flowed, and the player could not move. It recovered on its own after tens of seconds. Alongside it, the joiner's measured-budget line jumped to 225 ms, climbed past 300, then decayed back.

**Two wrong fixes came first, and both were reasonable.** The first: the budget readout is smoothed from declared stamps, a resume feeds it the kept tail's stall-era stamps plus one stall-sized gap, so reset the `ArrivalMonitor` on a timeline restart. It passed a scripted test that reproduced the resume, and made things *worse* in the browser. The second: a pong answering a pre-stall ping measures the suspension as a round trip and poisons the clock fit, so rebuild both estimators and refuse cross-stall pongs. Also tested, also shipped, also fixed nothing. Both were reverted.

**What the captured data said.** Panel readouts added for the third attempt, read during a stuck window: acknowledgement lag 4, identical to healthy; input aim **-5 ticks** against a 4-tick accepting window; worst raw pong 1056 ms; round trip smoothed to 313 ms while the link was 21. So the inputs *were* arriving and *were* being refused, for naming ticks the server had already closed. No 40-second pong ever arrives, because pings stop while the frame loop is frozen, which is why the second fix could not have helped: it treated a poisoning that barely existed.

**Cause.** After a resume the clock fit trails the stream: its window predates the stall, the first pongs back are delayed behind a draining backlog, and it refills at one ping a second. Every input names its tick from that fit, so for as long as the fit lags by more than the late window, every input is dropped.

**Fix.** Floor the named tick at `(newest arrived stamp + playout depth) / step`. The server *wrote* that stamp, so server time is provably past it, and the floor needs no clock at all; it only ever lifts the aim, and never past where a perfect clock would have aimed, because the stamp trails true server time by the one-way delay. When the fit is healthy its estimate exceeds the floor and nothing changes.

**General lesson, and it cost two shipped fixes.** **A readout that misbehaves is usually downstream of the broken quantity, not the broken quantity.** Both wrong fixes repaired *measurements* of the clock while the clock itself went on naming closed ticks; the second even reset the thing whose lag was the actual fault, then let it re-lag. The corollary is the fix that worked: **where a bound can be derived from something the peer stated, prefer it to an estimate.** An estimate is wrong exactly when it is under stress; a stamp the server wrote is a fact at any clock skew.

**And a blind spot worth naming.** The client's acknowledgement lag *cannot* see this failure: the server acknowledges an input on arrival, before admission, so refused inputs are acknowledged exactly like accepted ones. The verdict exists only on the server, which is why `InputSchedule` now reports rejections split by side with the last margin in ticks, and why the host panel shows them per seat.

### Four bugs with one shape (bomb grid)

The lattice example was written in a day and then debugged from four screenshots, and the four faults turned out to be one fault wearing four costumes. Each is worth its own entry, and the shape they share is worth more than any of them.

**The symptoms, in the order they were reported.** A player who kept walking after the key was released. A player who could not stop during the interval after winning a round. A player who snapped back constantly while running across open ground. And a residual two snaps per hundred frames that survived all three fixes.

**Three of the four looked exactly like network faults**, and none of them was. The panel said what a network fault says: corrections happening, a player not where the server puts them. Every one of them was the client and the server running the same rule on different clocks.

**1. An acknowledgement is not permission to forget an input.** An input names the tick `press + playout`, and the server acknowledges it on **arrival**, which on a fast link is a hundred milliseconds before that tick. The pending list was being trimmed on the sequence number alone, so the release you pressed was discarded in flight and never ran locally: `held` kept the old direction for ever. The list is two things at once, the replay buffer for corrections and the client's own schedule of inputs whose tick has not come, and trimming the first emptied the second from under it. `retain(|p| p.seq > seq || !p.applied)`.

**2. A server that stops simulating has to say so.** The round-over interval freezes every player so the last explosion stays readable, and *nothing in a frame expresses that*: the players simply stop moving, which is indistinguishable from everybody standing still. The client kept predicting through it and every frame of the interval was a correction invented out of a rule it had never been told. This is exactly the bug `PredictedPlayer::set_active` was added for in the black hole example, arriving again in a different game, which is what a repeated cause looks like.

**3. Prediction must run on the server's tick grid, not the frame's.** The client advanced its player once per rendered frame; the server advanced once per tick. Even at matching rates the two grids are unaligned, so they cross every cell boundary up to a tick apart, and any frame arriving in that window is a genuine disagreement. It scales with boundaries crossed, which is why **open ground** made it obvious and a cluttered board hid it. Fixed by making the prediction a function of the tick: catch up to `clock / SIM_STEP_MS`, one step at a time. The `dt` parameter was then deleted from the client's `tick` entirely, because a caller must not be able to influence how fast a prediction runs.

**4. The authority was not advancing in whole ticks either.** `TickDriver::run` hands over the **measured** elapsed time, which is correct and documented for logic that integrates, and fatal for logic that is predicted: at 62 Hz it delivers 16, 17, 16, 16, 17, so the simulation's rate becomes a property of the host's scheduler. The client stepped in exact ticks, the server in measured ones, and they accumulated a walk at different rates. This was the residual that survived three client-side fixes, at 2.2 snaps per hundred frames with no packet loss, no jitter worth the name, and every input accepted on time.

**The general lesson, and it is the fourth principle at the top of this file made concrete.** A shared rule and tick-addressed inputs are not enough for prediction. **The authority and its clients must advance the same simulation in the same quantum, on the same clock, from the same rule**, and every one of those four is load bearing. Three of the four bugs above were a violation of the *quantum*, in three different places, and each one presented as a network problem because a correction is what a network problem looks like.

**The discrete case is not a special case, it is the visible one.** Every one of these bugs exists in a continuous game too, where each shows up as a permanent sub-pixel correction that easing hides completely. On a lattice there is no fraction of a cell to ease through, so the same fault becomes a jump the player can see and the panel can count. That makes a grid game a genuinely better test rig for prediction than a smooth one: it cannot hide its own netcode.

**What it changed in plaza.** `TickDriver::run_fixed` and `run_fixed_for`, which pace to real time while delivering whole steps of exactly the size asked for, carrying the remainder and dropping a stall rather than repaying it as a burst. Before this there was no way to run live at a cadence with a constant delta: `run` measured, and `run_virtual` was fixed but unpaced. `run`'s documentation now names the hazard rather than leaving it to be discovered, which cost four rounds of blaming the network.

### An input that names a place cannot be scheduled into agreement (pellet maze)

The maze example was built after `bomb_grid` had already paid for the four bugs above, so it started with a shared rule, tick-addressed inputs and a matched quantum. It still disagreed, and the disagreement was of a kind none of that machinery addresses.

**A turn is a request for a place.** Press a direction and you do not turn: the turn is queued and taken at the next junction where that direction is open. So the two sides can agree exactly about *when* the request was made, run the same function on the same tick, and still resolve it at **different junctions**, because a junction is decided by the input history between the request and the corner. Get it wrong by one and the sides are not one cell apart, they are in different corridors, and the error grows with every step instead of being corrected away.

That is why the counter is separate. A cell snap is bounded: one jump, over. A wrong junction is unbounded until a frame drags the client back. Averaging them together lets a hundred cheap corrections hide three expensive ones, so the panel reports `wrong junctions: N of M turns, worst K cells apart` above the snap rate.

**Latency alone still does not cause one**, at any depth. Losing the request does, and only that. The two are pinned as separate tests, because the intuition that a place-input is "more sensitive to lag" is wrong and would send the next person tuning the wrong parameter.

**The policy has to cross the wire.** How long a queued turn stays alive is a server setting, and a client that assumed a different one would predict a turn the server had already dropped and then run down a corridor the server never entered: a wrong junction manufactured entirely out of a disagreement about policy. It rides in `ServerPolicy` with the playout depth and the send rate.

**Secrecy is a property of what the server sends, not of what a client draws.** The invisibility power-up made the frame **per recipient**: a hidden player is *absent* from everybody else's copy rather than flagged in it. A client handed a position it should not have has already lost the secret whatever it renders, and this is the same rule `card_table` applies to a hand of cards, arriving in a game where the hidden thing moves sixty times a second. Per-recipient frames cost one clone per seat here and are the only honest implementation.

**And that was still a leak, because a frame is not the whole stream.** Hiding the player from every frame shipped, and gave away nothing at all, because the *events* were still broadcast: a pellet vanishing names the exact cell on the exact tick, and being an event rather than a frame it is not even rate limited. A power-up taken names a cell. A turn report names the junction, and no client ever read another player's. Two frame fields gave it away too: a pellet count that dropped while nothing visible moved, and a pickup that disappeared from a cell. The fix is an audience on each event, held for everybody else until the vanish ends and then sent late rather than never, plus the two counts computed per recipient. **The general rule: secrecy is a property of the entire outbound stream, and the leak is always the message you did not think of as a position.** The test that pins it reads the wire, not the server's intent: take the hidden player's real cell each tick and check every op every other seat was handed.

**Two bot bugs, one shape, and it is not the netcode's shape.** Three of four seats are bots, so a bot that looks broken makes the example look broken. Both faults were guards that were true far more often than they read as being: `drive_bots` skipped any player mid-step, and since a player begins its next step the instant it ends the last, that was nearly always, so a bot only ever chose a direction while already stuck against a wall. And the routing BFS was seeded in every direction including backwards, so a runner ate the cell it stood on, found the nearest remaining pellet behind it, turned round, and paced one corridor for the whole round. Fixing both took eating from 36 pellets in 45 seconds to 165. **Neither was visible without a number**: "the bot runs around a lot" is what both look like.

**A feature measured is a feature you can delete.** Routing a threatened bot runner toward a nearby energizer sounded obviously right and was written. Measured over a minute it devoured no more pursuers, ate 22 fewer pellets, and left six power-ups uncollected, because a runner already crosses every corridor eating and walks over them anyway. Deleted, with the measurement kept in the test that replaced it.

### Where a float actually breaks determinism, and where it does not (seed defense)

The determinism example sends a seed instead of a world, so an arithmetic difference between two machines is never corrected: it compounds for the length of a wave. Building it meant writing deliberate ways to break determinism, so that the detector could be shown catching something, and **two of the first three did not work at all**. Why they did not is more useful than the one that did.

**A float in an accumulator diverges from nothing, as long as the result is re-quantised.** The first quirk moved enemies with an `f32` multiply and add instead of fixed point. It never changed a single tick, on any seed, over any length of run. The reason is that the result is truncated back to 1/256 of a tile *every tick*, so the float error is discarded before it can accumulate: the sum of a hundred rounded values is the same whether or not each addition was exact. This is the strongest argument for fixed point there is, and it is also the thing people get backwards. "We use floats internally but round the positions" really is most of the protection.

**A float in a *constant* breaks it immediately.** A runner covers 4.2 tiles a second, which at a 25 ms tick is 26.88 in 256ths. The integer ratio floors that to 26. Working the same number out in floating point and rounding gives 27. Four percent, multiplied by time, for ever: ten seconds later the two machines' runners are a tile and a half apart and each is being shot at by a different tower. The lesson to carry: **audit the constants, not the loops.** A rate, a radius or a price computed once at startup is where the machines part company, and it is the code nobody re-reads because it has no state in it.

**A float in a rarely-consulted comparison diverges too rarely to matter, or to test.** The second failed quirk made a tower's *range* a float, changing the radius by 1/256 of a tile. It is a genuine difference and it is nearly unobservable: it only changes anything during the fraction of a tick an enemy spends inside that band while a tower happens to be off cooldown. Real, undetectable in a minute of play, and therefore useless as a demonstration. Worth knowing when triaging: a determinism bug's *frequency* depends on how often the differing value is consulted, not on how wrong it is.

**A quirk that cannot diverge is a test that proves nothing.** The third failed one was "iterate the towers in hash order instead of placement order", which sounded like the canonical determinism bug and turned out to be impossible in this code: damage is additive and the dead are collected after every tower has fired, so no tower can take another's kill within a tick. The design was already safe, and the toggle would have sat in the panel implying a detection that could never fire. It was replaced by a *targeting rule* quirk, which does diverge, and the loop now carries a comment saying why its order is safe rather than leaving the next reader to assume it.

**The general rule, and it applies well beyond this example: a fault injector has to be tested like anything else.** Every one of the three is now covered by a test that asserts it *does* change the world, so the panel cannot come to claim a detection that no longer happens. The same discipline that catches a vacuous test catches a vacuous demo.

**And the first real bug the digest caught was in the server, not a client.** The server laid a wave out at the end of the tick it announced the wave on; clients laid it out at the start of the tick the announcement named. One tick apart, and every wave began with a mismatch. The wave is now scheduled to a tick exactly like a build op is, which is the same principle the rest of this file keeps arriving at from different directions: **if two machines must do a thing at the same moment, the moment has to be named, not implied by when each of them noticed.**

### Replaying the past is the same discipline as agreeing with the present (ghost trials)

The racing example stores a run as the inputs that produced it and replays them to make a ghost, which means a machine has to agree with **a recording made somewhere else, at some other time**. That is `seed_defense`'s problem with the second party removed, and everything that was true there is true here with one difference: the recording cannot be asked to compromise. There is no negotiating with last week's log.

**An event log is small because an event is a change, not because it was compressed.** One entry per change of input rather than one per tick: a two-lap run is 146 entries over 1208 ticks, 738 bytes against 12,088 bytes of sampled positions. Worth knowing where that ends, though. The autopilot that drives the tests originally steered on *every tick*, the way a bang-bang controller does, and it scored barely three times better than the path. Giving it a deadband so it drove like a person took the ratio to sixteen. **The saving is a function of how still the input holds**, so the technique pays for humans and pays much less for machines.

**Deciding by reconstruction is not a heuristic, and it is cheap.** The server never watches anybody race: it is handed a log and a claimed time, replays the log, and compares. There is no plausibility check, no speed cap, no statistical model, because the inputs either produce that time or they do not. The cost people fear is worth measuring rather than fearing: one trial is about 1200 ticks of integer arithmetic, once, at the end of a run somebody spent half a minute driving.

**The rules belong in the version.** `build.rs` hashes `rules.rs` into the wire version alongside the message shapes, because a change to how a racer handles invalidates every stored log exactly as surely as a change to a message shape does. A log carries the version it was recorded under, and one from a different version is refused rather than replayed wrong: replaying it would produce *some* run, and that run would be a lie about what its player drove. This is the friendly kind of failure, an honest player and a valid log and a world that moved underneath it, and it is only friendly if something notices.

**The self check found a real bug on its first run, and it was not in the physics.** The client replays its own finished log and compares it to the run it just drove. On one machine, with one implementation, that should be impossible to fail. It failed immediately: `finished_tick` is the *index* of the tick a lap completed on, so the ticks taken is one more than it, and the client counted one way while the replay counted the other. Twenty milliseconds, invisible on screen, and it would have made **every honest submission** get refused for claiming a time its own log did not produce.

The general point is the one worth keeping. The check does not test the simulation, which has plenty of other witnesses. It tests the **recorder**, which has none: a recorder that closes a span one tick early produces a ghost that drifts away from the run it came from, slowly, in a way that reads as bad luck. Any system that stores events to rebuild state later wants this check, and it is four lines.

**Opponents can be free, and that is the same trick as a free wave of enemies.** The race mode puts three CPU drivers on the circuit, and the log does not grow by a byte, because a bot's input is a pure function of the world it is in. One player's key presses reproduce a four-way race, every shove and every stolen pickup included. The condition is strict and worth stating: the moment a bot reads a clock, a generator, or anything the log does not carry, the race stops being reproducible. So the sloppiness that makes the field feel real comes from a **hash of the tick and the seat** rather than from a random number generator, since a generator is hidden state that a recording would have to save and restore.

That noise is sampled in chunks of ticks rather than per tick, for two reasons that turned out to be the same reason. A driver whose mind changes every tick reads as a twitch rather than as a mistake. And a driver whose mind changes every tick produces one log entry per tick, which is exactly what stops an event log being small. **How still an input holds is both a feel property and a bandwidth property**, and they push the same way.

**A control that acts on nothing is worse than a control that acts on something dull.** The racing example shipped with latency, jitter and loss sliders that were wired only to the offline test harness. On a real host they moved nothing at all. That is a nastier fault than it sounds, because the example's headline claim is "latency cannot affect your lap": a player drags the slider to 400 ms, sees no change anywhere on the screen, and has just been shown a convincing demonstration of a thing that was not being tested. The impairment now runs on the real path (the verdict and the ghosts go through a per-seat link, and a submission can be dropped), so the claim is made against a link that is genuinely bad rather than against one that is not there. **If a demo has a switch, something has to be able to fail when it is thrown.**

**Latency is genuinely not on the path, which is worth stating once.** Four runs at 0, 80, 250 and 400 ms one way produce *identical* times, because the run happens on the machine driving it. Every other playground here spends its effort making latency cheap; this is the one that can say it costs nothing, and the reason is architectural rather than clever.

### A browser build is a phone build, whether or not anybody meant it to be (all the playgrounds)

Every playground here ships a wasm bundle, so every one of them is one URL away from a phone. Two of them, `bomb_grid` and `pellet_maze`, were driven entirely by `WASD` and had **no pointer input at all**: the page loaded, the game ran at full speed, and nothing a finger could do would move anything. That is a worse failure than a crash, because it looks like a working demo.

**macroquad synthesises a left click from a touch by default**, which is why the examples driven by *tapping* were already fine without anybody thinking about it: a menu, a build strip, a tile to place a tower on. `is_mouse_button_pressed` fires from a tap at the same coordinates.

**It does not cover anything held, and it does not cover two at once.** A synthesised mouse is a single pointer, so "steer left while charging" cannot be expressed through it. Anything holdable has to read `touches()` directly, and take the mouse only as one extra pointer when there are no touches, or one finger reads as two.

Two smaller decisions that were not obvious until they were made. **A discrete game wants buttons, not a stick**: three of these take one of a few values, and thresholding an analogue drag back into them is a threshold to get wrong, where a stick is right for the two that steer continuously. And **the controls stay hidden until the process sees its first touch**, latched rather than sampled, because a thumb pad drawn over a desktop window is clutter in the middle of what a player is looking at, and one that appeared and vanished between taps would be worse than one that was never there.

### Three bugs that made the example look like it was working (hit_scan)

**Symptom.** Nothing. That is the entry. The rifle was devastating, the bot stood its ground, and the movement looked smooth. Each of the three was found by a test that had been written to assert something else.

**The shooter was in their own ray cast.** A ray starting at the centre of a body hits that body at zero distance, so every trigger pull resolved as a hit on the shooter and no shot ever reached anybody else. The first duel test came back `hit == Some(0)`, where 0 was the shooter. Read as a hit rate, it is a weapon that never misses.

**The prediction ran a playout depth early.** `press` set the held direction as well as scheduling it for the tick it named, so the client walked from the keypress and the server walked from the tick, and the two took the same route out of step. 240 corrections in eight seconds, and on a continuous body every one of them is a few units that easing hides. This is exactly the second bullet of [bomb_grid's entry](#four-bugs-with-one-shape-bomb-grid), written down, cross-referenced, and then reintroduced by hand in a different file. **A lesson in a README is not a lesson in the code**; the thing that caught it was bomb_grid's *test* ported over, not bomb_grid's prose.

**A bot wedged against a wall for the whole session.** Steering is quantised to eight directions, so a bot pressed against a vertical face while wanting to go a few degrees off due west resolves to due west, which has no vertical component left to slide along. It pushed into the wall for nine and a half seconds of test time without moving. A stationary bot is a *plausible* bot, which is why this is the same shape as pellet_maze's load-bearing bots one section up: the failure state and the healthy state look identical from outside.

**The general form.** A bug that breaks an example loudly costs an afternoon. A bug that leaves it running costs however long it takes somebody to distrust a number. All three of these had a plausible reading, and none of them would have been found by playing the thing. The defence is not more care, it is asserting the *premise* as well as the logic: that a shot can miss, that latency alone produces no corrections, that a bot covers ground.

### A curtain of forty bullets, measured very precisely (curtain_fire)

**Symptom.** Every test passed and every number was wrong by an order of magnitude.

**Cause.** The enemy curtain is a closed-form function of the tick, and the emitters were written to release one bullet per period regardless of pattern. That is correct for a spiral and wrong for a ring, which is *defined* by releasing a salvo at once. So the "ring" was a slow spiral, the field peaked at forty bullets, and the entire point of the example, that a derived half costs a fixed number of bytes where a streamed half costs per bullet, was being demonstrated on a field small enough for the streamed half to be perfectly affordable.

**Fix.** Salvo size became a property of the pattern, and the emit loop starts at the oldest still-live salvo rather than at zero, so evaluating a wave costs the same at the end of it as at the beginning. The test now asserts density directly, per pattern, and separately asserts that the densest one is a real curtain.

**The general form.** **A demo's premise needs a test as much as its logic does.** A thin curtain and a thick one take the same code path, so nothing failed; the byte comparison was arithmetically correct about a situation that was not the one being claimed. Any example whose point is "at scale, X beats Y" needs an assertion that the scale is present, or it will confirm itself at whatever scale it happens to have.

### A harness that aimed like an oracle (hit_scan)

**Symptom.** A skirmish test ran, ships shot each other, every verdict came back `Plain`, and the panel's whole reason to exist read as zero.

**Cause.** The harness aimed at the server's position for each target, because that was the position it had to hand. Lag compensation exists to reconcile *the shooter's view* with the present, so a shooter that aims at the present has nothing to compensate: the rewound world and the current world both contain the target, every shot is `Plain`, and the granted-by-rewind counter is honestly zero. The test was measuring a scenario in which the feature is inert.

**Fix.** The harness reads `client.render()` and aims at what that client is drawing, which is what a player does.

**The general form.** This is [`mean_render_error`](#the-general-form-4)'s mistake one level up, and worth stating in its general shape because it has now appeared twice in this repository: **a measurement taken against the authority is not a measurement of what a client experiences.** Wherever a harness reaches for server state because it is convenient, ask whether the thing being measured is defined in terms of what a client had. If it is, the convenient number answers a different question and looks fine doing it.

### A rate check credited per request is a rate multiplier (gow_3d)

`gow_3d` gives the client authority over its own position, so the server's only defence is asking whether a claimed position was reachable. The obvious form is to measure each claim against the time since the last accepted one. It has a false-positive problem: claims arrive between ticks, so two that bunch up measure zero elapsed against each other and the second is refused **for arriving together**. Jitter alone produced refusals, and refusals are the only signal the design has that somebody is cheating.

The obvious repair is to credit a minimum, one tick of clock grain, because the server cannot resolve anything finer. That fixed the false refusals and opened a hole twice as bad as the one it closed. **Whatever you credit per request is a rate the caller can claim at will.** A client sending twice per tick was credited a full tick each time, and a 2.0x speed cheat passed at a full 2.00x, up from being stopped dead.

Nothing about the code looked wrong in either version, and the unit tests for both passed. What caught it was a table printed by a test that had been there the whole time: a row reading `2.0x  achieved 13.44  gain 2.00x` where every previous run had read `1.35x`. The lesson about the table is separate and larger than the bug: **a test that prints a number a human reads is a different instrument from one that asserts a bound**, and this one caught a regression no assertion in the file was watching for.

The correct shape is a **budget that accrues from the clock and is spent by movement**, rather than an allowance recomputed per request. It cannot be gamed by asking more often, because it accrues from elapsed time however many times it is asked: two bunched packets spend one budget between them, and two whole steps in no elapsed time are refused, because that is not a bunched packet, it is twice the speed. It also has to be capped, or a disconnection is a teleport.

This generalises past games. Any check of the form "is this request allowed given the time since the last one" has the same two failure modes, and the same fix: rate limits, token refills, retry backoffs, and quota accounting all want an accruing bucket rather than a per-request allowance.

### A protocol hash over the ops misses the types they carry (poketo)

`plaza_wire::build::emit(&[paths])` hashes the files you list. poketo listed `protocol.rs`, which is where its ops are defined, and the ops carry types from two other files: `Overworld` embeds `Trainer`, `BattleState` embeds `Battle`. A creature could gain a field, or a tile change shape, without the version moving at all. Two builds that disagreed about the wire would complete the handshake, agree they matched, and then mis-decode, which is worse than refusing to connect because it looks like a game bug.

The repair that only fixes the instance is adding the two files to the list. The repair that fixes the class is `Wire::detect()`, which starts from types tagged `/// plaza-wire: root` and walks their fields transitively, so a payload two files away counts and a type nobody references warns rather than silently drops out. **Walking the fields is the only version of this that a person cannot forget to update**, and a hand-maintained list of what is on the wire is exactly the artifact that goes stale during the refactor that needed it.

Verified rather than assumed, because a version hash is the kind of thing that looks right whatever it does: adding a field to `Creature`, two files from the ops, moved the version from 3864428394 to 561229205, and reverting restored it exactly. Under the file list it did not move.

The resolver also caught something the hash was never the point of. gow_3d had **two `Authority` types**, one in `controls.rs` and one in `protocol.rs`, with a `match` converting between them; the resolver refused to build, naming both. That is the two-derivations-of-one-fact shape this catalogue keeps returning to, and a build step that indexes wire types by name turns out to be a detector for it.

### Smaller ones worth remembering

**Three measurements whose scenes could not show the effect (gow_3d).** A tower test comparing a height filter against a volumetric grid, where the tower was 40m tall and the view radius 30m, so the volume grid's vertical reach covered the whole building and excluded nobody: both arms examined all 480 people and the comparison had no contrast. A walk test that started every character with a 12-unit jump onto the axis it was walking them along, so the validator was right to refuse it and the refusal count measured the test rather than the code. A cheat-cap test whose jump was larger than the cap could ever bank, so it landed nothing and "gained no more than an honest runner" passed with **zero gained**. Same shape three times: the code was fine, the scene could not produce the phenomenon, and each one returned a plausible number rather than an error. The fixes are all assertions about the *scene* now (the tower must out-reach the view; the cheat must land jumps before the cap is checked), because a fixture that silently stops exercising the thing is worse than one that breaks.

**A test lane that ran straight through two pillars.** Every ray test in hit_scan's first draft failed, none of them for a reason involving the code: the obvious horizontal line across the arena at y 100 crosses both side pillars. It is a named constant now (`OPEN_LANE_Y`). Geometry fixtures deserve the same suspicion as the numbers they produce, and a fixture that is wrong fails loudly only when you are lucky.

**Ctrl-C would not kill the windowed host.** Actix caught the signal for a graceful shutdown while the window kept running, and the controller sprayed queue-full errors into dead links. Fixed with `disable_signals`, leaving signal handling to the process.

**Turning coins off crashed a joiner.** The client indexed its own wallet in a list the server stops sending when the feature is off. The first joiner takes seat three, so it indexed three into an empty list. Latent in the offline build too, where seat zero happened to be safe.

**Changing the enemy count made the horde lunge.** Rebuilding the world reset the server clock to zero, so every client's packet-age estimate spiked and projected samples wildly forward. Fixed by preserving the clock across a rebuild.

**The offline build refused to start.** The default role is host, which requires a server, and the teaching build deliberately compiles no networking at all. The role check now only runs in builds that have networking.

## The diagnostic playbook

What worked, repeatedly, after several rounds of confident wrong guesses:

**Instrument before theorising.** Every hypothesis in this document that was formed from reading code was wrong. Every one formed from captured data was right. The pattern that worked was: add a targeted diagnostic behind a switch, have a human play for a minute, read the output, fix once.

**Print the discriminator, not the context.** The first correction log reported the server's dash flag, which reads true throughout a grapple because dashing is how a grapple is fought. It implied a cause that was pure coincidence. Adding the distance to the nearest hole made the actual story readable at a glance. Choosing what to print is part of the diagnosis.

**Make thresholds adaptive.** A thirty pixel correction means nothing without knowing the send rate, the latency and how much contact is happening. Tracking a running mean and variance and reporting deviations from it survives changing conditions, where a constant tuned once does not.

**Read the shape of a counter.** Climbing without bound means systematic. Plateauing means transient and self-healing. A regular sawtooth means a threshold. Each shape named a different class of cause and each was right.

**Compare against the offline build.** Both examples keep a single-process build with the same simulation code. Any counter that reads zero there and non-zero over a socket isolates the fault to the transport or the code wrapping it, which is a very large search space removed for free.

**Check the harness before believing a zero.** A measurement of the ghost read zero at every setting, and the finding was nearly written up as "there is no unresolved state." The harness had no acknowledgement loop, so the server's baseline never advanced, every frame was a full rebuild carrying no samples, and the number was zero for a reason with nothing to do with the question. It cost several rounds. A zero from a harness you wrote is a claim about the harness until you have shown the harness can produce a non-zero.

**And writing that down did not prevent it happening again.** A later harness, measuring what a player costs the server, was built the same way and inflated both its bandwidth and its CPU figures, which were quoted in a doc comment before the zero in the sample column was noticed. The lesson as prose is not load bearing; the fix is that `examples/players.rs` now *asserts* its sample bytes are non-zero, so the harness fails rather than reporting. Any measurement harness over an acknowledged stream should carry the same assertion, because the failure produces numbers that look entirely plausible.

**An assertion whose failing side cannot occur passes for the wrong reason.** Written three times in one sitting, each time reading as a real check. `error.min(CONST)` and then `assert!(error < CONST)`, which the clamp guarantees. `assert!(gained <= honest)` in a scenario where the cheat's jump exceeded the cap it was testing, so `gained` was **zero** and the bound held vacuously. `assert!(health >= was.saturating_sub(was))`, which is `health >= 0`. None of them can fail, all of them look like the thing they were meant to check, and a suite full of them is green for the same reason an empty suite is.

The property they share is that the *setup* had quietly stopped producing the phenomenon, so what remained was a comparison against a value that could not lose. That points at the fix, which is not proof-reading assertions harder: **assert the setup produced the phenomenon, before asserting anything about it.** The cheat has to land jumps before its average is worth bounding; the first choice has to deal damage before a resend is worth checking; the tower has to out-reach the view before two strategies are worth comparing. Those guards are cheap, they fail loudly when a fixture drifts, and they are the only thing that distinguishes a test that passes from a test that ran.

**A test that pins a number can be pinning a coincidence.** One assertion required *zero* contested pickups lost. It held at one configuration and broke at every other, and measuring across the range showed a steady 1 to 3 that does not improve with a wider buffer, which is the signature of a float tie-break rather than staleness. The assertion was over-fitted, not the code wrong. Prefer asserting the shape (rare, bounded, monotone) over asserting a value that happened to come out round.

**A green test proves nothing until you have watched it fail.** Three of the four bomb grid fixes came with a test that passed on the first run for the wrong reason: the player walked into a wall and stopped, so "it stopped" held whether or not the fix worked. Each only became a test after the assertion was moved somewhere it could fail (mid-walk, short of the wall) and the fix was disabled to confirm it did. It costs a minute. A test written from the same theory as the fix cannot falsify that theory, and passing one feels exactly like being right.

**A counter that reads zero right after you fixed it is a claim about the counter.** Already in this file as a harness lesson; it applies just as hard to a readout. After four fixes the snap counter read zero, which is either the netcode working or the detector broken, and the two are indistinguishable until loss is dragged up and the number climbs.

**Instrument by elimination when the theories run out.** The residual in bomb grid survived three fixes and every remaining hypothesis was wrong when tested. What found it was adding readouts that could *rule things out*: the client's simulated tick against the newest frame's tick, and the server's late-input counter which the panel had been hiding. Once those said "clock healthy, no inputs late, no loss", the only thing left unmeasured was the authority's own step size, and that was it. Choosing instruments that can exonerate is as useful as choosing ones that can accuse.

**A shadow A/B beats toggling.** To answer whether predicting the dash is worth it, run two predictors over identical inputs differing only in that flag and compare their mean error. One session, no toggling, no reliance on remembering how the last run felt.

**A test written from a theory tests the theory.** Two fixes for the resume bug above shipped green: each came with a scripted socket test that reproduced the author's *model* of a resume and proved the fix worked against it. Both were wrong in the browser. A test built from the same assumption as the fix cannot falsify that assumption, and passing it feels exactly like being right. What broke the loop was instrumenting the real client, playing until it happened, and reading numbers nobody had predicted.

**Verify the test discriminates before believing it.** Once a fix is written, disable it and check the test actually fails, then restore. The tick-floor test reads `-6213` with the floor removed and passes with it, so it is measuring the fix rather than the weather. This costs one minute and is the only thing separating a regression test from decoration.

## What this changed in plaza

The principles above are guidance. These are the code changes the bugs argued for, all of them shipped, and each one stayed inside the north star in plaza's north star: one concern, usable alone, generic over application types, additive to the existing primitives. The application still owns its payloads, its physics, its socket and its tick.

**Prediction can see the world it predicts in.** `PredictedPlayer` gained a context parameter, because a client predicting a *forced* entity had nowhere to put the forces and black hole was smuggling the whole gravitational field through every buffered input. Being unable to pass the world in is exactly what pushes people into writing the second, lesser rule that the first principle warns about, so this was an API deficiency rather than a game problem.

**A predicted entity is not always being simulated.** `set_active` and `teleport`, because nothing expressed "the server is holding this still" and black hole's client was inventing a correction every packet for a hole frozen through a respawn delay.

**Reconciliation reports what it did.** `reconcile` returns a `Correction`, and `CorrectionMonitor` keeps the running statistics both examples had hand-rolled, twice, including the same adaptive outlier test.

**Both server input models are supported.** `HeldInputPredictor` joins `PredictedPlayer`, because replaying discrete inputs against a server that holds a direction and integrates it double counts, which is why horde had abandoned the primitive and hand-rolled a worse one. The two models are now named in the crate docs so the choice is deliberate rather than discovered.

**Relevance streams have reliable bookkeeping.** `server_utils::DeltaBaseline` owns the per-subscriber baselines, the acknowledgement frontier, the staleness rebuild and the digest drift check. Two of the worst bugs here were defects in that bookkeeping rather than in the game, and it never learns what a key means.

**The impairment link is transport-faithful.** `net_sim::LatencyLink` defaults to ordered delivery, and both private copies of that queue are gone. Black hole kept one for a while after horde was migrated, and it was still the unclamped version: at the shipped defaults (15 ms of jitter against a ~16 ms send interval) it could hand its own client an older frame after a newer one, which the pellet stream has no tolerance for because `swallowed` and `spawned` are order-sensitive. A fix that is applied to one of two copies is a fix half the codebase does not have.

**The client half of the delta stream is a block too.** `client_utils::DeltaMirror` is the exact counterpart of `DeltaBaseline`: it applies the packet, checks generations, counts the sequence gaps, folds the digest and compares it. Shipping only the server half meant anyone adopting `DeltaBaseline` got no help writing the side that has to agree with it, which is precisely where every serious bug here lived. It also carries the rule that was previously a comment in one example: **apply every packet whatever baseline it names**, because these deltas carry absolute values and are therefore idempotent, and a client that discards what it cannot rebase starves its own mirror instead.

**Both sides now share one definition of the things they must agree about.** `SlotKey` (the `(index, generation)` pair and its `u64` packing) and `SetDigest` (the fold) live in the client crate, and `server_utils` re-exports them. Two implementations that agree today are a disagreement waiting to happen, and the failure would present as a divergence about the *world* rather than about the arithmetic, which is a genuinely bad afternoon to spend.

**Seats are a block, and freshness is part of their type.** `server_utils::SeatTable::seat` returns a `Seating` rather than an index, so `Fresh` and `Existing` cannot be collapsed. That distinction is exactly "reset this seat's accumulated state" versus "do not", and forgetting it is the warm-arena join bug that opened this whole investigation. Making the caller match on it turns a thing you have to remember into a thing the compiler asks about.

**The listen-server scaffolding was duplicated too, and it was not trivial.** `plaza_session::host::Host` owns the HTTP layer: the stamped index, the revalidation headers, the preflight on the served directory, the banner, and leaving signals to the process. `plaza_wire::build` owns the version hashing, which had been copied byte for byte between the two examples.

**"Where can this go" and "does this belong in the library" are separate questions.** The four roles and their argument parsing had to move out of both playgrounds, and they could not move into `plaza_session`, because the browser client needs the same vocabulary and a wasm bundle must not inherit an HTTP server to learn the name of its own role. That forced a dependency-free crate. It was tempting to read the constraint as a design and publish it, but argument parsing is an opinion every real application already has, and it would have made a published crate depend on a CLI. It lives under `examples/` as shared scaffolding instead. Deduplication tells you code should have one home; it does not tell you that home is the library.

**A fixed timestep is a five-line pattern with three decisions in it.** `client_utils::FixedTimestep` and `Periodic` replaced six hand-written accumulators (the filed count said five, which is its own evidence). The decisions each copy made differently: whether to cap the catch-up after a backgrounded tab, whether to carry the remainder or zero it, and whether the thing being stepped gets told the step size. The last is the one that bites, because a client integrating by its frame delta against a fixed-step server drifts continuously and it reads exactly like network jitter.

**Coalescing is a policy with a trap in it.** `client_utils::InputCoalescer` carries the keepalive, and the reason for it: sending purely on change means a *dropped* direction change is not a missing update but a wrong state that persists, because the server holds the last direction it received. The player keeps gliding until they press something else, it is intermittent, and it reads as the controls sticking rather than as packet loss.

**An input schedule has to say which way it refused.** `InputSchedule` splits its rejections into closed-tick and too-far-ahead and keeps the last margin in ticks, because a single total says a player cannot act and nothing about why, while the two sides have opposite causes and opposite fixes. Nothing on the client can substitute for it: an input is acknowledged on arrival, before admission, so a refused input and an applied one look identical from there.

## What the wire work measured

**The byte cost was in the thing sent most often, not in the thing that looked expensive.** Horde's downstream sat around 70 KiB/s at the defaults, and the obvious suspect was the encoding: the server's bandwidth model priced a 3-byte id and a quantised position while the wire actually carried a two-element array per handle and two 5-byte floats per position. Making the wire carry what the model priced (one packed integer per handle, fixed-point `i32` pairs per position, `u8` for every fieldless enum) was worth about **1.5x**. Correcting each visible enemy four times a second instead of in every packet was worth **4x**, and it is not an encoding change at all.

**MessagePack writes enum variant names out in full**, which is not obvious from its reputation: every spawn was carrying six bytes of `"Swarm"` and every departure eleven of `"OutOfRange"`. Compact mode drops *struct field* names, not variant names. Where a fieldless enum rides a hot path it wants an explicit `u8` mapping, with the numbers pinned in the conversion so reordering the variants cannot silently renumber the wire.

**A quantised wire needs a reason per field, not a policy.** Positions cross at 1/16 of a unit because that is two orders of magnitude under the smallest enemy radius and nothing reconciles against an exact float; handles cross as the packing the digest already uses, so there is not a second packing to disagree with. Both are `#[serde(into/from)]` conversions, so the simulation stays `f32` and the quantisation exists only on the wire, which is the only place it is affordable.

## What building the blocks taught, on top of what the bugs did

These came out of the extraction rather than the debugging, and two of them appeared twice in different types, which is what makes them worth stating.

**Cold start is a distinct state that needs its own answer.** `CorrectionMonitor` alarmed loudest at startup, because a baseline initialised to zero says every correction is enormous. `DeltaBaseline` had the naive bug hiding inside its own recovery mode, because recovery diffs against the acknowledged state and there is no acknowledged state before the first acknowledgement, so it silently fell back to the very behaviour it existed to replace. Same shape both times: a mechanism defined in terms of accumulated state has undefined behaviour before that state exists, and the accidental fallback is usually the naive thing. Worth asking of anything that learns: what does this do on sample zero, and is that a decision or an accident?

**Attach an integrity check to the report, not to the state change.** The digest comparison originally ran only when an acknowledgement advanced the frontier. A client re-acknowledging the same sequence still reports what it is holding, and a mirror that loses something *without* losing a packet reports it exactly then, so the check skipped the one case it was for. A client telling you its state is information whether or not your model moved.

**A richer key can delete an entire side channel.** Horde carried an out-of-band death announcement with a long comment about why it was unavoidable. It was unavoidable only because the diff was keyed by index. Once the diff moved into `(index, generation)` space, a dead slot retracts the occupant the client was told about and a reused slot reads as despawn-then-spawn on its own, and the whole mechanism was deleted. What your key identifies decides how many side channels you need.

**Take the function, not the trait.** Three places wanted a distance and none of them added a bound: `reconcile` returns two states and lets the caller subtract, `with_teleport` takes the metric as an argument, and `DeltaBaseline` takes a digest as a `u64`. A trait bound would tax every user for the benefit of the few wanting telemetry.

**Derives are part of the API contract.** `LatencyLink` was not `Clone`, and that alone is why horde reimplemented it, which is how a transport-faithfulness fix ended up living in one example instead of the library. A primitive that cannot sit inside application state will be reimplemented, and a plaza state must be `Clone`.

**A second consumer is what proves an extraction, and a straggler is what disproves it.** Every block here was written against horde and retrofitted to black hole, or the reverse, and each retrofit found something: a divergence in what the two meant by a "fresh" seat, a rate that divided by zero on the first frame, and one impairment queue that had simply never received a fix the other one had. The rule that falls out is not "extract early" but "extract, then go and find the other copy", because the copy nobody migrated is the one still carrying the bug.

**Name tests after the bug, not the function.** The horde retrofit rewrote the most-debugged code in the repo and shipped on a single test run, because six tests are named for actual historical failures. A test named for the behaviour under test says something broke; a test named for the bug says which regression you just reintroduced.

## Deployment: the failure that is not in the code at all

The browser client is a wasm bundle, a gitignored build product, and it does **not** rebuild when the server does. A page built before a wire change still loads, still appears to run, and only the messages whose shape changed are rejected. That combination reads as a netcode bug and is a deployment one, and it cost two rounds of diagnosis.

Three things now prevent it, and they are worth having together because each covers what the others cannot:

- The host serves `index.html` dynamically with the wasm's modification time stamped into the URL, read per request so rebuilding the client reaches an already running host without restarting it.
- Static assets are served `no-cache`, which is what makes the stamp effective: a cached page would keep quoting the old stamp, which is the trap that makes cache busting look like it does not work.
- A client announces its wire format in `Op::Hello` on connect, and a server that speaks a different one replies `Op::Outdated` so the page can say "reload" instead of the server flooding its log with per-message decode warnings. The version is not a constant anyone maintains: `build.rs` hashes the source files that define the messages, so the server and the bundle agree exactly when they were built from the same code. A version that has to be bumped by hand is skipped precisely when it matters, which is during the change that needed it.

That last one errs toward asking for a reload that was not strictly needed, since it changes when those files change at all, comments included. That is the right direction to be wrong in: the cost is a page load, and the opposite mistake is the silent half-working session the whole mechanism exists to prevent. It also cannot rescue a client older than the handshake itself, which is the bootstrapping floor every protocol version has.

## One rate is not enough, and the metric could not see why

A late finding, from playing rather than from a test. At a 1 Hz send rate the horde *looked* like 1 Hz, which contradicted both the case study and this project's own measurement that running the enemy rule locally beats interpolating at low rates by a wide margin (12 px mean error against 57 px).

Both were right, and both were measuring the wrong thing. `mean_render_error` compares an enemy's position against server truth, and that number is genuinely good, because every client runs the enemies' own rule. What it cannot see is **continuity**, and what it does not cover at all is **players**.

Two things were actually running at 1 Hz. Remote players had no smoothing whatsoever, a bare `self.players[p] = pos` per packet, so peers teleported once a second. And, worse, `step_enemy` aims at `players[target]`, so the whole horde was gliding smoothly toward a point that jumped once a second: at `PLAYER_SPEED` that point is up to 190 px stale, nearly half a view radius, while a Swarm enemy covers only 62 px in the same second. A *synchronised* heading change across hundreds of entities is far more visible than the same magnitude of error scattered randomly, and a positional mean cannot express it at all.

**The fix is two send rates, because the two streams answer different questions.** Enemy positions may be stale, because they are the behaviour's *output* and every client recomputes it. Player positions may not, because they are the behaviour's *input*. This is the case study's own principle, which was recorded here and then not applied: sync the input to the behaviour, not just its output. Their 1 Hz was enemies only; players are a handful of entities and cheap to send often. Horde had a single global `sync_hz` that starved both together.

Measured on the fixed build: with one shared 1 Hz rate a peer is up to **204 px** behind the truth, matching the predicted 190; splitting the rates (1 Hz entities, 30 Hz players) takes it to **7 px**, and the entity stream is untouched.

Remote players are now drawn through `RemoteView`, which is what that block is for, so a peer is interpolated between samples with dead reckoning on starvation. Deliberately kept separate from the authoritative array the *rules* read: interpolation is presentation, and the rules keep consuming the authoritative sample, which is the principle that stopped the horde lunging in the first place.

The general lesson, and it is the sixth time in this project: **an error metric and a smoothness metric are different measurements, and averaging position error hides every discontinuity.** If something looks wrong and the numbers look right, suspect that the numbers measure a different quantity than the eye does.

### And then the first attempt at the fix made it worse

Worth recording in full, because three separate defects hid behind one symptom and the log line that exposed them was nearly deleted for being noisy.

**Interleaving two streams into one position.** Players now arrive on both the entity packet and the player frame, and those travel the same delayed link at different rates, so a packet built earlier can arrive *after* a newer player frame. Taking it anyway walked the authoritative position backwards in time, and that position is what the enemy rule reads. Samples older than the newest for that player are now rejected.

**A velocity derived from whatever two samples happened to be adjacent.** Across two interleaved streams the gap between samples can be a millisecond or two, and a small position difference over a smaller time is a spike. The view then dead reckoned along it. A minimum gap before a velocity is recomputed fixed it.

**A render target computed the wrong way, which is the general one.** The target was `now_ms - one send interval`, where `now_ms` is an estimate of server time *now*. A sample is a link delay old by the time it arrives, so that target sits permanently *ahead* of the newest snapshot and the view never interpolates at all: it extrapolates, hits its cap, and holds. On a host it is worse, because pongs return instantly while frames go through the impairment link, so the two disagree by the whole latency. The fix is [`InterpolationClock::resync`], which steers the render clock toward the stream, so the target trails the newest sample without knowing the latency, the jitter, or how good the clock sync is.

**The library bug underneath, found by the person playing it rather than by a test.** Past the extrapolation cap, `ExtrapolationBase` returned the *un-extrapolated* state. At the cap an entity has coasted `velocity * max_ms` forward; one millisecond later it was drawn back at the raw sample, a jump of the entire window in the wrong direction, flickering whenever a jittery target crossed the boundary. It now caps the *duration* instead, so the entity coasts to the limit and stops there. **Two tests asserted the old behaviour**, which is the tcp double-encoding lesson again: a test written from the implementation pins the bug rather than the requirement.

**And the technique was there all along.** Gambetta's entity interpolation, which plaza implements and `netcode_playground` demonstrates, is *render in the past between two real snapshots and do not extrapolate*. Peers were being rendered with `RenderOpts::default()`, which has `extrapolate: true`. Dead reckoning a **player** is guessing at a human's intention, which nothing on the wire carries, so it overshoots every direction change and snaps back when the truth lands. Peers now interpolate only, trailing by two send intervals so two snapshots always bracket the target.

**Rendering in the past requires a history, not just the newest snapshot.** Putting every remote entity on one delayed timeline is right, and it exposed which entities can actually *be* on it. A peer can, because `RemoteView` keeps a snapshot buffer to interpolate within. A projectile could not: the client held only the newest list and replaced it wholesale each packet, so once the server stopped listing a shot (it hit something) there was nothing left to draw it from. Any shot fired and destroyed inside the render delay had therefore never existed at the target and was silently dropped. Measured at a 4 Hz player rate, that was **every** shot: none were drawn at all.

The rule that falls out: **an entity can join a delayed timeline only if the client can reconstruct its state at an arbitrary past instant.** That means either buffering its samples, or holding enough to compute it.

For a shot the second is nearly free, and it is what the wire now carries. A `ProjectileSpawn` is an origin, a velocity and the time it was fired, sent **once** as an event; the client flies it locally and can evaluate it at any instant exactly. That put shots back on the shared timeline for real, and the measurement is unambiguous: the number of shots actually drawn went from 0.2, 0.1 and **0.0** per frame at 30, 16 and 4 Hz to essentially rate-independent. The *held* count still rises as the delay grows, because a shot fired after the instant being rendered is queued until the timeline reaches it, which is the buffer working rather than a loss.

It is also the same move as everything else that has worked here, one level down: **send the input to the behaviour, not the behaviour's output.** Re-sending a live projectile's position every packet is sending the output of an equation both sides can solve. It is cheaper too, since a shot costs one message instead of one entry in every packet for its whole flight.

**What extrapolation is actually for, since it caused all of this.** It is the *starvation* fallback: when the render target runs past the newest snapshot there is nothing to interpolate between, and the only choices are freeze or coast. It is not a general technique, and whether coasting helps is a property of the **entity**, not the game. It works when the next state follows from the current one, which is true of vehicles, projectiles and anything with inertia and a turning limit, and is where the term comes from (military simulation, where every entity is a vehicle). It fails for anything steered instantaneously by a person or an AI, because there the velocity is not a constraint on the future, it is a record of the past.

That gives a hierarchy, and plaza has all four rungs: run the entity's own rule if you know it and know its inputs (best, and what makes a 1 Hz enemy stream playable); interpolate between two real snapshots if you do not (safest, and what peers now do); extrapolate from one snapshot and a velocity only if the dynamics are predictable; hold if they are not. Reach down the list only when the rung above has no data.

**Do not quieten a warning that is doing its job.** The extrapolation warning was briefly downgraded to `trace` for being repetitive. It was the only thing announcing all of the above. It is back at `warn` and now says what a *steady* occurrence usually means, which is a render target ahead of the stream rather than a starved link.

**Where it ended up.** Peers on the delayed timeline was only a third of it, because everything else (deaths, sparks, claims, health) still applied on arrival, so the scene was being drawn from several clocks at once. The client now queues packets and applies them when its render clock reaches them, and a token type makes drawing at any other instant inexpressible. Inputs went the other way at the same time and for the same reason: the server buffers them by tick and executes in tick order rather than on arrival. Both are the same idea from opposite ends, which is that **one side owns the clock and everything else is scheduled against it**, and the two principles above are what that decomposed into.

[`InterpolationClock::resync`]: https://docs.rs/plaza_client_utils

## What is deliberately not predicted, and why

Worth stating so nobody "fixes" these:

- **Black hole, collision separation between holes.** Predicting it requires predicting the other holes' motion, which means running the whole field forward rather than one entity. Left as a correction, and it is the residual visible during close grapples.
- **Black hole, the dash, when the toggle is off.** Kept selectable specifically so the cost of not predicting an ability is demonstrable rather than described.
- **Horde, the local player, entirely.** Nothing about it is predicted any more. It is drawn from the played-out stream at the same instant as everything else, because against a scheduled server a prediction is a second world rather than a head start. Black hole still predicts its own hole, and correctly: its server applies input on arrival, so there is no schedule to disagree with.
- **Both, remote players.** They are interpolated or simulated under the shared rule, not predicted, because there are no local inputs to predict from.

## What the host legitimately sees that a joiner does not

The host is the server, so its omniscient readouts are honest rather than cheating: the truth overlay, bandwidth accounting, digest mismatches, phantom counts. A joiner sees only what a real client can see. That difference is the point, and the impairment sliders act on real outbound connections so the host can show a joiner what two hundred milliseconds feels like. The host deliberately keeps feeling its own impairment settings, so the effect is symmetric and comparable.
