# 33. Bring your own socket

This chapter covers what plaza provides and what it expects when your transport is QUIC, Steam sockets, a Unix pipe or something else.

This chapter applies the block-and-prescription split from [chapter 00](00-what-plaza-is-made-of.md) to transports. The shipped transports are prescriptions built on a published seam and a transport written outside the workspace has the same access to it. [foreign_soil](../../examples/foreign_soil/) implements a Unix-socket transport and then a UDP one using only the published API, with neither shipped transport's feature enabled, to check that the seam is actually enough for an outside consumer. It found real gaps doing it, which are now pinned by its assertions, so it works as the seam's test suite written from outside.

## What the seam gives you

Writing a transport is a socket pump plus calls into `ConnectionManager` and the manager provides everything except the socket I/O itself:

- **The registry and the bridge.** `register` a connection with its outbound queue, `forward_incoming` the raw frames you read, `deregister` on the way out. Decoding, targeting, fan-out, presence events and the controller's streams all happen behind those three calls.
- **The measurement plane.** Answering probes, timing round trips, recording samples: assembled in `LinkDriver` or usable piecemeal from the control module if your loop does not fit the driver's shape.
- **The conditioner.** [Chapter 31](31-faking-a-bad-network.md)'s impairment, per connection, both directions, so a custom transport can be tested under the same simulated conditions as the shipped ones.
- **The order channel.** `take_orders` hands your loop the stream that `close_connection`, `set_deadline` and the drain send through, so governance ([chapter 40](40-the-right-to-say-no.md)) works on your transport from the start. Each order carries a `Farewell`: flush what is queued, write `farewell.encode(&codec)` (a `Kind::Goodbye` frame) last, then close with its code where your medium has one. It must be its own `select!` arm, because the outbound arm is disabled the moment `deregister` drops the sender, which is exactly when a close must still work.
- **The door.** A transport that admits by credential reuses the shipped one's rules: `begin_pending` takes a place under `Limits::pending_connections` and `admission::Pending::on_frame` reads each frame before admission and yields the credential or a refusal. Your `ConnectionAdmitter` then answers. Nothing is registered until it says `Admitted`.
- **The inbound gate.** If the session sets a `Rate`, `record_inbound_activity` returns a verdict rather than nothing and a frame it refuses must not be forwarded. `LinkDriver::inbound` and `control::handle_inbound` return it for you as `Inbound::Shed` (drop it, keep the connection) and `Inbound::Eject` (close it); a frame that came out of the impairment queue instead reports through `LinkDriver::ejected()`, because `due()` returns what the socket is owed and a close is not a frame. Close an ejected connection with `Farewell::new(POLICY_VIOLATION)` (1008), as both shipped transports do: WebSocket repeats the code in its close frame and on TCP the `Goodbye` frame is the only place it travels.

The session usage guide has the whole connection loop as code under [Writing Another Transport](../../session/README.USAGE.md#writing-another-transport); this chapter explains the reasons behind it.

## What stays yours

Framing and limits depend on the medium, so they stay with you: length-delimiting a stream, enforcing the max frame size and deciding what "the peer went away" looks like on your medium. For a byte stream, `plaza_wire::framing` names the 4-byte big-endian length prefix the TCP transport uses and `LengthDelimited` decodes it from bytes you feed in, refusing a frame over the size you give it. `LinkDriver` is only a convenience: it uses nothing a transport outside the crate cannot use, so using the parts directly only costs you the assembly.

Two constraints from the wire's design affect a datagram transport, both covered in [chapter 30](30-bytes-on-the-wire.md). Plaza is a stream wire format with no fragmentation fields, so each message must fit one datagram and your code has to refuse what does not. The conditioner models a stream, so if you need genuinely unordered delivery you have to simulate it yourself.

## How foreign_soil was built

Copy foreign_soil's method more than its code: build against the published API only, assert the behaviors you depend on and when the seam is missing something, record it as a finding instead of reaching into private API. Code that reaches into private API can break on the next release, while a recorded finding gets the seam extended for everyone. The governance API in [chapter 40](40-the-right-to-say-no.md) was built this way, from examples that needed a primitive and recorded its absence.

## The lab

[foreign_soil](../../examples/foreign_soil/) is a harness rather than a game: run it and read its assertions as a checklist of what the seam guarantees, then read its README for the gaps it found and what each one showed. It is the only lab for this chapter and the best starting point for a transport of your own.
