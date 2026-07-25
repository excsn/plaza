# Learnings from the playgrounds

Everything the black hole and horde playgrounds taught while they became real listen-servers: the principles that prevent whole classes of bug, the record of what actually broke and how each was found, and what all of it changed in plaza itself.

It is kept because in almost every case the cause was somewhere other than where the symptom pointed, and the wrong theories were reasonable enough to be worth writing down next to the right one.

The principles come first because they are free. No primitive prevents the bugs below; only writing the code differently does.

## Principles

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

### Techniques worth reusing

Running two predictors over identical inputs that differ in exactly one variable, and comparing their error, answers "is this prediction earning its keep" in one session instead of two, with no toggling and no memory of how the last run felt. It is only cheap because predictors are pure and hold no resources, which is an argument for keeping them that way.

Reading the shape of a counter rather than its value: a count climbing without bound is systematic, a count that plateaus is transient and self-healing, and a regular sawtooth is a threshold effect. Each shape pointed at a different class of cause, and each time it was right.

## The bug catalogue

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

### Smaller ones worth remembering

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

**A test that pins a number can be pinning a coincidence.** One assertion required *zero* contested pickups lost. It held at one configuration and broke at every other, and measuring across the range showed a steady 1 to 3 that does not improve with a wider buffer, which is the signature of a float tie-break rather than staleness. The assertion was over-fitted, not the code wrong. Prefer asserting the shape (rare, bounded, monotone) over asserting a value that happened to come out round.

**A shadow A/B beats toggling.** To answer whether predicting the dash is worth it, run two predictors over identical inputs differing only in that flag and compare their mean error. One session, no toggling, no reliance on remembering how the last run felt.

## What this changed in plaza

The principles above are guidance. These are the code changes the bugs argued for, all of them shipped, and each one stayed inside the north star in [IMPROVEMENTS.md](../IMPROVEMENTS.md): one concern, usable alone, generic over application types, additive to the existing primitives. The application still owns its payloads, its physics, its socket and its tick.

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
