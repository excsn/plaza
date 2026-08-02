# encode (`session/benches/encode.rs`)

`cargo bench -p plaza_session --bench encode`

Building the identical `[tag][body]` frame, M4 Pro: `fresh` (`Vec::new()`), `hinted` (`Vec::with_capacity` from the last frame), `arena` (`BytesMut` + `split().freeze()`).

| shape | codec | fresh | hinted | arena |
|---|---|---|---|---|
| one op | json | 155.4 ns | 57.7 ns | 89.4 ns |
| one op | msgpack | 90.4 ns | 30.2 ns | 38.2 ns |
| tick batch, 16 ops | json | 929 ns | 602 ns | 1.294 µs |
| tick batch, 16 ops | msgpack | 490 ns | 338 ns | 408 ns |
| snapshot, 256 entities | json | 26.8 µs | 26.4 µs | 44.9 µs |
| snapshot, 256 entities | msgpack | 8.44 µs | 7.82 µs | 9.51 µs |
