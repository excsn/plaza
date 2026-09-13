# 20. Hiding the wire

This chapter covers why your character responds the instant you press the key when the server is authoritative and eighty milliseconds away.

The vocabulary of this chapter and the next (prediction, reconciliation, interpolation and lag compensation) comes from Gabriel Gambetta's Fast-Paced Multiplayer series and Glenn Fiedler's Gaffer on Games articles. Read Gambetta for the theory first; it is short and this guide does not repeat it. These two chapters map each idea to the block that implements it here and report where the ideas caused trouble while building the examples. If you build apps, the same applies with one substitution: client-side prediction is optimistic UI and reconciliation is what happens when the server rejects an optimistic update.

## The four principles

The client crate's docs start with four principles, drawn from the netcode bugs found while building the playgrounds. The docs say everything else in the crate only *recovers* from bugs, while the principles *prevent* them.

1. **A shared rule must be shared code.** If the client and server both apply movement, they both compile the same function instead of each keeping a copy that happens to agree today.
2. **Prediction is presentation.** Shared rules consume authoritative state; the predicted state is only drawn and is never fed back into decisions.
3. **One instant per frame.** Everything a frame renders is evaluated at the same timestamp, rather than at whatever time each subsystem last heard about.
4. **The timeline comes from declaration, not arrival.** If the render clock is steered by packet arrival, ping becomes an input to the game.

Following them from the start costs nothing. Retrofitting each one has cost about a week of diagnosis.

## Predict, then reconcile

The mechanics are the Gambetta loop: apply your input locally the moment it happens, remember it in a [`ClientInputBuffer`](../../client_utils/API_REFERENCE.md) and when the server's authoritative state arrives for a tick you have already left behind, rewind to it and replay the inputs the server had not seen yet. When prediction was right, the replayed state lands where you already are and nothing visible happens. When it was wrong, the replay moves you to the corrected position.

Plaza ships the loop assembled two ways and which one you need depends on your server's input model. A server that consumes one input per step gets `PredictedPlayer`, which replays. A server that integrates held inputs over time gets `HeldInputPredictor`, which dead-reckons and eases. Choosing wrong gives no error: replay against a held-input server double-counts inputs, which the docs point out looks like unexplained drift.

Prediction needs a server half too, which lives in core's reconciliation module (input tracking, per-client acknowledgment). The shared state types implement one `Interpolatable` trait, so a single impl serves the client's buffers and the server's rewind in the next chapter.

## The two clocks and the quantum

The simulation runs on game time, which a pause menu can stop, while netcode keeps running on wall time so the sockets stay open. On both sides the simulation must advance in the *same fixed quantum*: `TickDriver::run_fixed` on the server and `FixedTimestep` on the client, which hands you whole steps so you cannot accidentally integrate by the frame delta. [bomb_grid](../../examples/bomb_grid/) had four bugs where the two sides stepped different quanta and three of them looked exactly like network faults.

For timestamps that cross the wire, `RttEstimator` smooths round trips and `ClockSyncEstimator` fits offset *and drift rate* by least squares. Its docs state the limit: regression recovers the drift rate cleanly but cannot recover an asymmetric route constant from RTT alone. `Timeline` keeps probe bookkeeping correct across reconnects and tab resumes, because measurements in flight across a reconnect no longer measure the network and feeding them to a smoothed estimator skews it for minutes.

## Visible corrections

`ErrorSmoother` eases what you *draw* toward the corrected state without touching the logical state (principle 2). It offers a duration **or** a fraction per frame and which you want depends on how often corrections arrive. A fixed-duration ease never finishes once corrections arrive more often than its duration and past that point entities look visibly *worse* as the rate rises. Measured against a two-unit correction with a 0.1s ease, worst visual error holds at 2.67 while corrections arrive every 0.5s or every 0.1s, then goes to 6.67 at every 0.05s and 15.00 at one every frame; `ErrorSmoother::at_rate(0.85)` gives 4.41 and 11.33 for the same two cases. Below the crossover the duration is the better choice and switching buys nothing, so the smoother offers both.

Magnitude matters separately from rate. A fixed duration clears a large error and a small one in the same time, so the large one just moves faster. It should be the other way round: a small offset is invisible and can linger, while a large one is already visible and every extra frame shows the entity in the wrong place. `AdaptiveDecay` accounts for this, keeping 0.95 of the error per frame under a quarter unit and 0.85 over one. Whether to snap or ease depends on the *cause* of a correction rather than its size. `CorrectionMonitor` learns what normal corrections look like for your game instead of asking you to hand-tune a threshold, since the right threshold differs from game to game.

A grid game cannot ease half a cell, so [bomb_grid](../../examples/bomb_grid/) counts snaps instead of smoothing them. It measured zero snaps from latency alone when the comparison is made at the frame's own timestamp. Snaps come from lost inputs rather than from a slow wire.

## When the design removes the need for prediction

Before you reach for any of this, check what the player is already waiting for. [gow_3d](../../examples/gow_3d/) is a zone of characters with no prediction, no reconciliation, no input buffer, no sequence numbers and no correction to ease off. It still feels responsive on a bad connection, because the genre's design makes the player wait before the network is involved.

An ability with a cast time hides its round trip, because the bar is already running. The delay itself does not shrink: it is the same 150ms at every cast time. What changes is the *share* of the wait that is network delay and the share is what a player perceives.

```
      cast     rtt 30    rtt 150    rtt 300
         0       100%       100%       100%
       400         7%        27%        43%
      1500         2%         9%        17%
```

At the cast times the genre actually uses, a bad connection is a smaller share of the wait than a good connection is of an instant ability. The global cooldown does for the *inputs* what a cast time does for the outcome: a player who cannot act again for a second and a half does not need their next input to be frame-tight. That covers instant abilities too. The two waits overlap rather than add up, so a long cast costs nothing extra.

Compare [puck_rink](../../examples/puck_rink/), which uses its own fixed-point arithmetic, per-frame digests and re-simulation of every confirmed frame to hide a hundred milliseconds on five bodies. puck_rink handles the latency in its netcode and gow_3d handles it in its game design; both work. Check whether your design can absorb the latency before you build the machinery in this chapter.

## Replacing it

The bundles (`PredictedPlayer`, `HeldInputPredictor`) are built only from public primitives and the crate's docs name the parts so you can rebuild them: the input buffer, the predicted entity, the smoother and the estimators each work on their own. The crate has no workspace dependencies at all, so any engine loop, wasm included, can use whichever subset you keep.

## The lab

[netcode_playground](../../examples/netcode_playground/) puts the Gambetta series into a playable demo: prediction, reconciliation, interpolation and lag compensation each have an off switch, so you can see what goes wrong without each one. Then [bomb_grid](../../examples/bomb_grid/) for the discrete case and [csp_net_example](../../examples/csp_net_example/) if you want the minimal headless reading path through the same loop.
