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

One message taken every 2ms, single shot rather than a median.

| depth | inbound swept | decoded swept |
|---|---|---|
| 8 | 33 | 25 |
| 16 | 41 | 25 |
| 32 | 49 | 41 |
| 64 | 89 | 73 |
| 128 | 145 | 137 |

Both slope 0.93. Point-to-point deltas are irregular because what the drainer removes during the fill is timing-dependent, so 0.93 is not distinguishable from 1.00 here. What it does show is no separation between the two queues: the decode step between them was the candidate for splitting them and does not.

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
