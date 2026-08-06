# netcode_playground

An interactive, in-browser demonstration of `plaza_client_utils` and `plaza_server_utils`. CSP is client-side prediction, SSR is server-side reconciliation; the demo also covers entity interpolation and lag compensation, all four parts of Gabriel Gambetta's netcode series.

The point is to *feel* the mechanisms rather than read about them: raise the latency, switch one off, and watch how it breaks.

This is the *server-authoritative* netcode model. For the other family, peer-to-peer deterministic rollback, see the sibling [`rollback_playground`](../rollback_playground/).

## What you are looking at

A box you drive, some bots you do not, and a latency slider between you and the server.

| Colour | Meaning |
|---|---|
| solid blue | your box, where the **client predicts** it is right now |
| faint white ring | your box, where the **server** last confirmed it (the "ghost") |
| solid orange | a remote box, **interpolated** a little in the past for smoothness |
| faint orange ring | that same remote box's **true** server position |

Under load the blue box pulls ahead of its ghost (prediction running before the server confirms), and snaps back when a packet arrives (reconciliation). The solid orange bots trail their faint rings (interpolation renders them slightly in the past so there are always two snapshots to blend).

## Running it

Native window, no ceremony:

```sh
./run-native.sh          # or: cargo run -p netcode_playground --release
```

Same code as the browser build, opened as a desktop window. Nothing to install.

In the browser (wasm):

```sh
./serve.sh               # then open http://localhost:8080
./serve.sh 9000          # or pick a port
```

`serve.sh` does the whole ceremony: installs the `wasm32-unknown-unknown` target if it is missing, builds the release wasm, copies it next to the page, shrinks it with `wasm-opt` if you have binaryen, and serves the directory (via `basic-http-server` if installed, else `python3 -m http.server`). Hold the left mouse button (or WASD / arrows) to move; right-click a bot to shoot. A frame counter sits bottom right, so a stutter can be told apart from a network effect.

## The controls, and what each reveals

Every toggle routes around one mechanism, so turning it off shows what that mechanism was doing:

| Control | Off shows |
|---|---|
| latency / jitter / loss / server-rate sliders | crank these first; nothing below is visible at 0 ms. Lower the server rate to see the client cope with a coarse snapshot stream |
| **adaptive buffering** | with it on, the interpolation delay grows to absorb jitter (watch the delay readout); off, a fixed delay stutters when jitter exceeds it |
| **client-side prediction** | input lag: the box waits a full round trip before moving |
| **server reconciliation** | drift: the prediction runs off (through walls, past dropped inputs) and is never corrected |
| **entity interpolation** | teleporting: remote boxes jump between server snapshots instead of gliding |
| **extrapolation (dead reckoning)** | raise packet loss: remotes freeze on their last snapshot instead of coasting along their velocity |
| **second order (fit a curve)** | coast along a fitted curve instead of the last velocity. Drop the server rate below 10 Hz first, or it does nothing at all, see below |
| **correction smoothing** | the reconciliation correction snaps in one frame instead of easing over a few |
| **clock sync** | raise the latency slider: with it on the remotes recover; off, the free-running clock drifts and interpolation stays broken |
| **smooth clock** | with it on, a latency change is absorbed by gliding the render clock's *playback rate* (watch the `clock playback` readout leave 1.00x and settle back); off, the clock corrects by nudging its *position*, a small snap each packet |
| **lag compensation** | right-click a moving bot: with it on the shot hits, off it misses |
| show server ghost | hides the authoritative overlay, if you just want to play |

Reconciliation, smoothing, and prediction form a chain: reconciliation has nothing to correct without a prediction, and smoothing eases the reconciliation correction. The dependent toggles grey out when their prerequisite is off.

The wall is the clearest single demonstration. The server clamps every box to the arena; the client's prediction does not. Push into a wall with reconciliation **on** and the blue box overshoots by about one round trip and rubber-bands back. Turn reconciliation **off** and it walks straight through and keeps going.

Lag compensation is the fourth part, and its own demonstration. Because the orange bots are drawn in the past (interpolation), you aim at where a bot *was*. Right-click one: the shot carries the server-time you were seeing. With lag comp **on** the server rewinds each bot to that time before checking the hit, through `plaza_server_utils::HistoricalStateBuffer`, and the crosshair flashes green. **Off**, the server checks the present, the bot has moved, and it flashes red. Raise the latency to widen the gap.

## How it is built, and what it proves

The demo depends on **`plaza_client_utils` and `plaza_server_utils`**, not on `plaza` core. That is deliberate and is half the point: core is async (fibre + `async_trait`) and does not target wasm, whereas both util crates are runtime-free and do. A whole working client, and a toy server, are assembled from those two and nothing else, which is the design bet the crates were built around. That the *server* crate is also wasm-safe is what lets the toy server's lag-compensation rewind run in the browser alongside the client.

Everything the client talks to is simulated in the same page, Gambetta-style:

