# snapshots (`core/benches/snapshots.rs`)

`cargo bench -p plaza --bench snapshots -- <group>`

One `create_snapshot` and one `send_message` per recipient, M4 Pro. `immediate` is the deployed shape (every shipped provider builds its view synchronously and no shipped `send_message` awaits); `yielding` suspends without waiting on anything outside the runtime; `delayed` waits on a timer, modelling a provider that reads a database, at ~1.3 ms per call once tokio's timer granularity is accounted for.

Strategies: `sequential` is the controller's loop today, `concurrent` is `FuturesUnordered`, `handrolled` is a `poll_fn` over a `Vec` of the boxed futures `async_trait` already returns, `probed` polls the first call once and picks the loop from whether it was ready.

## snapshot_pass, immediate

| recipients | sequential | concurrent | handrolled | probed |
|---|---|---|---|---|
| 4 | 845.0 ns | 1.016 µs | 875.9 ns | 909.8 ns |
| 16 | 4.982 µs | 6.175 µs | 4.999 µs | 5.456 µs |
| 64 | 36.27 µs | 38.66 µs | 35.79 µs | 37.22 µs |
| 256 | 356.9 µs | 365.9 µs | 372.8 µs | 372.2 µs |

## snapshot_pass, yielding

| recipients | sequential | concurrent | handrolled | probed |
|---|---|---|---|---|
| 4 | 981.5 ns | 1.126 µs | 964.0 ns | 956.1 ns |
| 16 | 5.389 µs | 6.053 µs | 5.163 µs | 5.209 µs |
| 64 | 38.42 µs | 41.17 µs | 36.69 µs | 36.93 µs |
| 256 | 360.5 µs | 370.9 µs | 366.6 µs | 371.5 µs |

## snapshot_pass, delayed

| recipients | sequential | concurrent | handrolled | probed |
|---|---|---|---|---|
| 16 | 21.34 ms | 1.350 ms | 1.346 ms | 1.350 ms |
| 64 | 85.39 ms | 1.437 ms | 1.417 ms | 1.450 ms |

## snapshot_context

Per-agent `context.clone()` against borrowing it.

| context | 64 cloned | 64 borrowed | 256 cloned | 256 borrowed |
|---|---|---|---|---|
| `Full` | 40.97 µs | 37.80 µs | 381.1 µs | 376.5 µs |
| `ForPerspective(String)` | 38.47 µs | 37.46 µs | 382.0 µs | 382.5 µs |
| `Custom(Arc)` | 38.67 µs | 38.00 µs | 368.6 µs | 371.1 µs |

Two of the six pairs go the wrong way; run-to-run variance on this bench is ~5%.
