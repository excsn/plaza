# foreign_soil

A transport plaza did not ship, written against the published surface only.

Every other example rides `plaza_session`'s WebSocket or TCP adapter. This one writes a Unix-socket transport the way a third party would, to find out whether the seam the docs advertise is deliverable. The audit called it "advertised but not usable from outside the crate that defines it"; this example is what settles that, because a claim about a seam is only worth what a consumer of it proves.

```
cargo run -p plaza_example_foreign_soil
```

It stands the transport up, connects a hand-written client, and prints what the seam gave and what it did not. The checks are assertions, so a change that closes a gap or opens one shows up here rather than in a paragraph.

A Unix socket rather than TCP is deliberate: no TLS, no HTTP upgrade, no address handling, so what is left under test is the seam. `plaza_session` is depended on with **neither** `tcp` nor `actix_ws`, because borrowing a shipped adapter's machinery would answer a different question. That it compiles at all is the first result.

## What the seam gave

| capability | free | what it cost |
|---|---|---|
| registry, agent index, `Hello` | yes | `register` and `deregister` |
| inbound to the controller | yes | `forward_incoming`, and the deserialize bridge behind it |
| outbound, `Session` delegation | yes | one `session_channel` and three delegated methods |
| answering a peer's probes | yes | `frame::answer_ping`, one match arm |
| measuring the link | **no** | ~40 lines: the schedule is public, the correlation is not |
| impairment | **no** | delay only, by hand; the rest is unreachable |

## What it had to reimplement, and what that risks

**Probe correlation.** `Probes` carries the schedule and `record_link_rtt` takes the result, so originating probes is straightforward. Matching a `Pong` to the probe it answers, and discarding the older ones it skipped, is `ProbeState` and is `pub(crate)`. Getting the discard wrong leaks the table on a lossy link, which nothing surfaces until memory grows.

**Framing.** `LengthDelimitedCodec` reaches an adapter only through the `tcp` feature, so a transport that does not want TCP compiled in writes its own. `Limits::max_frame_bytes` then becomes a number the adapter enforces rather than one the crate enforces for it.

**Impairment, which is the real wall.** `set_agent_link_profile` is public and an application will call it, and `link_profile` lets an adapter see the result, but the queue that implements it is `pub(crate)`. This example honours the delay and nothing else, and says so rather than pretending. Reimplementing it means getting four rules right that are invisible when wrong: release times made monotone so a delayed frame holds up what is behind it, a loss under `Delivery::Reliable` costing `RETRANSMIT_PENALTY` rather than deleting the frame, a queue cap that refuses `Kind::Ops` only so a full queue never wedges a handshake, and the passthrough fast path applying only when the queue is also empty.

**`bytes` itself.** `OutboundFrame` is `bytes::Bytes`, and neither the type alias nor the crate is re-exported from the crate root, so an adapter depends on `bytes` directly and hopes the version unifies. `session_channel` exists to spare a transport exactly this for the channel crate; nothing does it for `bytes`.

## The privilege gap, now measurable

The shipped adapters hold a `pub(crate)` handle whose impaired check is one relaxed atomic load. A third-party adapter reads `manager.link_profile(conn_id)`, which takes the registry's lock, per frame per direction. `docs/benches/passthrough.md` prices that difference at 7.3x on the path every frame takes.

Building this example with no shipped transport compiled in makes the compiler say the same thing: `link_handle` warns as never used, because the only callers are the adapters plaza ships.
