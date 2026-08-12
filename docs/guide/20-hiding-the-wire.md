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

Corrections happen; the craft is in what the player sees. `ErrorSmoother` eases what you *draw* toward the corrected truth without ever touching the logical state (principle 2 again). It offers a duration **or** a fraction per frame, and which you want is a lesson rather than a preference: a fixed-duration ease has a correction rate above which it never finishes, and past that point entities get visibly *worse* as the rate rises. The crossover is exactly the duration. Measured against a two-unit correction with a 0.1s ease, worst visual error holds at 2.67 while corrections arrive every 0.5s or every 0.1s, then goes to 6.67 at every 0.05s and 15.00 at one every frame; `ErrorSmoother::at_rate(0.85)` gives 4.41 and 11.33 for the same two cases. Below the crossover the duration is the better pick and the swap buys nothing, which is why both are there.

Magnitude is a separate axis from rate. A fixed duration clears a large error and a small one in the same time, so the large one merely travels faster, and that is backwards: a small offset is invisible and can afford to linger, while a large one is already visible and every extra frame it survives is a frame the entity is somewhere it is not. `AdaptiveDecay` is the rate that accounts for it, keeping 0.95 of the error per frame under a quarter unit and 0.85 over one. Snap versus ease, though, is chosen by *cause* and not by magnitude; `CorrectionMonitor` learns what "normal" looks like for your game instead of asking you to hand-tune a threshold, because there is no fixed normal.

The discrete case deserves its own sentence: a grid game cannot ease half a cell, so [bomb_grid](../../examples/bomb_grid/) counts snaps instead of smoothing them, and its measurement is the chapter's cleanest fact: latency alone causes zero snaps when the comparison is made at the frame's own timestamp. What snaps a player is a lost input, not a slow wire.

## The cheapest prediction is the one the design made unnecessary

Before you reach for any of this, ask what the player is already waiting for. [gow_3d](../../examples/gow_3d/) is the counter-example to the whole chapter: a zone of characters with no prediction, no reconciliation, no input buffer, no sequence numbers and no correction to ease off, that still feels responsive on a bad connection, because the genre's designers made the player wait before the network ever got involved.

An ability with a cast time hides its round trip, because the bar is already running. Be precise about the mechanism, though, since "cast times hide latency" is repeated more often than it is examined: **the delay never shrinks.** It is the same 150ms at every cast time. What changes is the *share* of the wait that was delay, and the share is what a player perceives.

```
      cast     rtt 30    rtt 150    rtt 300
         0       100%       100%       100%
       400         7%        27%        43%
      1500         2%         9%        17%
```

At a cast time the genre actually uses, a bad connection is a smaller share of the wait than a good connection is of an instant ability. The global cooldown does the same job for the *inputs* that a cast time does for the outcome: a player who cannot act again for a second and a half is a player whose next input was never going to be frame-tight, which is why an instant ability is not the exception it looks like. The two waits overlap rather than stack, so a long cast is free rather than doubly expensive.

Set that beside [puck_rink](../../examples/puck_rink/), which spends owned fixed-point arithmetic, per-frame digests and re-simulation of every confirmed frame to hide a hundred milliseconds on five bodies. Both are correct answers. The difference is that one asked the network and the other asked the designer, and it is worth knowing which question you are allowed to ask before you build the machinery in this chapter.

## Ripping it apart

The bundles (`PredictedPlayer`, `HeldInputPredictor`) are wired from public primitives and nothing more, and the crate's docs name the parts so you can re-wire them: the input buffer, the predicted entity, the smoother, the estimators are each independently useful. The crate has no workspace dependencies at all, so any engine loop, wasm included, can host whichever subset you keep.

## The lab

[netcode_playground](../../examples/netcode_playground/) is the Gambetta series made tactile: prediction, reconciliation, interpolation, and lag compensation each have an off switch, so you can watch each mechanism's absence as a distinct kind of wrong. Then [bomb_grid](../../examples/bomb_grid/) for the discrete case, and [csp_net_example](../../examples/csp_net_example/) if you want the minimal headless reading path through the same loop.
