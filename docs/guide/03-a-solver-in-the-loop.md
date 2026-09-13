# 03. A solver in the loop

This chapter covers what a rigid-body solver costs in a multiplayer game and how the answer depends on which netcode you picked.

A physics engine has its own state, so every question the [previous chapter](02-choosing-your-netcode.md) asked about who is allowed to be wrong applies to it too. Plaza runs the same engine at the same version in two examples with opposite configurations, because the two examples use different netcode families.

Nothing here is a plaza block. Plaza has no physics and does not plan to add any; you bring [rapier](https://rapier.rs/) or your own solver and this chapter covers where it meets plaza.

## Determinism depends on the family

Rollback re-simulates. Every client runs the step and a digest checks that the machines agree, so the solver must produce identical results everywhere. Rapier provides that through the `enhanced-determinism` feature, which is **mutually exclusive with `parallel` and `simd8`**. Choosing rollback therefore means giving up the solver's threading.

State sync does not re-simulate. The server runs the only simulation and clients draw what arrives, so determinism buys nothing: only one machine ever takes a step. You can turn on `parallel`.

[puck_rink](../../examples/puck_rink/) and [cube_yard](../../examples/cube_yard/) are the same crate at the same pinned version with opposite flags. The flags follow from each one's netcode family rather than from tuning. Pick the family first, since it decides the physics configuration.

If you choose rollback with a solver, two more things follow.

**The solver version becomes part of your protocol.** Determinism only holds between identical versions and a build-time wire version cannot detect a mismatch: `plaza_wire`'s hashes cover your *type definitions* and neither a dependency bump nor a cargo feature changes one. puck_rink puts the exact build on the wire in `Physics::Rapier { pin }` so a peer compiled against another rapier is refused rather than left to diverge quietly. It folds the determinism feature into that pin, because a build with it and a build without it simulate differently under the same version number.

**Measure whether you need it.** puck_rink's rink does not: with the feature on and off, a native build and a wasm build produce byte-identical digests. The backend uses only `+ - * /`, `sqrt`, `clamp` and `abs`, with no transcendental functions and no joints. Those are the two things `enhanced-determinism` affects. Adding a joint or a motor changes that. To check, compile the step to wasm, run it under node and diff the digests against native.

## Solver state and joining

The state your game thinks it has is positions and orientations. The state the solver *runs on* is that plus contact manifolds, islands and sleeping flags, carried between ticks and never present in a view. A client reconstructed from a view therefore starts with a plausible-looking world that diverges on its first contact.

This does not matter for state sync, because no client reconstructs the simulation. For rollback it decides your join path: puck_rink's fixed-point backend is fully described by every frame, so a joiner is caught up one tick after arriving and the rink shipped with no snapshot provider at all, while its rapier backend must hand over a serialized pipeline, measured at 4216 bytes. The [`SnapshotProvider`](../../core/API_REFERENCE.md) returning `Option` is what lets one backend decline and the other not.

Write a test for it. `a_view_cannot_seed_a_running_world` re-simulates from a projection and checks that it *diverges*, so if someone makes the view complete enough, the test fails instead of a player noticing.

## What the solver provides

**Sleeping.** A solver's sleep flag is a bandwidth signal, though at the solver's granularity rather than yours. In a settled scene most bodies are not moving and saying so costs one bit against the thirty-three a velocity costs. A hand-rolled simulation has to work that out; a solver already tracks it, so `!body.is_sleeping()` is the obvious input to [`RestDetector`](../../server_utils/API_REFERENCE.md). In cube_yard's settled yard that is 901 bodies out of 905.

Check what unit it applies to first. Rapier sleeps an **island**, which is every body in a chain of contacts, so one cube still jostling in a scattered heap reports every cube touching it as awake and each of those sends a velocity on the wire while holding still. In cube_yard this showed up as patches of a hundred-odd cubes drawn as moving while lying flat on the ground with nothing near them. The wire needs a per-body, purely local answer, so feed the detector "has *this* body moved recently" and let it count the run of quiet ticks: 205 cubes claiming to be awake became 56, against 57 that had actually moved.

**Continuous collision.** puck_rink's fixed-point step avoids tunnelling only because of its numbers: a puck of radius 6 capped at 6 units per tick can never quite cross a wall in one step. Raise the speed or thin the geometry and you need CCD, which is hard to hand-roll and which a solver has.

**What it does not provide.** Kinematic bodies do not depenetrate against each other, so you still write paddle-on-paddle or player-on-player rules yourself.

## The player's body and the solver

The most expensive mistake in cube_yard was giving the solver the one body a person is steering. Roll mode began as a torque, with friction turning spin into travel so that mass and momentum were real. It never worked. A torque only becomes travel through grip, so friction controlled everything else: with friction raised enough to stop the cube spinning on the spot, it measured 1059N of static friction against a 950N motor, the cube stopped dead and a gathered ball of cubes dragged it down to 0.4 units per second. Tuning any coefficient moved the problem somewhere else, because no setting worked for all of it.

A player pressing a key expects to move and expects to stop. A solver has no way to represent that intent. Driving the horizontal velocity directly and reading the **roll off the velocity that results** fixed the handling, made the spin always match the travel and removed the dependency on grip so completely that friction could drop to almost nothing. Gravity, jumping, contacts and the entire field stayed physical. Weight stayed too, as a coefficient on the drive scaled by what the player is carrying.

Give the solver only the bodies nobody is steering.

## Traps

**Collision filter fields.** Carried cubes were meant to stop pushing the player, so the player collider got `collision_groups` and the carried cube a solver filter excluding it. Nothing was filtered: `collision_groups` and `solver_groups` are separate fields and `solver_groups` defaults to `ALL`, so the player's default membership of everything satisfied whatever filter the cube named. A filter set on the wrong field silently does nothing. The suite stayed green through a whole sequence of follow-on fixes, each credited with an improvement it had not caused. Printing the player's contacts exposed it: the player was resting on four cubes it was supposed to pass through, with no floor contact at all. When a mechanism exists to *stop* something, write a test that checks it stopped.

**Persistent forces.** `add_force` and `add_torque` persist across timesteps until you reset them, so a field applied every tick grows without bound. Spin reached 46 rad/s against a cap of 4.6 and bodies were thrown two hundred units. A value that far out of range usually means something is being applied repeatedly rather than that a coefficient is mistuned.

**Ground checks.** Asking the narrow phase what the player is touching is better than checking that vertical speed is small, which is also true at the apex of every jump. It still failed in its own way: a cube stuck to the underside is something touching below you, so a gathered clump became its own launchpad and jump could be held down forever.

## Quantising both sides

Glenn Fiedler's [state synchronization](https://gafferongames.com/post/state_synchronization/) recommends quantising the simulation on both sides: if the server simulates at a precision it never transmits, the client sees a rounded copy of a state that has already moved on.

With a solver this has a cost the articles do not mention, which falls on sleeping. Snapping every body onto the wire's grid each tick took cube_yard's settled pile from 901 asleep to **0**: a resting body jitters by less than one quantisation step, so it is re-snapped forever and writing a body's position marks it modified, which is enough that it never reaches the sleep threshold. Guarding on `is_sleeping` does not help, because the body never gets into that state. Snapping only bodies that are **moving** fixes it: a body that is not moving is not drifting, so there is nothing to correct.

Measure whether it helps. In cube_yard it does not: 41894 bytes against 41806 over a settling yard, a difference of 0.2%. The technique helps when the *client* extrapolates by running the simulation forward between updates. cube_yard's client only draws. If your client simulates, quantise both sides. If it does not, the quantisation costs you and buys nothing.

## Drawing what a solver produces

A rigid body tumbles and a renderer that draws axis-aligned boxes cannot show that: macroquad's `draw_cube` takes a position and a size and no rotation. Rotating bodies need a mesh you rebuild each frame, which is also the fast path, 901 cubes for about 158us.

Watch the batcher underneath. macroquad clamps a draw call at 10000 vertices and 5000 indices and prints a warning instead of failing, drawing only the front of the buffer, so one mesh of 905 cubes rendered about a quarter of the scene and the rest was missing. Split the mesh into chunks to stay under the limit. Read all of a spike's output: this warning was printed on every frame of the rendering spike that was supposed to catch it, but the check only grepped for its own success message.

## The lab

[puck_rink](../../examples/puck_rink/) with `--features rapier`, which compiles both backends and lets the server pick at startup, so one scripted trace runs through the fixed-point step and the solver and prints them side by side. Then [cube_yard](../../examples/cube_yard/), which is the other family: 901 bodies nobody re-simulates, priced from 23.90 Mbit/sec down to 0.23 with an error column beside every row.
