# Plaza examples

One crate each, smallest first. Run any of them with `cargo run -p <crate>` (the crate name is in the last column). The two playgrounds are the exception: they target the browser and have their own scripts, see their READMEs.

| Example | Shows | Crate |
|---|---|---|
| [`shared-counter`](shared-counter/) | The smallest complete application: two clients share one value. | `plaza-example-shared-counter` |
| [`pong`](pong/) | Real WebSockets, 60Hz simulation. Two browser tabs to play. | `plaza_example_pong` |
| [`whack_a_mole`](whack_a_mole/) | A scheduler-driven game loop with scoring. | `plaza_example_whack_a_mole` |
| [`ability_cooldowns`](ability_cooldowns/) | Scheduled events that expire. | `plaza_example_ability_cooldowns` |
| [`timed_debuff`](timed_debuff/) | A callback scheduler undoing an effect on a timer. | `plaza_example_timed_debuff` |
| [`typing_indicator`](typing_indicator/) | Game-time timeouts that reset on activity. | `plaza_example_typing_indicator` |
| [`card_table`](card_table/) | Turns, rounds, and phases with hidden information: each player sees only their own cards. | `plaza_example_card_table` |
| [`csp_net_example`](csp_net_example/) | Client-side prediction and server reconciliation over a simulated network (headless). | `plaza_csp_net_example` |
| [`netcode_playground`](netcode_playground/) | The same made interactive in the browser, plus interpolation and lag compensation. See its [README](netcode_playground/README.md). | `netcode_playground` |
| [`rollback_playground`](rollback_playground/) | The other netcode family: peer-to-peer deterministic rollback, two peers predicting each other's inputs in the browser. See its [README](rollback_playground/README.md). | `rollback_playground` |
| [`horde_playground`](horde_playground/) | Scale: thousands of enemies, four players, per-player relevance and a low send rate. A real **listen-server**: host, join over a socket, or deploy headless. See its [README](horde_playground/README.md). | `horde_playground` |
| [`blackhole_playground`](blackhole_playground/) | Sending a *field* instead of its consequences: thousands of pellets moved by a handful of black holes. Also a **listen-server** with the same four roles. See its [README](blackhole_playground/README.md). | `blackhole_playground` |

For example:

```sh
cargo run -p plaza-example-shared-counter
```

The four playgrounds (`netcode`, `rollback`, `horde`, `blackhole`) pull in a large graphics dependency, so they are excluded from the default workspace build; a bare `cargo build`/`test` skips them. Run each via its own `run-native.sh` or `serve.sh`.

`horde` and `blackhole` are genuine multiplayer over a real socket (built on `plaza`, `plaza_session`, and `plaza_ws`), not scripted single-player. `./run-native.sh` hosts and plays by default; a `--role` argument switches between `headless` (deploy), `observer` (watch), `host`, and `client` (join). Their `static/*.wasm` are gitignored build artifacts, so run `serve.sh` to produce and host the browser client on a fresh checkout. The other two (`netcode`, `rollback`) remain single-process browser demos.
