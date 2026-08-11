# 02. Choosing your netcode

The question this chapter answers: which model fits my game, before any mechanics matter?

Every multiplayer architecture is an answer to one question: **what crosses the wire, and who is allowed to be wrong?** Plaza supports the main answers rather than choosing one for you, and the examples pair off so you can feel the differences instead of taking them on faith.

## The families

**State-sync: the server sends the world.** Inputs go up, a snapshot of the world comes down every tick, the latest frame wins, and a client that missed ten frames is fully described by the eleventh. This is the simplest model by a wide margin: no join special case, no catch-up protocol, no client-side simulation to keep honest. Its cost is bandwidth proportional to world size, and its feel is "the world is where the server last said it was". Choose it when the world is small, or when reactions are measured in hundreds of milliseconds rather than tens. Most turn-based games, most apps, and more action games than people expect live happily here. Lab: [tag_arena](../../examples/tag_arena/), and [pong](../../examples/pong/) as the smallest hosted version. When the world is *not* small, [cube_yard](../../examples/cube_yard/) is the same model taken to 901 solver-driven bodies and priced stage by stage, from 23.90 Mbit/sec down to 0.23. And when the world is larger than any client can hold, [spacemo](../../examples/spacemo/) is the version where the frame is a per-client subset from the first stage rather than as an optimisation, because in a volume no broadcast exists to optimise: it predicts the local ship, interpolates nothing it was not told about, and forgets what stops being mentioned, since a server that can no longer see you for someone simply goes quiet.

**Server-authoritative with prediction: the server sends the world, the client refuses to wait for it.** Same authority story as state-sync, but the client simulates its own entity forward immediately and reconciles when the server's answer arrives, while rendering everyone else slightly in the past. This is the Gambetta model, the default for action games, and the whole subject of [chapters 20](20-hiding-the-wire.md) and [21](21-everyone-elses-ghosts.md). Choose it when your own character must feel instant and the world cannot be made deterministic. Lab: [netcode_playground](../../examples/netcode_playground/), where every mechanism has an off switch.

**Deterministic lockstep: the wire carries causes, never the world.** Every client runs the full simulation from a shared seed, and only inputs cross the wire. Bandwidth stops depending on world size entirely: [seed_defense](../../examples/seed_defense/) runs a tower defense where added latency costs *nothing*, zero divergence from 0 to 400ms, while a single lost cause costs a snapshot's worth of recovery. The price is determinism discipline: fixed-point or carefully ordered floats, defined iteration orders, and a digest check as your only instrument, because a diverged client looks completely healthy. Choose it for simulation-heavy worlds with modest player counts, and only if you can stomach the discipline.

**Deterministic rollback: lockstep that refuses to wait.** Peers exchange inputs, predict missing ones, and roll the simulation back when a prediction was wrong. This is the fighting-game model, and it is the one family with no server authority at all. [rollback_playground](../../examples/rollback_playground/) puts two full peers side by side with prediction and rollback each toggleable, so you can watch the responsiveness, correctness, and smoothness triangle trade against itself.

**Event-sourced: the op stream is the record.** When the ops themselves are the artifact, replaying them through shared rules is both the feature and the anti-cheat. [ghost_trials](../../examples/ghost_trials/) makes a racing ghost out of an input log, and the server verdicts your lap time by replaying your inputs: the claimed time either falls out of the evidence or it does not, no heuristics, and latency cannot change a lap time because the link is not in the loop.

## You are allowed to mix

The families are per-stream choices, not identities. [curtain_fire](../../examples/curtain_fire/) runs a bullet-hell where the enemy curtain is a closed-form function of the tick (derived on each client, nearly free) while player fire is streamed (paid for forever), and its README prices the two against each other on one wire. [card_table](../../examples/card_table/) snapshots state but narrates turns as ops between snapshots. Choosing your netcode is really choosing per kind of state: what is derivable, what is streamable, what must be secret, what must be instant.

## The other axis: who runs the server

- **Dedicated server**: the process nobody plays in. Simplest authority story, and what most chapters assume.
- **Listen server**: one player's machine hosts. Plaza's playgrounds lean on this shape (one binary, `--role host/client/observer/headless`), and the loopback socket that connects the host's own player is deliberately not a shortcut: bytes serialize and copy exactly as over a real socket, so the host is never the only client that cannot be wrong. The host's *latency advantage* is real, though, which is why impairment tooling exists ([chapter 31](31-faking-a-bad-network.md)).
- **Peer-to-peer**: only the rollback family goes here, and it inherits that family's constraints.

## A decision sketch

Not a flowchart, but the questions in the order they eliminate options: Can the whole interesting world fit in a snapshot at your tick rate? State-sync, stop here, enjoy your life. Must your own inputs feel instant? Add prediction. Is the world huge but perfectly simulable, and can you enforce determinism? Lockstep, or rollback if there is no server to wait for. Is the op log itself the product? Event-sourced. Is some state derivable from the tick? Derive it, whatever else you chose.

And one warning from the examples that generalizes: the model you choose decides what *kind* of bug you will have. State-sync bugs are stale frames. Prediction bugs are corrections. Lockstep bugs are silent divergence. Pick the failure mode you would rather debug at 2am.
