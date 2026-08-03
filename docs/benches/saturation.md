# saturation (`session/benches/saturation.rs`)

`cargo bench -p plaza_session --bench saturation -- <scenario>`

One consumer stalled, counting what got through before the queue refuses, over a TCP session with 4 KiB JSON frames, M4 Pro. Depth is invisible to this crate's other benches, which measure uncontended paths: a `try_send` into a queue of 64 costs what it costs into one of 4096.

The reading is the **slope**. One more slot should absorb one more frame, so slope 1.00 says the configured depth is the binding term. The intercept is buffering plaza does not own.

## outbound, median of 5

A client that never reads its socket, at three frame sizes.

| depth | 512 B | 4 KiB | 40 KiB |
|---|---|---|---|
| 64 | 1113 | 199 | 78 |
| 128 | 1175 | 261 | 142 |
| 256 | 1297 | 391 | 270 |
| 512 | 1561 | 646 | 526 |
| 1024 | 2074 | 1157 | 1038 |

slope 1.00 at all three sizes; intercepts 1049, 135 and 14 frames.

| frame bytes | intercept, frames | intercept, KiB |
|---|---|---|
| 512 | 1049 | 524 |
| 4096 | 135 | 541 |
| 40960 | 14 | 560 |

**The intercept is a byte budget, not a frame count**: about 540 KiB across an 80x range of frame sizes. The 7% drift is rounding, since a partly-buffered frame counts as a whole one and that is worth more at 40 KiB than at 512 B.

It is socket buffering plus the framed writer, and on loopback both ends are the same machine: macOS 26.4.1 starts `net.inet.tcp.sendspace` at 128 KiB and auto-tunes to `net.inet.tcp.autosndbufmax` of 4 MiB, with `recvspace` another 128 KiB filling on the stalled client's side. What transfers to another platform is the shape, byte-fixed with slope 1, not the constant.

The consequence for sizing: `outbound` is the binding term only once frames are large. At 512 B the socket already holds a thousand frames, so the default depth of 64 adds 6% to what a client can fall behind by; at 40 KiB it holds 14, and the queue is nearly all of it.

## inbound and decoded

A controller that never subscribes. The bridge blocks moving frames from one queue to the other, so the two sit in series with no consumer between them.

| depth | inbound swept, decoded 8 | decoded swept, inbound 8 |
|---|---|---|
| 8 | 17 | 17 |
| 16 | 25 | 25 |
| 32 | 41 | 41 |
| 64 | 73 | 73 |
| 128 | 137 | 137 |

Both slope 1.00, intercept 9. Accepted is `inbound + decoded + 1` exactly. **The two depths are interchangeable**: moving a slot from one to the other changes nothing.

## inbound and decoded, draining controller

One message taken every 2ms, median of 5. The drainer counts what it removes and stops when the flood does, so absorption is `accepted - taken` rather than `accepted`.

| depth | inbound swept | decoded swept |
|---|---|---|
| 8 | 15 | 15 |
| 16 | 24 | 24 |
| 32 | 39 | 26 |
| 64 | 72 | 55 |
| 128 | 135 | 120 |

Slopes 1.00 and 0.88. The inbound sweep is on the undrained control, its intercept two frames lower for the messages in flight through the bridge when the drainer stops.

The decoded sweep is not, and its depth-32 point is non-monotone against its own neighbours, which reads as noise rather than as a separation between the queues.

What the sequence of attempts says is worth more than either number. Inferring absorption from the accepted total gave 0.93 and 0.93; medians alone gave 1.07 and 0.87; counting the drainer's messages but reading the count after the settle, while it was still emptying the queues, gave 0.12 and 0.02. Only counting over the right window resolved anything, and it resolved one sweep of two. **The undrained pair carries the conclusion**: byte-identical integers, six runs.

## per-connection footprint

256 silent connections per preset, flooded until every outbound queue is full, resident bytes divided by connections. A controller drains presence, which a measurement has to model: without one, a preset carrying `PresenceOverflow::Backpressure` blocks every registration at the queue depth.

`broadcast` is 256 connections receiving one frame each time; `per-recipient` is 64 connections each addressed alone, which is the snapshot path and what `memory_budget` is derived against. The overflow policy is forced to `Drop`, since `Disconnect` empties the queue being measured.

| preset | outbound | max payload | derived, B | broadcast, B | per-recipient, B |
|---|---|---|---|---|---|
| action | 4 | 512 | 2048 | 1472 | 132352 |
| horde | 47 | 40960 | 1925120 | 66176 | 2198784 |
| turn_based | 4 | 8192 | 32768 | 20160 | 15104 |
| social_relay | 4 | 512 | 2048 | 0 | 42240 |
| spectator | 4 | 8192 | 32768 | 0 | 14848 |
| lobby | 4 | 4096 | 16384 | 3072 | 8960 |
| local | 4 | 4096 | 16384 | 320 | 0 |

**`horde` is the row that validates the derivation**: 2198784 measured against 1925120 derived, 14% over, the excess being per-connection task and socket overhead. It is the only preset whose retained queue is large enough to dominate what the flood allocates, so it is the only one where this reads as memory rather than as allocator churn.

Everywhere else the reading is `retained + some fraction of churn`, and churn wins. Overrunning the socket takes about 2000 sends per connection at 512 B against 124 at 40 KiB, and a freed buffer does not shrink the arena, so `action` shows 132 KiB per connection for a queue that retains 2 KiB.

The `0`s are the method's floor, not a measurement: `ps` reports whole KiB for the process, which is 4 B per connection at 256 and 16 B at 64. `broadcast` reads zero wherever one refcounted frame is all that is retained.

Taken together: `memory_budget` is checked against a formula that holds where it matters, and this method can only confirm it where the queue is large.

## presence

Connections arriving against a controller that never subscribes.

| depth | joins accepted |
|---|---|
| 8 | 8 |
| 16 | 16 |
| 32 | 32 |
| 64 | 64 |
| 128 | 128 |

slope 1.00, intercept 0. Exact: nothing buffers underneath.

## conditioner

A 30s link delay, outbound deep enough that the conditioner is what fills.

| depth | frames accepted |
|---|---|
| 8 | 8 |
| 16 | 16 |
| 32 | 32 |
| 64 | 64 |
| 128 | 128 |

slope 1.00, intercept 0.
