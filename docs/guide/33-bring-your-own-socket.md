# 33. Bring your own socket

The question this chapter answers: my transport is QUIC, Steam sockets, a Unix pipe, or something stranger; what does plaza owe me, and what do I owe it?

This chapter is the block-and-prescription promise from [chapter 00](00-what-plaza-is-made-of.md) at its most demanding. The shipped transports are prescriptions; the seam they stand on is published, and a transport written outside the workspace stands on the same floor. The proof lives in the repo: [foreign_soil](../../examples/foreign_soil/) implements a Unix-socket transport and then a UDP one against the published surface alone, with neither shipped transport's feature even enabled, on the stated principle that a claim about a seam is only worth what a consumer of it proves. It found real gaps doing it, which are now pinned by its assertions; that is the seam's test suite in the shape of a stranger.

## What the seam gives you

Writing a transport is a socket pump plus calls into `ConnectionManager`, and the manager brings everything that is not literally socket I/O:

- **The registry and the bridge.** `register` a connection with its outbound queue, `forward_incoming` the raw frames you read, `deregister` on the way out. Decoding, targeting, fan-out, presence events, and the controller's streams all happen behind those three calls.
- **The measurement plane.** Answering probes, timing round trips, recording samples: assembled in `LinkDriver`, or usable piecemeal from the control module if your loop's shape disagrees with the driver's.
- **The conditioner.** [Chapter 31](31-faking-a-bad-network.md)'s impairment, per connection, both directions, so a custom transport is testable under the same fake weather as the shipped ones.
- **The order channel.** `take_orders` hands your loop the stream that `close_connection`, `set_deadline`, and the drain speak through, so governance ([chapter 40](40-the-right-to-say-no.md)) works on your transport the day it boots. It must be its own `select!` arm; the session docs explain the trap that rule exists to avoid.
- **The inbound gate.** If the session sets a `Rate`, `record_inbound_activity` returns a verdict rather than nothing, and a frame it refuses must not be forwarded. `LinkDriver::inbound` and `control::handle_inbound` return it for you as `Inbound::Shed` (drop it, keep the connection) and `Inbound::Eject` (close it); a frame that came out of the impairment queue instead reports through `LinkDriver::ejected()`, because `due()` returns what the socket is owed and a close is not a frame. What a close *looks* like is yours: the WebSocket transport spends `1008 Policy Violation` on it and TCP has no code to spend.

The session API_REFERENCE has a literal numbered recipe under "Writing Another Transport"; this chapter is the why behind its steps.

## What stays yours

Framing and limits are what a transport *is*, so they stay with you: length-delimiting a stream, enforcing the max frame size, deciding what your medium's version of "the peer went away" looks like. `LinkDriver` is explicitly a convenience, not a ceiling: it reaches for nothing a transport outside the crate cannot reach, so using the parts raw loses you nothing but the assembly.

Two constraints from the wire's design will shape a datagram transport, both stated in [chapter 30](30-bytes-on-the-wire.md): plaza is a stream wire format with no fragmentation fields, so each message must fit one datagram and yours is the code that refuses what does not; and the conditioner models a stream's physics, so genuinely unordered delivery is your simulation to write if you need it.

## The discipline that makes it work

foreign_soil's method is the part to copy, more than its code: build against the published surface only, assert the behaviors you depend on, and when the seam is missing something, record it as a finding rather than reaching around it into private API. Reaching around works exactly until the next release, and the finding is how the seam grows for everyone. That loop, examples pressing on the seam and the library growing where they had to hand-write, is how the governance surface in [chapter 40](40-the-right-to-say-no.md) came to exist at all: two examples wrote their own transports to prove what was missing, the primitives were extracted, and the rewrites then deleted both hand-written transports. The deletion is the proof of the extraction.

## The lab

[foreign_soil](../../examples/foreign_soil/) is a harness rather than a game: run it and read its assertions as a checklist of what the seam guarantees, then read its README for the gaps it found and what each one taught. It is the sole and sufficient lab for this chapter, and the best starting skeleton for a transport of your own.
