# broadcast (`session/benches/broadcast.rs`)

`cargo bench -p plaza_session --bench broadcast -- <group>`

Resolving a `MessageTarget` through the agent index against the registry scan it replaced, M4 Pro.

## one_agent, `Agent(id)`

| connections | indexed | scan |
|---|---|---|
| 8 | 36.6 ns | 27.9 ns |
| 64 | 36.9 ns | 153 ns |
| 512 | 36.8 ns | 1.17 µs |
| 4096 | 36.7 ns | 10.0 µs |

## eight_agents, `Agents(8)`

| connections | indexed | scan |
|---|---|---|
| 8 | 388 ns | 83.8 ns |
| 64 | 387 ns | 448 ns |
| 512 | 392 ns | 3.80 µs |
| 4096 | 391 ns | 31.7 µs |

## all_except_one, `AllExcept(id)`

| connections | indexed | scan |
|---|---|---|
| 8 | 59.8 ns | 54.0 ns |
| 64 | 581 ns | 539 ns |
| 512 | 6.09 µs | 5.56 µs |
| 4096 | 68.3 µs | 63.4 µs |

## exclusion_list, `AllExceptThese(k)`, 512 connections

Indexed and scan track within a few percent at every k from 4 to 128 (5.1 to 7.9 µs).