- **Toy authoritative server** ([src/sim/server.rs](src/sim/server.rs)): a plain synchronous struct, not a plaza `StateController`. It holds every box, applies your inputs in sequence order, moves the bots, and clamps to the arena. It records each tick into a `plaza_server_utils::HistoricalStateBuffer` and rewinds through it to resolve shots. A real deployment runs the server on a server; this stands in for it.
- **Latency link** (`plaza_client_utils::net_sim::LatencyLink`, behind the `net-sim` feature): a time-ordered delay queue, one per direction, driven by the frame clock. The sliders feed its delay, jitter, and drop rate.
- **Client** ([src/sim/client.rs](src/sim/client.rs)): the `client_utils` consumer, through its drop-in bundles: a `PredictedPlayer` for your box and one `RemoteView` per remote. Plus an `InterpolationClock` (with `resync`) for the render target, and an `RttEstimator` for the round-trip readout. Each side pings the other, so both measure their latency.

The macroquad frame loop is the simulation clock, so there is no client-side tick driver: the frame delta is what the world steps by.

### What building it found, and what it added

Writing a real consumer is how gaps surface (the same method that turned up the missing turn restart in `card_table`). Three showed up (see the improvement ledger for the reasoning):

- **`InterpolationClock`** (new in `client_utils`): the interpolation render target was bookkeeping every client hand-rolls, an estimate of server time, advanced by frame delta, minus a fixed delay. It is now one small type. The client's `clock` field is it.
- **`ErrorSmoother`** (new in `client_utils`): reconciliation snaps the corrected position in one frame, which is correct per Gambetta but abrupt under high latency. `ErrorSmoother` eases only the *rendered* position toward the exact logical state, and the smoothing toggle turns it on and off. It is a standalone primitive, not a method on `PredictedEntity`, because smoothing needs to blend states (which prediction does not) and any jumping entity can use it.
- **`plaza_server_utils`** (new crate): lag compensation needed the server rewind, `HistoricalStateBuffer`, which existed in async `plaza` core, unused and unreachable from wasm. Relocating it to a runtime-free crate gave it a wasm home and its first consumer, and unifying its `Interpolatable` trait with the client's fixed a real bug: the old version's `TryInto<f32>` bound could not accept `u64` time.

## What measuring second-order dead reckoning found

The bots orbit, and an orbiting entity coasted along its last velocity leaves on the tangent, so fitting a curve through the last three snapshots should track it better. `plaza_client_utils::trajectory::TrajectoryPredictor` does exactly that, and in isolation it cuts the error over a 100 ms gap on a circular path by 45%.

In this demo it does **nothing**, and the reason is worth more than the technique:

| server rate | frames dead-reckoned | velocity | curve | change |
|---|---|---|---|---|
| 30 Hz | 0% | 0.00 px | 0.00 px | 0% |
| 20 Hz | 0% | 29.62 px | 29.62 px | 0% |
| 10 Hz | 11% | 24.13 px | 24.08 px | 0% |
| 5 Hz | 47% | 19.11 px | 17.78 px | 7% |
| 2 Hz | 79% | 35.52 px | 33.92 px | 5% |

The acceleration term goes as the *square* of the gap, so it is worth thousandths of a pixel over a short one. At a normal server rate the adaptive buffer grows its delay to cover jitter and loss, and the render target never gets more than a few milliseconds past the newest snapshot. There is no gap to improve. A better extrapolator needs something to extrapolate, and below about 10 Hz it finally has some.

Two measurement traps on the way to that table, both the same shape as ones this project has hit before:

- **Averaged over every frame the difference is invisible**, because the error is dominated by the interpolation delay that both policies pay identically. The measurement has to restrict itself to the frames actually being dead-reckoned, which is why `World::extrapolating` exists.
- **Even then it read zero**, and only a probe printing the actual gap sizes (4 ms) explained why. The metric was correct and the situation was simply not one the technique addresses.

The toggle stays because the negative result is worth being able to reproduce, and because the low-rate slider makes the regime reachable.

## Files

| File | What |
|---|---|
| [src/sim/](src/sim/) | the headless simulation: `types`, `server`, `client`, `world`. Fully unit-tested without a window. The network is `plaza_client_utils::net_sim`. |
| [src/render.rs](src/render.rs), [src/ui.rs](src/ui.rs) | macroquad drawing and the egui control panel |
| [src/main.rs](src/main.rs) | the frame loop and input |
| [static/](static/) | `index.html` and the vendored `mq_js_bundle.js` loader |
| `serve.sh`, `run-native.sh` | the two ways to run it |

The simulation logic is the valuable part and is where the tests live (`cargo test -p netcode_playground`); the renderer only reads its results.

## Notes

- The crate is a workspace member but is excluded from `default-members`, so a bare `cargo build` / `test` / `check` skips macroquad's large dependency tree. `cargo <cmd> --workspace` still includes it.
- A built wasm bundle is shareable by hosting the `static/` directory anywhere.
