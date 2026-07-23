# rollback_playground

An interactive, in-browser demonstration of **rollback netcode**, built on `plaza_client_utils`'s [`rollback`](../../client_utils/src/rollback.rs) module.

Where the [`netcode_playground`](../netcode_playground/) shows the *server-authoritative* model (an authority decides, the client predicts its own entity and is reconciled), this shows the other family, *peer-to-peer deterministic lockstep*: two peers run the **same** simulation, exchange **only inputs**, predict the inputs they do not have yet, and roll back to fix a guess when the real input turns out different. It is the model fighting games use.

The point is to *feel* it: raise the latency and watch the same three-way trade every networked game makes, responsiveness, correctness, and smoothness, you can only ever have two of on demand.

## What you are looking at

Two arenas, side by side, one per peer. The left is **your peer**; the right is the **opponent's peer**, the exact same simulation running as the other player sees it. You drive the blue box with WASD or the arrow keys; the orange box is a scripted opponent that changes direction now and then (those turns are what break prediction).

| Colour | Meaning |
|---|---|
| solid blue | you |
| solid orange | the opponent |
| bright white ring | the box this peer controls directly (never predicted, never rolls back) |
| faint white ring | a predicted box's **last confirmed** position (the "ghost") |

The same box is drawn in both panels. In your peer, your blue box wears the bright ring (you own it, it is exact) and the orange box is **predicted**, it is the one that jumps when a rollback corrects it. In the opponent's peer it is the mirror: orange is exact, your blue box is the predicted one. Whichever box a peer predicts trails a faint ghost at its last confirmed position; the gap between the ghost and the solid box is exactly how far ahead the peer is guessing, and how far a rollback would re-simulate.

Above the panels, the headline: **IN SYNC** or **DESYNCED**. Both peers are compared at the newest frame they have every input for. That two independent simulations, each guessing about the other, still agree there, is the whole promise of rollback.

## Running it

Native window, no ceremony:

```sh
./run-native.sh          # or: cargo run -p rollback_playground --release
```

Same code as the browser build, opened as a desktop window. Nothing to install.

In the browser (wasm):

```sh
./serve.sh               # then open http://localhost:8080
./serve.sh 9000          # or pick a port
```

`serve.sh` does the whole ceremony: installs the `wasm32-unknown-unknown` target if it is missing, builds the release wasm, copies it next to the page, shrinks it with `wasm-opt` if you have binaryen, and serves the directory (via `basic-http-server` if installed, else `python3 -m http.server`).

## The controls, and what each reveals

The three netcode toggles are the demonstration; each picks a different corner of the trade-off.

| Control | What it does |
|---|---|
| latency / jitter / loss sliders | crank latency first; at 0 ms nothing below is visible, because every input arrives the frame it is needed |
| **prediction** | on: guess the missing remote input (repeat its last one) and simulate ahead. Off: **delay-based** lockstep, wait for the real input before advancing |
| **rollback** | on: when a confirmed input disproves a guess, restore the frame it went wrong and re-simulate to now. Off: trust the guess forever. Greyed out under delay-based play, which predicts nothing |
| **input redundancy: none / blind / targeted** | repeat recent inputs in every packet, so a dropped packet is recovered from a later one instead of forcing a long misprediction |
| show last-confirmed ghost | hides the faint confirmed-position rings |

Run the three combinations at, say, 150 ms latency and watch the headline and the boxes:

- **prediction on, rollback on** (the default): both boxes move the instant you press a key, and the banner stays **IN SYNC**. Rollback is silently correcting mispredictions every time the opponent turns, watch the "last rollback" readout tick. This is what shipping rollback feels like: responsive *and* correct.
- **prediction on, rollback off**: still responsive, but the banner goes **DESYNCED**. Each peer is now living in its own wrong guesses, never corrected, so the two simulations drift apart. This is the mode that does not exist in real games; it is here to show what rollback is buying.
- **prediction off** (delay-based): the banner stays **IN SYNC**, but the whole simulation **hitches**, the logical-frame counter crawls, because each peer waits a full network delay for the other's input before it can advance a frame. Correct and smooth, but not responsive. This is the older lockstep model rollback replaced.

That is the trade in your hands: prediction buys responsiveness, rollback buys back the correctness prediction spends, and the only alternative (delay) spends responsiveness to keep both.

Loss has its own lesson. Set **redundancy** to `none` and raise packet loss: a lost input packet now leaves a peer predicting further and rolling back deeper when the input finally arrives (a later packet). Switch to `blind` and the same loss barely shows, because every input rode in several packets. `targeted` is the same idea done by acknowledgement rather than by repetition, and the section below is what measuring the two against each other found.

## Which inputs to repeat

A rollback peer tolerates packet loss because a lost input arrives in a later packet instead. *Which* past inputs each packet repeats is a policy, and the demo offers three so the trade is visible rather than assumed. Two peers, 100 ms latency, 1200 frames, eight seeds:

