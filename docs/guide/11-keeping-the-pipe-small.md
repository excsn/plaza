# 11. Keeping the pipe small

The question this chapter answers: how do I afford fifty players, or three thousand entities, on a wire that charges by the byte?

The previous chapter made replication correct. This one makes it cheap, and it is deliberately a separate chapter because the tools here are optimizations with sharp edges: every one of them buys bandwidth by making some failure mode subtler, and each block's docs price that trade with measurements instead of adjectives.

## First, measure, because the expensive thing is not what it looks like

The founding incident of this chapter comes from [horde_playground](../../examples/horde_playground/): with 128 players and 3000 enemies, the enemies everyone worries about were 9% of traffic, and player-to-player broadcast, which is quadratic in players, was 81%. The byte cost lives in the thing sent most often, not the thing that looks expensive. Three rounds of optimising the wrong thing produced `RateMeter::share_of`, and the rule the crate docs state: a claim about bandwidth should be a number on screen, not an assertion in a README. Put the meter up before you optimize anything.

## Relevance: send only what is near

The [`relevance`](../../server_utils/API_REFERENCE.md) module is interest management as blocks: Morton keys and a `GridQuantizer` map world positions to cells, a `SpatialGrid` answers range queries without scanning the world, a `VisibilitySet` holds each client's visible set as a bitset and diffs it word-at-a-time into exactly the entered/left stream your spawn and despawn messages need. Cell size, the relevance rule, and the wire encoding stay yours: blocks, not a policy.

The subtle block is `TierBoundary`: any threshold that affects the wire will flap when an entity loiters at the edge, so the boundary is two radii, and it takes less distance to stay in than to get in. Hysteresis is not an optimization here; it is what stops a loiterer from costing a spawn-despawn pair per tick.

## Deltas: send only what changed, against what actually arrived

Diffing against what you last *sent* silently assumes every packet arrives; one drop and the client is permanently missing something that is never mentioned again. [`DeltaBaseline`](../../server_utils/API_REFERENCE.md) diffs against what the client *acknowledged*, with the ack being the newest contiguous state, not the newest bit seen, because taking the newest bit hands the diff a state that never existed, and the measured result of that mistake was recovery statistically indistinguishable from no recovery.

The client half is `DeltaMirror` in the client crate (shared types, one implementation, because two implementations that agree today are a disagreement waiting to happen), and the horde numbers make the case: at 25% simulated loss, the naive diff left 185 unremovable corpses on screen; the acked baseline left single digits, for about 3x the bandwidth of naive. The digest that proves both ends still agree doubles as the entire resync protocol: dropping the mirror *is* the resync request. No resync message exists.

## Aggregation: keep the distant contribution, drop only its resolution

Relevance is binary, in or out, and the [`aggregate`](../../server_utils/API_REFERENCE.md) docs name exactly when binary is wrong: it is right for entities a client merely *draws*, and wrong for entities it has to *compute* with, because dropping an input silently changes the answer. [blackhole_playground](../../examples/blackhole_playground/) is the hostile case: clients integrate 2000 pellets through a gravity field, and culling distant attractors cut bandwidth while multiplying simulation error 2.4x, because a hole you were not told about still bends every pellet you hold.

`AggregateTree` is Barnes-Hut for the wire: distant groups collapse to one stand-in at their weighted centroid, tunable by a single `theta`, and `theta = 0` returns every point exactly, so aggregation off is the same code path. How coarse you may go is a property of the consumer, not of the approximation: the gravity field tops out near theta 1.0, a crowd you only draw is fine at 1.5.

## Derive instead of sending

The cheapest bytes are the ones that never exist. [curtain_fire](../../examples/curtain_fire/) runs its enemy bullet curtain as a closed-form function of the tick, derived on every client for free, while player fire is streamed and paid for forever, and its README prices the two against each other on one wire. Before you compress a stream, ask whether it needs to be a stream at all.

## Ripping it apart

Relevance and aggregation both answer *what to send*. Neither answers *how much*, and that is a separate block: [`priority`](../../server_utils/API_REFERENCE.md) keeps a score per entity that **survives the ticks it is not sent on**, so the ones that did not fit are the ones that go next, nothing starves, and a budget becomes a ceiling instead of an outcome. [`rest`](../../server_utils/API_REFERENCE.md) is its cheapest input: in a settled scene most things are not moving, and saying so costs one bit against the thirty-three a velocity costs. A solver already knows which, and `!body.is_sleeping()` is the obvious signal, but check its granularity before you take it: rapier sleeps an **island**, meaning every body in a chain of contacts, so one cube still jostling in a heap reports the whole heap awake. Feeding `rest` a per-body "has this moved recently" instead took cube_yard from 205 cubes claiming to be awake to 56, against 57 that had actually moved.

[cube_yard](../../examples/cube_yard/) is that measured end to end, and its result is the argument for reading this chapter in order: 901 cubes went from 23.90 Mbit/sec to 4.20 by quantising and bit packing, which sounds like the answer and is not, because 4.20 has a floor no encoding reaches past. The priority budget took it to **0.23**, and the honest reading is that the last 16x was choosing rather than compressing. Two cautions the example paid for: budget the *wire*, envelope included, or you overshoot by the frame around the payload, and prefer `order`/`sent` over `fill` when a cube's real cost spans an order of magnitude, since packing until full beats any estimate generous enough to be safe.

Everything here composes or omits independently: a grid without deltas, deltas without aggregation, your own interest rule over the same bitset diff. The blocks share only small vocabulary types (slot keys, digests) that live in the client crate so a browser can hold up its half. And every block keeps its honesty instrumentation public, phantom counts beside missing counts, because a metric for wrong-things-present needs its right-things-absent twin: a starved mirror agrees with everything.

## The lab

[horde_playground](../../examples/horde_playground/) with the HUD meters up: watch share_of tell you where the bytes go, then add loss with the sliders and watch the acked baseline hold where naive recovery would rot. Then [blackhole_playground](../../examples/blackhole_playground/) to feel aggregation as a correctness knob, including its best negative result: correcting the worst-off pellets first loses to a plain round-robin sweep by 2.4x to 3.5x, because bounded sweep is what actually bounds error.
