# 31. Faking a bad network

This chapter covers how to test your game at 200ms with jitter and loss, at your desk on localhost, with results you can trust.

On localhost everything arrives instantly and in order, so netcode bugs stay hidden until a real player on hotel wifi finds them. Plaza builds impairment into the session: the session measures it, simulates it and lets you adjust it from a slider while the game runs.

## The link conditioner

The session layer owns a per-connection **conditioner**: a delay, jitter and loss profile applied to whatever crosses the connection, in each direction independently (`LinkProfile` has an `up` and a `down`, so a symmetric 100ms round trip is 50 each way). It sits on the *link* rather than in your app's queues, so everything crosses it (your ops, snapshots and the measurement probes alike) and what you observe under impairment is what a real player would get. The default is passthrough and costs nothing.

The conditioner behaves like the real transports. Order is preserved, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall then a burst, as it does on a stream. Under the `Reliable` model (which is how TCP and WebSockets behave) a "lost" frame is retransmitted late rather than deleted, as a stall costing TCP's minimum RTO, because on a reliable stream a lost segment never reaches the application as a missing message. `Datagram` mode really drops frames, for trying out a transport you do not have yet. The jitter draw is seeded from the connection id rather than the clock, so an impaired run can be reproduced.

## Measuring the link

The same session measures every connection with probes on the frame path: `Ping` out, `Pong` back, timed by the server. WebSockets deliberately have two measurements: the socket's own ping below the conditioner and the frame-path probe through it. The gap between them is what plaza plus the configured link costs that connection, which is useful while debugging.

The crate docs recommend three habits. Compare budgets against `min_rtt` rather than the mean: jitter only ever adds delay, so the smallest sample is the best estimate of the link, while a mean misrepresents a connection that is usually fine and occasionally awful. Once a number decides anything, trust only what *the server* measured and never what a client reports; a client can only delay its own probe answer and make itself look worse, which is the safe direction. Read what the link dropped from the session's counters, because a frame the link lost never reached your code, so your code cannot have counted it.

## Client-side and test-side simulation

For tests and demos with no session in the middle, the client crate's `net-sim` feature ships `LatencyLink`, a deterministic latency, jitter and loss pipe. Impairment tooling has to behave like the transport it stands in for. `LatencyLink`'s early default reordered frames, which WebSockets cannot do. Chasing that fake reordering cost a full diagnostic cycle.

## Results that latency does not change

Some of the most useful slider experiments are the ones where nothing changes. [seed_defense](../../examples/seed_defense/) shows zero divergence from 0 to 400ms because lockstep pays for latency somewhere other than divergence. [ghost_trials](../../examples/ghost_trials/) has a test asserting that latency cannot change a lap time, because the network is not part of that calculation. When a slider fails to move a number, that tells you where your architecture's costs actually are. Every playground README lists its claims with the slider that would disprove them.

## Replacing it

The conditioner and the probe machinery are blocks a custom transport gets through `LinkDriver` or uses piecemeal ([chapter 33](33-bring-your-own-socket.md)); a transport that wants a different model writes its own conditioner and keeps everything else. The profiles are plain data set through the manager at runtime, so a debug UI only needs to send new profiles.

## The lab

Any playground with sliders works. [horde_playground](../../examples/horde_playground/) is the most complete and it is where an earlier shortcut impaired a queue the real traffic did not use, which is why the impairment has to cross the real path. Drag latency up and watch which meters move; add loss and compare what [chapter 11](11-keeping-the-pipe-small.md)'s recovery fixes against what it costs in bandwidth. Then run [ghost_trials](../../examples/ghost_trials/) and watch a number stay the same.
