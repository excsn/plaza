# 21. Everyone else's ghosts

This chapter covers why other players do not teleport even though you cannot predict them and how a shot lands where you actually aimed.

## The rendering hierarchy

For an entity you do not control, the client crate's docs give a strict order of preference. Move down the list only when the option above has no data:

1. **Run the rule.** If you know the entity's governing rule and its inputs, simulate it; this is running the shared rule rather than predicting. `HeldInputPredictor` can simulate a remote whose *intent* you know, which its docs call the least obvious thing in the crate.
2. **Interpolate.** The normal case: render the entity a beat in the past, between two states you actually received.
3. **Extrapolate.** Updates stopped arriving; coast briefly.
4. **Hold.** Freeze the entity rather than draw a guess.

`RemoteView` bundles this policy: push snapshots in, ask for a render state at a timestamp and it works down the list for you.

## Interpolation

Rendering remotes slightly in the past trades a small, constant delay for smooth motion built from real states. The blocks are `SnapshotBuffer` plus `InterpolationClock` and the clock follows principle 4 from [the previous chapter](20-hiding-the-wire.md): the render timeline is *declared* as an offset from synced time and never steered by when packets happen to arrive, because an arrival-steered clock makes ping an input to the game. `ArrivalMonitor` measures how much delay you actually need instead of relying on a rule-of-thumb constant.

A straight line between two samples works while they are close together and fails when they are not. At sixty a second the error across a 16ms chord is invisible; at ten, the chord flattens 100ms of a curved path and the entity visibly turns corners, sliding to each sample and changing direction. `HermiteView` interpolates with the velocity at *both* ends, so the joins between segments are smooth. It is a separate type from `RemoteView` because `RemoteView` keeps a single velocity for coasting past the newest sample and a spline needs one per sample. The two measurements point in opposite directions. On a smooth curve at 10Hz it is 484x more accurate, because a cubic is near-exact given true derivatives. Across 500 cubes colliding in [cube_yard](../../examples/cube_yard/) it is **39x worse than the straight line it replaces**, because on 5% of frames it leaves the segment its own two samples bracket and a chord cannot. The velocity at a sample assumes a smooth path to the next one and a contact breaks that assumption after the packet has gone.

Use the spline for paths that curve smoothly between samples and the straight line anywhere bodies collide. cube_yard is the reason `HermiteView` was built, but cube_yard itself must not use it.

## Extrapolation as a fallback

The crate treats extrapolation as the fallback for when updates stop. Dead reckoning a *player* fails because the velocity records how the player was moving and the player can change it at any time. `ExtrapolationBase` therefore caps how far it will coast and then holds. The cap's current shape comes from a bug: the old code discarded the result past the cap and jumped back the entire window in the wrong direction. Two tests had asserted that behaviour, so they pinned the bug rather than the requirement.

The playground also has a negative result: a curve-fitting extrapolator that measured better on paper changed nothing at normal send rates, because there was no gap to extrapolate across. `TrajectoryPredictor` only helps below roughly 10Hz.

## Deriving presentation on the client

A body that slides rather than walks is the most common reason correct netcode still looks wrong. The obvious fix is to put animation state on the wire: a pose, a phase or an event saying "this one is running now".

That is almost never necessary, because the client already has the data an animation depends on. [gow_3d](../../examples/gow_3d/) animates a walk cycle from **the speed its interpolation already computes** from the two samples it keeps, a cast pose from the bar already on the frame, a recoil from the landing event already delivered and a fall from health reaching zero. None of it crosses the wire or needed a new field.

Two details make derived animation work. Phase the cycle on **distance covered rather than on time**. Otherwise a body that is slowing down moonwalks through its own stride. Derive from the sample stream rather than from the render clock, so the animation stays correct at any send rate, including the low rates this chapter covers.

Death did need a server change, in relevance rather than a new field: a downed character left the spatial index the instant it died, so it left every audience and no client was ever told it had fallen. Bodies now stay indexed briefly while they fall. **An entity has to stay relevant for as long as its animation takes**, which makes this an interest-management rule rather than a rendering one.

## Lag compensation

Everything so far only changed what clients draw. Lag compensation involves the server's authority, because you aimed at where the target *was* (you render the past) and by the time your shot reaches the server, the target has moved. The server rewinds: [`HistoricalStateBuffer`](../../server_utils/API_REFERENCE.md) keeps a rolling history of the authoritative world and the hit is judged against the world at the instant the shooter saw, interpolated between bracketing states through the same `Interpolatable` impl the client uses.

[hit_scan](../../examples/hit_scan/) is the lab. Its panel counts hits granted by rewind *and* deaths suffered behind cover side by side, because turning the rewind off moves the unfairness onto the shooter rather than removing it. Lag compensation decides who absorbs the latency. This mechanism and both of its paradoxes were described by Yahn Bernier in [Latency Compensating Methods in Client/Server In-game Protocol Design and Optimization](https://developer.valvesoftware.com/wiki/Latency_Compensating_Methods_in_Client/Server_In-game_Protocol_Design_and_Optimization) (2001), the primary source for most of this chapter and the previous one.

**Client-reported hits.** You could skip all of this and let the client say what it hit. The client knows exactly what it was aiming at and a "hit" message would be simpler than a rewind buffer and free of precision error. Plaza does not offer it and Valve rejected it for a reason beyond cheating clients: even a clean client with anticheat intact can have hit messages injected by a proxy on a third machine anywhere along the route. Client-authoritative outcomes therefore fail even for honest players. Every contested decision in this guide is resolved from what the client *named* rather than from what it *claimed happened* and [chapter 40](40-the-right-to-say-no.md) puts every bound on a server-side measurement for the same reason.

Fairness also depends on *when* an input counts. [`InputSchedule`](../../server_utils/API_REFERENCE.md) executes inputs on the tick the client named, so two players who pressed together execute together whatever their ping. Inputs outside the window are rejected rather than corrected, because a cheater could hide backdated inputs in that slack. The tick is derived from time rather than counted, after an incident where a rebuilt world reset the counter and silently refused every input forever.

[auction_floor](../../examples/auction_floor/) does the same in an app: contested claims are decided from what each client *named* rather than when packets arrived, with a floor built from what the server measured, so ping does not decide who wins an auction either.

## Replacing it

Each level of the hierarchy is its own block and `RemoteView` is just the policy that combines them; you can write your own policy over the same buffers. The server-side rewind is independent of everything client-side except the shared `Interpolatable` trait, which is one impl on your state type.

## The lab

[netcode_playground](../../examples/netcode_playground/) again, this time the interpolation and lag-compensation switches: watch remotes stutter when interpolation is off and your shots start missing when rewind is off. Then [hit_scan](../../examples/hit_scan/) for both counts, shots honored versus deaths behind cover, at your chosen latency.
