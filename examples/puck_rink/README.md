# puck_rink

Two-on-two air hockey, built for the one thing no server-authoritative example had: a **body two teams are pushing at once**. Every other example predicts what one player owns; a puck's next position depends on inputs you do not have, which is the case rollback exists for.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh -- --role client --connect ws://host:8097/ws
./wasm-serve.sh                          # build the browser client, host it on :8097
cargo run -p puck_rink --bin scripted    # the headless re-simulation audit
```

Every seat always has an actor: bots skate until humans take their paddles, one leaver at a time. WASD or arrows; push the puck through the far mouth.

## The topology: rollback under a server

`rollback_playground` is peer-to-peer: two peers exchange inputs and stay identical. Here the topology is the one a deployed game actually has, and the server holds two jobs at once:

- **Input orderer.** Every `Frame` echoes the inputs it applied (`world(f) = step(world(f-1), applied(f))`). That echo is what a client's [`RollbackSession`](../../client_utils/API_REFERENCE.md) confirms against: one ordered truth instead of n² peer exchanges.
- **Authority.** The world in the frame is not advisory; a client that diverged would be corrected by it. The digest is how we know that correction never has to happen.

The client runs the same fixed-point `sim::step` the server runs, predicts every unconfirmed input (repeat-last, which a held direction makes mostly right), and rolls back when the echo disproves a guess. `plaza_client_utils::rollback` is consumed as shipped: `StateHistory`, `InputTimeline`, and the session loop, first consumer outside its own playground.

## The number: one puck, two treatments

The panel draws the puck one of two ways and measures both the same way, recording what was shown each frame and comparing when the authoritative world for that frame arrives:

- **Interpolate**: delayed server frames blended, the standard treatment for anything owned by someone else. Smooth, and late by the render delay plus the one-way, which on a contested puck is exactly the window where your paddle visibly passes through it.
- **Rollback**: the re-simulated present. On the beam, corrections arrive as re-simulations instead of position lerps, and the panel prices them: corrections count, mean snap size, and re-simulated frames, the cost the IDEAS entry asked to see beside the error.

The toggle covers the puck alone. Remote paddles take the delayed blend in both modes and only your own paddle is drawn from the predicted present: a remote input at the present is a guess, and a drawn guess jumps on every disproof, which for a bot flipping its held direction is constantly.

## The digest, or why fixed point is not a style choice

The simulation is `Fx` throughout (`plaza_client_utils::fixed`); the renderer is the only float consumer. Every frame carries an FNV digest of the world, and the client checks its own re-simulation of every confirmed frame against it: the count sits on the panel and **must stay zero on the diverged side**. This is seed_defense's discipline reaching rollback: a contended body diverges quietly, two clients disagreeing about one collision by one representational unit both stay plausible for a whole rally, and only a digest says so. The scripted run is the same audit headless: re-simulate every broadcast frame from the echoed inputs and assert zero divergence.

Rapier was considered for the puck and declined for exactly this file: five circles and four walls need no solver, and the determinism claim would move from ~300 lines of owned `Fx` into a third-party crate where it holds same-version-only.

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client, and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`); MessagePack with a build-derived protocol version; the session's pong clock is the simulation clock, so input frames are aimed at sim time. Inputs are tick-addressed through `plaza_server_utils::InputSchedule` (level semantics: a held direction repeats until replaced), which is the same input model the client's prediction assumes.
