# 03. A solver in the loop

The question this chapter answers: I want real physics in a multiplayer game, so what does a rigid-body solver cost me, and does the answer depend on which netcode I picked?

It does, and that is the whole chapter. A physics engine is not a component you drop in beside your netcode; it is a second thing that owns state, and every question the [previous chapter](02-choosing-your-netcode.md) asked about who is allowed to be wrong has to be asked again about it. Plaza runs the same engine, at the same version, in two examples with opposite configurations, and the reason is entirely the family each one belongs to.

Nothing here is a plaza block. Plaza has no physics and does not want any; a solver is [rapier](https://rapier.rs/) or your own, and these are the seams it meets.

## Determinism is a family question, not a preference

Rollback re-simulates. Every client runs the step and a digest proves the machines agree, so the solver must produce identical results everywhere. That is a hard requirement, and rapier's answer is the `enhanced-determinism` feature, which is **mutually exclusive with `parallel` and `simd8`**. Choosing rollback therefore chooses to give up the solver's threading, and no amount of profiling changes that.

State sync does not re-simulate. The server owns the only simulation, clients draw what arrives, and determinism buys nothing at all: two machines are never asked to agree on a step because only one machine takes one. `parallel` is free to take.

[puck_rink](../../examples/puck_rink/) and [cube_yard](../../examples/cube_yard/) are the same crate at the same pinned version with opposite flags, and the flags were not tuned, they were derived. That is the useful shape to carry away: the netcode family decides the physics configuration, so pick the family first.

Two consequences worth knowing before you commit to rollback with a solver.

**The version becomes part of your protocol.** Determinism holds same-version-only, and a build-time wire version cannot see it: `plaza_wire`'s hashes cover your *type definitions*, and neither a dependency bump nor a cargo feature changes one. puck_rink puts the exact build on the wire in `Physics::Rapier { pin }` so a peer compiled against another rapier is refused rather than left to diverge quietly, and folds the determinism feature into that pin, because a build with it and a build without it are two different simulations wearing one version number.

**Measure whether you need it.** puck_rink's rink does not: with the feature on and off, a native build and a wasm build produce byte-identical digests, because the backend uses only `+ - * /`, `sqrt`, `clamp` and `abs`, with no transcendental anywhere and no joints, and `enhanced-determinism` acts on exactly those two things. Add a joint or a motor and that stops being true immediately. The point is not that the feature is useless, it is that it is checkable: compile the step to wasm, run it under node, and diff the digests against native.

## A solver's state does not fit in a snapshot

This is the seam that surprises people, and it costs a message.

The state your game thinks it has is positions and orientations. The state the solver *runs on* is that plus contact manifolds, islands, and sleeping flags, carried between ticks and never present in a view. A client reconstructed from a view therefore starts with a plausible-looking world that diverges on its first contact.

For state sync it does not matter, because nothing reconstructs anything. For rollback it decides your join path: puck_rink's fixed-point backend is whole in every frame, so a joiner is caught up one tick after arriving and the rink shipped with no snapshot provider at all, while its rapier backend must hand over a serialized pipeline, measured at 4216 bytes. The [`SnapshotProvider`](../../core/API_REFERENCE.md) returning `Option` is what lets one backend decline and the other not.

If you take one habit from this chapter, take asserting it rather than assuming it: `a_view_cannot_seed_a_running_world` re-simulates from a projection and checks that it *diverges*, so the day someone makes the view complete enough, a test says so instead of a player noticing.

## What the solver gives back

**Sleeping is a bandwidth signal.** In a settled scene most bodies are not moving, and saying so costs one bit against the thirty-three a velocity costs. A hand-rolled simulation has to derive that; a solver already knows, and `!body.is_sleeping()` is the whole input to [`RestDetector`](../../server_utils/API_REFERENCE.md). In cube_yard's settled yard that is 905 bodies out of 905.

**Continuous collision.** puck_rink's fixed-point step dodges tunnelling by arithmetic luck: a puck of radius 6 capped at 6 units per tick can just barely never cross a wall in one step. Raise the speed or thin the geometry and you need CCD, which is genuinely unpleasant to hand-roll and which a solver has.

**And what it does not.** Kinematic bodies do not depenetrate against each other, so a paddle-on-paddle or player-on-player rule stays yours to write. Reaching for an engine does not empty the rules file.

## Quantise both sides, and when it pays

Glenn Fiedler's [state synchronization](https://gafferongames.com/post/state_synchronization/) names quantising the simulation on both sides as its critical trick: the server simulating at a precision it never transmits means the client is looking at a rounded copy of a truth that has already moved on.

With a solver it has a cost the articles do not mention, and it lands on the sleeping above. Snapping every body onto the wire's grid each tick took cube_yard's settled pile from 905 asleep to **0**: a resting body jitters by less than one quantisation step, so it is re-snapped forever, and writing a body's position marks it modified, which is enough that it never reaches the sleep threshold. Guarding on `is_sleeping` does not rescue it, because that is the state it can no longer get into. Key on **motion** and the circle breaks, leaving the rule that was always right: a body that is not moving is not drifting, so there is nothing for snapping to prevent.

Then measure it, because in cube_yard it buys nothing: 41894 bytes against 41806 over a settling yard, a difference of 0.2%. That is not a refutation, it is a statement about the example. The technique earns its place when the *client* extrapolates, running the simulation forward between updates, and cube_yard's client only draws. If yours simulates, quantise both sides. If it does not, you are paying for a guarantee nobody uses.

## Drawing what a solver produces

Worth a paragraph because it bit us. A rigid body tumbles, and a renderer that draws axis-aligned boxes cannot show that: macroquad's `draw_cube` takes a position and a size and no rotation. Rotating bodies want a mesh you rebuild each frame, which is also the fast path, 901 cubes for about 158us.

The trap is the batcher underneath. macroquad clamps a draw call at 10000 vertices and 5000 indices and *warns rather than fails*, drawing the front of the buffer, so one mesh of 905 cubes rendered about a quarter of the scene and the rest was simply absent. Chunk to stay under it. And read all of your spike's output, not the line you were hoping for: this warning was printed on every frame of the rendering spike that was supposed to catch it, and the check grepped for its own success message.

## The lab

[puck_rink](../../examples/puck_rink/) with `--features rapier`, which compiles both backends and lets the server pick at startup, so one scripted trace runs through the fixed-point step and the solver and prints them side by side. Then [cube_yard](../../examples/cube_yard/), which is the other family: 901 bodies nobody re-simulates, priced from 23.90 Mbit/sec down to 0.15 with an error column beside every row.
