# controller (`core/benches/controller.rs`)

`cargo bench -p plaza --bench controller -- <group>`

## command_queue

4096 commands through a 32-deep channel, spawned producers, one consumer, M4 Pro.

| producers, threaded | fibre | tokio |
|---|---|---|
| 1 | 1.090 ms | 1.060 ms |
| 4 | 1.096 ms | 1.039 ms |
| 16 | 1.093 ms | 1.320 ms |
| 64 | 1.023 ms | 2.106 ms |
| 1, inline | 140.6 µs | 157.8 µs |

## op_path

One command in at the session, out at every inbox, through the real controller.

| | time |
|---|---|
| ops_per_command/1 | 521.7 ns |
| ops_per_command/8 | 502.2 ns |
| ops_per_command/64 | 689.0 ns |
| tick | 443.0 ns |
| broadcast_to/1 | 438.2 ns |
| broadcast_to/4 | 559.9 ns |
| broadcast_to/16 | 1.076 µs |
| broadcast_to/64 | 3.251 µs |
| snapshot_to/1 | 537.9 ns |
| snapshot_to/16 | 4.305 µs |

Run-to-run variance on this group is ~5-10%.

Sequential against concurrent `send_snapshots`, measured back to back by checking out the parent commit's `controller.rs`:

| | sequential | concurrent |
|---|---|---|
| snapshot_to/1 | 561.5 ns | 575.1 ns |
| snapshot_to/16 | 4.217 µs | 4.213 µs |

## command_handoff, state_reply, coalesce

Not yet recorded here.
