# 31. Faking a bad network

The question this chapter answers: how do I test my game at 200ms with jitter and loss, at my desk, on localhost, and trust what I see?

Localhost is a lie. Everything arrives instantly and in order, so every netcode bug waits politely until a real player on hotel wifi finds it. Plaza's answer is to make impairment a first-class, deliberate act: measured by the session, simulated by the session, and adjustable from a slider while the game runs.

## Impairment belongs to the link

The session layer owns a per-connection **conditioner**: a delay, jitter, and loss profile applied to whatever crosses the connection, in each direction independently (`LinkProfile` has an `up` and a `down`, so a symmetric 100ms round trip is 50 each way). Belonging to the *link* rather than to your app's queues is the design point: everything crosses it, your ops, snapshots, and the measurement probes alike, so what you observe under impairment is what a real player would get, not a simulation that quietly exempts the machinery. The default is passthrough and costs nothing.

The semantics are faithful to what the transports actually are. Order is preserved, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall then a burst, which is what jitter does to a stream. Under the `Reliable` model (the truth about TCP and WebSockets) a "lost" frame is not deleted, it is retransmitted late, a stall priced at TCP's minimum RTO, because on a reliable stream a lost segment never reaches the application as a missing message. `Datagram` mode really drops, for rehearsing a transport you do not have yet. The jitter draw is seeded from the connection id, not the clock, so an impaired run reproduces.

## Measure before and after you break it

The same session measures every connection with a probe plane that rides the frame path: `Ping` out, `Pong` back, timed by the server. On WebSockets there are deliberately two planes, the socket's own ping underneath the conditioner and the frame-path probe through it, and the gap between them is what plaza plus the configured link is costing that connection, which is a number you want while debugging.

Three habits from the crate docs are worth adopting whole. Compare budgets against `min_rtt`, not the mean, because jitter only ever adds delay, so the smallest sample is the honest estimate of the link and a mean flatters a connection that is usually fine and occasionally awful. Trust only what *the server* measured, never what a client reports, the moment the number gates anything; a client can only delay its own probe answer and make itself look worse, which is the safe direction. And read what the link dropped from the session's counters, because what the link lost never reached your code, which is the whole point of losing it, and therefore yours cannot have counted it.

## Client-side and test-side simulation

For tests and demos with no session in the middle, the client crate's `net-sim` feature ships `LatencyLink`, a deterministic latency, jitter, and loss pipe, and it carries a hard-earned rule: impairment tooling must be faithful to the transport it stands in for. Its early default reordered frames, which WebSockets cannot do, and chasing that phantom reordering cost a full diagnostic cycle. If your fake network can do things your real one cannot, it manufactures bugs.

## What the sliders cannot move

The most instructive slider experiments are the ones where nothing happens. [seed_defense](../../examples/seed_defense/) shows zero divergence from 0 to 400ms because lockstep pays for latency in a different currency. [ghost_trials](../../examples/ghost_trials/) asserts in a test that latency cannot change a lap time, because the link is simply not in that loop. When a slider fails to move a number, you have learned where your architecture's costs actually live, and that is the lesson the playgrounds are built around: every README's claims come with the slider that would falsify them.

## Ripping it apart

The conditioner and the probe machinery are blocks a custom transport gets through `LinkDriver` or uses piecemeal ([chapter 33](33-bring-your-own-socket.md)); a transport that wants different physics writes its own and loses nothing else. The profiles are plain data set through the manager at runtime, which is all a debug UI needs to publish.

## The lab

Any playground with sliders: [horde_playground](../../examples/horde_playground/) is the fullest, and it carries the meta-lesson that the impairment must cross the real path, learned when an earlier shortcut impaired a queue the real traffic did not use. Drag latency up and watch which meters move; add loss and watch [chapter 11](11-keeping-the-pipe-small.md)'s recovery earn its bandwidth. Then run [ghost_trials](../../examples/ghost_trials/) and watch a number refuse to move.
