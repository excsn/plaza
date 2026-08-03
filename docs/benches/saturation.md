# saturation (`session/benches/saturation.rs`)

`cargo bench -p plaza_session --bench saturation -- <scenario>`

One consumer stalled, counting what got through before the queue refuses, over a TCP session with 4 KiB JSON frames, M4 Pro. Depth is invisible to this crate's other benches, which measure uncontended paths: a `try_send` into a queue of 64 costs what it costs into one of 4096.

The reading is the **slope**. One more slot should absorb one more frame, so slope 1.00 says the configured depth is the binding term. The intercept is buffering plaza does not own.

## outbound, median of 5

A client that never reads its socket.

| depth | frames accepted |
|---|---|
| 64 | 198 |
| 128 | 261 |
| 256 | 389 |
| 512 | 646 |
| 1024 | 1158 |

slope 1.00, intercept 134. Accepted is `depth + 134` at every point, within one frame. The intercept is the kernel send buffer plus the framed writer, so it is a byte budget rather than a frame count: at 4 KiB per frame it is roughly 540 KiB, and a build sending larger frames should expect proportionally fewer. Unmeasured at a second frame size.

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
