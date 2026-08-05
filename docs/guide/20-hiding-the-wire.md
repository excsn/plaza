# 20. Hiding the wire

The question this chapter answers: the server is authoritative and eighty milliseconds away, so why does my character respond the instant I press the key?

**Read the source first.** The vocabulary of this chapter and the next, prediction, reconciliation, interpolation, lag compensation, comes from Gabriel Gambetta's Fast-Paced Multiplayer series, with Glenn Fiedler's Gaffer on Games close behind. Read Gambetta for the theory; it is short, brilliant, and this guide will not re-teach it. These two chapters do the part he leaves to you: mapping each idea to the block that implements it here, and reporting what building the examples taught about where the ideas bite. If you build apps, read on anyway with one substitution: client-side prediction *is* optimistic UI, and reconciliation is what happens when the server disagrees with your optimism.

## The four principles

The client crate opens with four principles, distilled from every netcode bug found while building the playgrounds, and the docs make a strong claim for them: everything else in the crate only *recovers* from bugs, the principles *prevent* them.

1. **A shared rule must be shared code.** If the client and server both apply movement, that is one function both compile, not two functions that agree today.
2. **Prediction is presentation.** Shared rules consume authoritative state; the predicted state exists to be drawn, never to be fed back into decisions.
3. **One instant per frame.** Everything a frame renders is evaluated at the same timestamp, not whatever each subsystem last heard.
4. **The timeline comes from declaration, not arrival.** A render clock steered by packet arrival quietly makes ping an input to the game.

They cost nothing to follow from the start and a diagnostic week each to retrofit.

## Predict, then reconcile

The mechanics are the Gambetta loop: apply your input locally the moment it happens, remember it in a [`ClientInputBuffer`](../../client_utils/API_REFERENCE.md), and when the server's authoritative state arrives for a tick you have already left behind, rewind to it and replay the inputs the server had not seen yet. Done right, a correction is invisible: the replayed state lands where prediction already put you, unless you were wrong, and being wrong is the mechanism working.

Plaza ships the loop assembled two ways, and **which one is not a taste decision, it is a property of your server's input model**. A server that consumes one input per step gets `PredictedPlayer`, which replays. A server that integrates held inputs over time gets `HeldInputPredictor`, which dead-reckons and eases. Choosing wrong is silent: replay against a held-input server double-counts inputs, and the docs flag this as exactly the kind of quiet mismatch that reads as mysterious drift.

The server half lives in core's reconciliation module (input tracking, per-client acknowledgment), because prediction is a two-sided contract, and the shared state types implement one `Interpolatable` trait so a single impl serves the client's buffers and the server's rewind in the next chapter.

## The two clocks, and the quantum

Wall time drives netcode; game time drives simulation. Keeping them separate is how a pause menu freezes the world without freezing the sockets. And on both sides, the simulation must advance in the *same fixed quantum*: `TickDriver::run_fixed` on the server, `FixedTimestep` on the client, which yields steps to you precisely so you cannot accidentally integrate by the frame delta. [bomb_grid](../../examples/bomb_grid/) earned this rule four times: four bugs, all "the two sides stepped different quanta", three of them looking exactly like network faults. Sharing the rule is not enough; sharing the timestep is required.

For timestamps that cross the wire, `RttEstimator` smooths round trips and `ClockSyncEstimator` fits offset *and drift rate* by least squares, with an honest limit stated in its docs: regression recovers the drift rate cleanly but cannot recover asymmetric route constant from RTT alone. `Timeline` keeps probe bookkeeping sane across reconnects and tab-resumes, because measurements in flight across a reconnect no longer measure the network, and feeding them to a smoothed estimator poisons it for minutes.

## When the correction shows

Corrections happen; the craft is in what the player sees. `ErrorSmoother` eases what you *draw* toward the corrected truth without ever touching the logical state (principle 2 again). Its config is a fraction per frame rather than a duration, and that is a lesson, not a preference: a fixed-duration ease has a correction rate above which it never finishes, and entities got visibly *worse* as the send rate rose. Snap versus ease is chosen by cause, not by magnitude; `CorrectionMonitor` learns what "normal" looks like for your game instead of asking you to hand-tune a threshold, because there is no fixed normal.

The discrete case deserves its own sentence: a grid game cannot ease half a cell, so [bomb_grid](../../examples/bomb_grid/) counts snaps instead of smoothing them, and its measurement is the chapter's cleanest fact: latency alone causes zero snaps when the comparison is made at the frame's own timestamp. What snaps a player is a lost input, not a slow wire.

## Ripping it apart

The bundles (`PredictedPlayer`, `HeldInputPredictor`) are wired from public primitives and nothing more, and the crate's docs name the parts so you can re-wire them: the input buffer, the predicted entity, the smoother, the estimators are each independently useful. The crate has no workspace dependencies at all, so any engine loop, wasm included, can host whichever subset you keep.

## The lab

[netcode_playground](../../examples/netcode_playground/) is the Gambetta series made tactile: prediction, reconciliation, interpolation, and lag compensation each have an off switch, so you can watch each mechanism's absence as a distinct kind of wrong. Then [bomb_grid](../../examples/bomb_grid/) for the discrete case, and [csp_net_example](../../examples/csp_net_example/) if you want the minimal headless reading path through the same loop.
