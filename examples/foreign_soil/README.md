# foreign_soil

A transport plaza did not ship, written against the published surface only.

Every other example rides `plaza_session`'s WebSocket or TCP adapter. This one writes a Unix-socket transport the way a third party would, to find out whether the seam the docs advertise is deliverable. The audit called it "advertised but not usable from outside the crate that defines it"; this example is what settles that, because a claim about a seam is only worth what a consumer of it proves.

```
cargo run -p plaza_example_foreign_soil
```

It stands the transport up, connects a hand-written client, and prints what the seam gave and what it did not. The checks are assertions, so a change that closes a gap or opens one shows up here rather than in a paragraph.

A Unix socket rather than TCP is deliberate: no TLS, no HTTP upgrade, no address handling, so what is left under test is the seam. `plaza_session` is depended on with **neither** `tcp` nor `actix_ws`, because borrowing a shipped adapter's machinery would answer a different question. That it compiles at all is the first result.

## What the seam gives

| capability | free | how |
|---|---|---|
| registry, agent index, `Hello` | yes | `register` / `deregister` |
| inbound to the controller | yes | `forward_incoming` and the bridge behind it |
| outbound, `Session` delegation | yes | one `session_channel`, three delegated methods |
| answering and originating probes | yes | `LinkDriver`, which owns the schedule and the correlation |
| impairment, all four ordering rules | yes | `LinkDriver`, holding the same `Conditioner` the shipped adapters do |

The connection loop is **65 lines**, of which about 25 are reading and writing a socket. Before the extraction this example's loop was 113 lines and did not implement jitter, loss, monotone release, the retransmit penalty or the queue cap; a complete one would have been about 160.

## What this example found

It was written to expose gaps, and it did. Each of these is now closed, and this example is what keeps them shut, because it is the only crate here that cannot see `pub(crate)`.

**`bytes` was in the seam.** `OutboundFrame` aliased `bytes::Bytes`, so an adapter took a direct dependency on that crate and hoped the version unified. It is now a `Frame` newtype taking `Vec<u8>` or `Bytes` and reading through `AsRef` and `Deref`. This example names `bytes` nowhere.

**The probe machinery was gated on the shipped transports.** `control` carried `#[cfg(any(feature = "actix_ws", feature = "tcp"))]`, so a transport enabling neither could not reach the link plane at any visibility. That was the seam being a privilege rather than a surface, and only a consumer with neither feature could see it.

**The conditioner and the probe table were `pub(crate)`.** Reimplementing them was ~40 lines for the probe correlation, whose failure mode is a table that leaks on a lossy link, and four ordering rules for the conditioner, all of which are silent when wrong. Both are public now, and `LinkDriver` assembles them so most adapters never touch either.

**The recipe produced a broken adapter.** It showed `register` and `forward_incoming` as synchronous, said nothing about answering `Kind::Ping`, and left `set_link_profile` a no-op. A probe frame reaching the deserialize bridge now warns once per connection instead of tracing, because that is a defect in the transport rather than a property of the traffic.

## What is still yours

Framing, and enforcing `Limits::max_frame_bytes` with it. Those are what a transport *is*.

And the parts, if the bundle does not suit. `Conditioner`, `ProbeState` and `LinkHandle` are public and each is useful alone, so a transport whose link genuinely reorders can keep the probe plane and write its own release queue: the shipped conditioner releases monotonically because a byte stream does not reorder, and that assumption is stated rather than hidden.
