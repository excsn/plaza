# Plaza: Server-Authoritative Shared State for Real-Time Games & Apps

[![crates.io](https://img.shields.io/crates/v/plaza.svg)](https://crates.io/crates/plaza) [![License: MPL-2.0](https://img.shields.io/badge/License-MPL%202.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)

**Plaza** is a foundation for applications where many people change one piece of state at once: multiplayer games, collaborative editors, shared whiteboards.

One controller owns the state and is the only thing that mutates it, applying one operation at a time, so your rules need no locks and there is never a question of which version is real.

Every change is an operation: a value, not a write. That makes it something you can validate, reject, broadcast, log, replay, or predict on the client before the server confirms it.

Each client's snapshot is built *for that client*, so a card game showing every player their own hand and only the count of everyone else's is the ordinary case rather than something bolted on.

***Building blocks, not a framework. Take what fits.***

## Project Status: Experimental ⚠️

The API changes, and breaking changes land without ceremony while the shape settles. It has no production users yet.

Where a decision belongs to your application (how long a disconnected player keeps their seat, how state is versioned, what a player may see), Plaza provides the bookkeeping and leaves the decision to you. Anything it does provide can be swapped for your own.

## Structure

*   `core/`: The main `plaza` library, the controller loop and the traits you implement. See [`core/README.md`](core/README.md) for installation, usage, and a complete program.
*   `session/`: Real transports: actix-web WebSockets and length-delimited TCP, with a pluggable wire format, plus the optional listen-server HTTP layer that serves a browser client from the same origin as the socket. See [`session/README.md`](session/README.md).
*   `lobby/`: Rooms on a single server: create, list, join, reap. See [`lobby/README.md`](lobby/README.md).
*   `client_utils/`: The client side: prediction (for either server input model), reconciliation, interpolation, correction smoothing, fixed timesteps, and the mirror that holds a streamed entity set. No async runtime and no server crates, so it suits wasm and engine plugins. See [`client_utils/README.md`](client_utils/README.md).
*   `server_utils/`: The pure server-side counterpart: historical state rewind for lag compensation, relevance streaming, crowd aggregation, delta baselines, and seat allocation. Also runtime-free and wasm-safe, and shares `client_utils`'s interpolation, digest and slot-key types so the two sides cannot disagree about them. See [`server_utils/README.md`](server_utils/README.md).
*   `wire/`: The `WireCodec` trait and message envelope shared by a server and its clients, kept runtime-free, plus a build-time protocol version so the two ends can tell they were built from the same definition. See [`wire/README.md`](wire/README.md).
*   `ws_client/`: `plaza_ws`, the client-side socket: one interface over desktop, browser and in-process. The counterpart to `session/`, which is server-only by construction. See [`ws_client/README.md`](ws_client/README.md).

Each crate carries an `API_REFERENCE.md` documenting its full public surface. [`INDEX.md`](INDEX.md) maps where everything lives.

## Getting Started

Please refer to **[`core/README.md`](core/README.md)** for installation, the four type parameters everything is generic over, and a complete runnable program.

## Examples

| Example | Shows |
|---|---|
| [`shared-counter`](examples/shared-counter/) | The smallest complete application. |
| [`pong`](examples/pong/) | Real WebSockets, 60Hz simulation. Two browser tabs to play. |
| [`whack_a_mole`](examples/whack_a_mole/) | A scheduler-driven game loop with scoring. |
| [`ability_cooldowns`](examples/ability_cooldowns/) | Scheduled events that expire. |
| [`timed_debuff`](examples/timed_debuff/) | A callback scheduler undoing an effect on a timer. |
| [`typing_indicator`](examples/typing_indicator/) | Game-time timeouts that reset on activity. |
| [`card_table`](examples/card_table/) | Turns, rounds, and phases, with hidden information: each player sees only their own cards. |
| [`csp_net_example`](examples/csp_net_example/) | Client-side prediction and server reconciliation over a simulated network. |
| [`netcode_playground`](examples/netcode_playground/) | The same, made interactive in the browser: drag the box, crank the latency, toggle each mechanism off to see it break. Also interpolation and lag compensation. See its [README](examples/netcode_playground/README.md). |
| [`rollback_playground`](examples/rollback_playground/) | The other netcode family: peer-to-peer deterministic rollback, two peers predicting each other's inputs. See its [README](examples/rollback_playground/README.md). |
| [`horde_playground`](examples/horde_playground/) | Scale, as a real listen-server: thousands of enemies, per-player relevance, host or join over a socket or deploy headless. See its [README](examples/horde_playground/README.md). |
| [`blackhole_playground`](examples/blackhole_playground/) | Sending a *field* instead of its consequences, also a listen-server with four roles (headless / observer / host / client). See its [README](examples/blackhole_playground/README.md). |
| [`bomb_grid`](examples/bomb_grid/) | Netcode on a **lattice**, where a correction cannot be eased away and has to be counted instead. Bombs, chain reactions, destructible walls. See its [README](examples/bomb_grid/README.md). |
| [`pellet_maze`](examples/pellet_maze/) | An input that names a **place** rather than a time, which no schedule can settle. Also per-recipient frames, used to make a player genuinely invisible. See its [README](examples/pellet_maze/README.md). |
| [`seed_defense`](examples/seed_defense/) | A wire that carries **causes instead of consequences**: a seed and a wave number produce the whole world on every machine, and a digest proves they still agree. See its [README](examples/seed_defense/README.md). |
| [`ghost_trials`](examples/ghost_trials/) | The op stream as an **event-sourced record**: a ghost is a replay of an input log, and a lap time is decided by replaying the evidence rather than by believing a number. See its [README](examples/ghost_trials/README.md). |

```sh
cargo run -p plaza-example-shared-counter
```

Turning the browser playgrounds into real listen-servers surfaced a run of bugs whose causes were consistently not where the symptoms pointed, and most of what is in `client_utils` and `server_utils` today is what those argued for. [`examples/LEARNINGS.md`](examples/LEARNINGS.md) is the record: the principles that prevent whole classes of bug, what broke and which reasonable theories were wrong, and what all of it changed in plaza itself.

## License

`plaza` is licensed under the Mozilla Public License Version 2.0 (MPL-2.0). You are free to use, modify, and distribute it under the terms of the MPL-2.0, which requires that modifications to MPL-licensed files be made available under the same license.