| loss | policy | B/s | inputs/pkt | converged |
|---|---|---|---|---|
| 0% | none | 375 | 1.00 | 8/8 |
| | blind | 2245 | 5.99 | 8/8 |
| | targeted | 1617 | 1.00 | 8/8 |
| 15% | none | 375 | 1.00 | 0/8 |
| | blind | 2245 | 5.99 | 8/8 |
| | targeted | 2309 | 2.85 | 8/8 |
| 50% | none | 375 | 1.00 | 0/8 |
| | blind | 2245 | 5.99 | 6/8 |
| | targeted | 3264 | 5.40 | 8/8 |

**Blind** repeats the last six frames every packet whether anyone needs them or not. **Targeted** carries a `plaza_client_utils::ack::AckWindow`, twelve bytes saying exactly which frames arrived, and repeats only the gaps.

The bandwidth crossover is around 12% loss: targeted costs 28% less on a clean link and 45% more at 50%. That much was expected. Two things were not.

**Blind redundancy makes a fixed number of attempts; targeted makes as many as the link demands.** Six packets carry each input under blind and then it is gone forever, which is fine until `0.5^6` of the inputs start outliving the tail. At 50% loss blind stopped converging twice in eight runs and targeted did not. The policies are not cheap against expensive, they are **bounded effort against bounded outcome**, and only the bandwidth axis makes the first look better.

**"More attempts" is not "unlimited attempts".** At 55% loss targeted drops to 6 of 8 as well. Its bound is not the attempt count but how long the sender keeps the input: once acknowledgements are themselves being dropped, the round trip that reveals a gap can outlast the window that could fill it. Lengthening the history moves the cliff and does not remove it.

**A single-instant sync check cannot tell a desync from a recovery in progress.** The first version of this measurement reported 0 of 8 converged at every loss rate for every policy, which was wrong. A peer holding a gap it has not been resent yet has simulated a predicted input there, so it legitimately differs from its opponent for another round trip. The measurement now settles the link before asking, and the number the demo shows is a live snapshot, so a brief DESYNCED flash under loss is expected rather than a failure.

## How it is built, and what it proves

The demo depends on **`plaza_client_utils` only**, not on `plaza` core and not even on a server crate: rollback has no server. Core is async and does not target wasm; `client_utils` is zero-dependency and does, which is what lets two full peers, their wire, and the whole rollback loop run in the browser.

Each peer is a [`RollbackSession`](../../client_utils/src/rollback.rs), the drop-in bundle, wired the same way any integrator would:

- **The deterministic step** ([src/sim/types.rs](src/sim/types.rs)): one pure function, `step(state, inputs) -> state`, the same rule on both peers. Determinism is the load-bearing assumption, re-simulating from a restored frame only lands on the other peer's state because the step is identical and side-effect-free. Integer-free movement kept simple so both peers compute bit-for-bit the same result.
- **The peer** ([src/sim/peer.rs](src/sim/peer.rs)): a thin wrapper over `RollbackSession` that only decides *policy* the session leaves open, predict-or-wait, and rollback on or off. The session itself owns the mechanism: a `StateHistory` of whole-world snapshots, an `InputTimeline` per player with repeat-last prediction, and the detect / restore / re-simulate loop.
- **The wire** (`plaza_client_utils::net_sim::LatencyLink`, behind the `net-sim` feature): one delay queue per direction, carrying input packets with a redundant tail. The sliders feed its delay, jitter, and drop rate.
- **The world** ([src/sim/world.rs](src/sim/world.rs)): advances both peers one logical frame per tick, crosses their inputs over the wire, and reports whether they agree.

### What the rollback module gives you

The reusable pieces this example is built on, all in `plaza_client_utils::rollback` (full surface in the crate's [API_REFERENCE.md](../../client_utils/API_REFERENCE.md)):

- **`StateHistory`**: a frame-indexed ring of whole-world snapshots, the save-states rollback restores. Pure save/restore by frame, no interpolation.
- **`InputTimeline`**: the inputs known for one player, with the gaps predicted by repeating the last confirmed input.
- **`RollbackSession`**: the whole loop wired together, the rollback counterpart to [`PredictedPlayer`](../../client_utils/src/predicted_player.rs) for the authoritative model. You supply the deterministic step; it predicts, detects mispredictions, restores, and re-simulates.

The primitives stay public for anyone who wants to wire the loop by hand; the session is the ready-made path.

## Files

| File | What |
|---|---|
| [src/sim/](src/sim/) | the headless simulation: `types`, `peer`, `world`. Fully unit-tested without a window. The network is `plaza_client_utils::net_sim`. |
| [src/render.rs](src/render.rs), [src/ui.rs](src/ui.rs) | macroquad drawing (the two panels and banner) and the egui control panel |
| [src/main.rs](src/main.rs) | the frame loop and input |
| [static/](static/) | `index.html` and the vendored `mq_js_bundle.js` loader |
| `serve.sh`, `run-native.sh` | the two ways to run it |

The simulation logic is the valuable part and is where the tests live (`cargo test -p rollback_playground`); the renderer only reads its results. The `client_utils` rollback module has its own tests, including a two-peers-converge determinism check (`cargo test -p plaza_client_utils rollback`).

## Notes

- The crate is a workspace member but is excluded from `default-members`, so a bare `cargo build` / `test` / `check` skips macroquad's large dependency tree. `cargo <cmd> --workspace` still includes it.
- A built wasm bundle is shareable by hosting the `static/` directory anywhere.
