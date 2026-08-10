# 21. Everyone else's ghosts

The question this chapter answers: you cannot predict other players, so why don't they teleport, and how does a shot land where you actually aimed?

## The rendering hierarchy

For an entity you do not control, the client crate's docs give a strict order of preference, and the discipline is to reach down the list only when the rung above has no data:

1. **Run the rule.** If you know the entity's governing rule and its inputs, simulate it; that is not prediction, it is the shared rule doing its job. `HeldInputPredictor` can simulate a remote whose *intent* you know, which its docs call the least obvious thing in the crate.
2. **Interpolate.** The normal case: render the entity a beat in the past, between two states you actually received.
3. **Extrapolate.** Updates stopped arriving; coast briefly.
4. **Hold.** Better a statue than a hallucination.

`RemoteView` bundles this policy: push snapshots in, ask for a render state at a timestamp, and it walks the ladder for you.

## Interpolation: honesty about the past

Rendering remotes slightly in the past is the standard trade: a small, constant staleness in exchange for smooth motion assembled from real states. The blocks are `SnapshotBuffer` plus `InterpolationClock`, and the clock embodies principle 4 from [the previous chapter](20-hiding-the-wire.md): the render timeline is *declared*, offset from synced time, never steered by when packets happen to arrive, because an arrival-steered clock makes ping an input to the game. `ArrivalMonitor` measures what your actual delay needs to be, instead of a folklore constant.

A straight line between two samples is right while they are close together and wrong when they are not. At sixty a second the error across a 16ms chord is invisible; at ten, the chord flattens 100ms of a curved path and the entity visibly corners, sliding to each sample and changing direction. `HermiteView` is the same rung done with the velocity at *both* ends, so the seams stop being corners, and it is a separate type from `RemoteView` for a concrete reason: that one keeps a single velocity, for coasting past the newest sample, and a spline needs one per sample. Two numbers set the expectation. On a smooth curve at 10Hz it is worth 484x, because a cubic is near-exact given true derivatives. On a cube falling and bouncing off a floor in [cube_yard](../../examples/cube_yard/), it is worth **2.1x**, because a body that changes direction between samples changed it in a way no spline was told about. Take the 2x and do not budget for the 484x on anything with contacts in it.

## Extrapolation: the fallback, not the technique

The crate is blunt: extrapolation is the starvation fallback, and dead reckoning a *player* fails for a stated reason worth memorizing: the velocity is not a constraint on the future, it is a record of the past. `ExtrapolationBase` therefore caps how far it will coast and then holds, and the cap's shape encodes a real bug: the old code discarded the result past the cap and jumped back the entire window in the wrong direction, and two tests had asserted that behaviour, pinning the bug rather than the requirement.

The playground's negative result completes the picture: a curve-fitting extrapolator measurably better on paper changed nothing at normal send rates, because there was no gap to extrapolate. A better extrapolator needs something to extrapolate; `TrajectoryPredictor` earns its keep only below roughly 10Hz.

## Lag compensation: the first decision with a loser

Everything so far kept clients pretty. Lag compensation is where authority gets involved, because you aimed at where the target *was* (you render the past, remember), and by the time your shot reaches the server, the target has moved. The server's answer is to rewind: [`HistoricalStateBuffer`](../../server_utils/API_REFERENCE.md) keeps a rolling history of the authoritative world, and the hit is judged against the world at the instant the shooter saw, interpolated between bracketing states through the same `Interpolatable` impl the client uses.

[hit_scan](../../examples/hit_scan/) is the lab, and its panel is the argument: it counts hits granted by rewind *and* deaths suffered behind cover, side by side, because turning the rewind off does not make the game fair, it moves the unfairness onto the shooter. Lag compensation is a choice of who eats the latency, and an honest game measures both sides of the meal. This mechanism and both of its paradoxes were described by Yahn Bernier in [Latency Compensating Methods in Client/Server In-game Protocol Design and Optimization](https://developer.valvesoftware.com/wiki/Latency_Compensating_Methods_in_Client/Server_In-game_Protocol_Design_and_Optimization) (2001), which is the primary source for most of this chapter and the previous one.

**Why not skip all of it and let the client say what it hit?** The client has pixel-accurate knowledge of what it was aiming at, and a "hit" message would be simpler than a rewind buffer and free of precision error. The reason plaza does not offer it, and the reason Valve rejected it, is stronger than "the client might be cheating": a completely clean client, with anticheat intact, can have hit messages injected by a proxy on a third machine anywhere along the route. Client-authoritative outcomes therefore fail even for honest players, which is why every contested decision in this guide is resolved from what the client *named* rather than from what it *claimed happened*. The same reasoning is why [chapter 40](40-the-right-to-say-no.md) puts every bound on a server-side measurement.

Fairness has a second lever: *when* an input counts. [`InputSchedule`](../../server_utils/API_REFERENCE.md) executes inputs on the tick the client named, so two players who pressed together execute together, whatever their ping. The window rejects rather than corrects, because backdating is exactly the slack a cheater hides in, and the tick is derived from time, never counted, after an incident where a rebuilt world reset the counter and silently refused every input forever.

[auction_floor](../../examples/auction_floor/) shows the same shape wearing an app's clothes: contested claims decided from what each client *named*, not when packets arrived, with a floor built from what the server measured. Ping never decides the winner of an auction either.

## Ripping it apart

Every rung of the hierarchy is its own block, and `RemoteView` is just the policy that stacks them; keep your own policy and the buffers underneath still serve. The server-side rewind is independent of everything client-side except the shared `Interpolatable` trait, which is one impl on your state type.

## The lab

[netcode_playground](../../examples/netcode_playground/) again, this time the interpolation and lag-compensation switches: watch remotes stutter when interpolation goes, and watch your shots start missing the moment rewind goes. Then [hit_scan](../../examples/hit_scan/) for the two-sided ledger, shots honored versus deaths behind cover, at your chosen latency.
