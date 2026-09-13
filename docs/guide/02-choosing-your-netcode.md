# 02. Choosing your netcode

This chapter helps you pick the netcode model that fits your game, before any mechanics matter.

Multiplayer architectures differ mainly in what crosses the wire and who is allowed to be wrong. Plaza supports the main models instead of choosing one for you and the examples come in pairs so you can try the differences yourself.

## The families

**State-sync.** Inputs go up, a snapshot of the world comes down every tick, the latest frame wins and a client that missed ten frames is fully described by the eleventh. This is by far the simplest model: there is no join special case, no catch-up protocol and no client-side simulation to keep in agreement with the server. It costs bandwidth proportional to world size and the client shows the world where the server last said it was. Choose it when the world is small or when reactions are measured in hundreds of milliseconds rather than tens. Most turn-based games, most apps and more action games than people expect work fine with it. Lab: [tag_arena](../../examples/tag_arena/) and [pong](../../examples/pong/) as the smallest hosted version. When the world is *not* small, [cube_yard](../../examples/cube_yard/) is the same model taken to 901 solver-driven bodies and priced stage by stage, from 23.90 Mbit/sec down to 0.23. When the world is larger than any client can hold, [spacemo](../../examples/spacemo/) sends each client its own subset of the world from the start, because in a volume there is no broadcast to optimise. It predicts the local ship, interpolates only what it was told about and forgets whatever the server stops mentioning, since the server says nothing when someone leaves your view and simply stops sending them.

**Server-authoritative with prediction.** The server sends the world as in state-sync, but the client simulates its own entity forward immediately instead of waiting and reconciles when the server's answer arrives, while rendering everyone else slightly in the past. This is the Gambetta model, the default for action games and the subject of [chapters 20](20-hiding-the-wire.md) and [21](21-everyone-elses-ghosts.md). Choose it when your own character must feel instant and the world cannot be made deterministic. Lab: [netcode_playground](../../examples/netcode_playground/), where every mechanism has an off switch.

**Deterministic lockstep.** Every client runs the full simulation from a shared seed and only inputs cross the wire, never the world state. Bandwidth no longer depends on world size: [seed_defense](../../examples/seed_defense/) runs a tower defense with zero divergence from 0 to 400ms of added latency, while a single lost input costs a snapshot's worth of recovery. The cost is strict determinism: fixed-point or carefully ordered floats, defined iteration orders and a digest check as your only way to see a problem, because a diverged client looks completely healthy. Choose it for simulation-heavy worlds with modest player counts if you can keep to that discipline.

**Deterministic rollback.** This is lockstep without the waiting: peers exchange inputs, predict missing ones and roll the simulation back when a prediction was wrong. It is the fighting-game model and the one family with no server authority at all. [rollback_playground](../../examples/rollback_playground/) puts two full peers side by side with prediction and rollback each toggleable, so you can watch responsiveness, correctness and smoothness trade off against each other.

**Event-sourced.** The op stream is the record. When the ops themselves are what you keep, replaying them through shared rules gives you both the feature and the anti-cheat. [ghost_trials](../../examples/ghost_trials/) makes a racing ghost out of an input log and the server checks your lap time by replaying your inputs. The claimed time is accepted only if the replay produces it, with no heuristics. Latency cannot change a lap time because the network is not part of the simulation.

## Mixing families

The families are chosen per stream and one game can use several. [curtain_fire](../../examples/curtain_fire/) runs a bullet-hell where the enemy curtain is a closed-form function of the tick (derived on each client, nearly free) while player fire is streamed (costing bandwidth for as long as it exists) and its README prices the two against each other on one wire. [card_table](../../examples/card_table/) snapshots state but narrates turns as ops between snapshots. In practice you choose per kind of state, depending on which of it is derivable, streamable, secret or has to feel instant.

## Who runs the server

- **Dedicated server**: a process nobody plays in. It has the simplest authority model and most chapters assume it.
- **Listen server**: one player's machine hosts. Plaza's playgrounds use this shape (one binary, `--role host/client/observer/headless`). The loopback socket that connects the host's own player serializes and copies bytes exactly as a real socket does, so the host's player goes through the same code path as every other client. The host does have a real latency advantage, which is why impairment tooling exists ([chapter 31](31-faking-a-bad-network.md)).
- **Peer-to-peer**: only the rollback family goes here and it inherits that family's constraints.

## A decision sketch

Ask these questions in order. Can the whole interesting world fit in a snapshot at your tick rate? Use state-sync and stop here. Must your own inputs feel instant? Add prediction. Is the world huge but perfectly simulable and can you enforce determinism? Lockstep (or rollback if there is no server to wait for). Is the op log itself the product? Event-sourced. Is some state derivable from the tick? Derive it, whatever else you chose.

The model you choose also decides what kind of bug you will have. State-sync bugs show up as stale frames, prediction bugs as corrections and lockstep bugs as silent divergence.
