# table_manners

Moderation as a live tool, and the surface a host needs to use one.

```sh
cargo test -p plaza_example_table_manners
```

A four-seat party with host tools. Every claim is a count, and the counts are asserted rather than printed, because each one is a property that has to hold every time rather than a number to admire.

## A kick must say why, and saying it races the close

The entry expected this to start imperfect by construction, since `deregister` drops the outbound queue along with the connection and a farewell queued behind it never leaves. It does, and the fix it forces is exactly the one predicted: **write the reason, flush, then shut the socket**. With that ordering, `reasons delivered == kicks`, and `silent closes == 0`, asserted on every kick and again on a room-wide drain.

That ordering cannot be expressed through the shipped transports at all, for the reason [`door_policy`](../door_policy/) recorded: `deregister` is bookkeeping, not a close.

## A kick is not a netdrop

The same guest leaving two ways, told apart by one field the socket cannot supply:

| how they left | seat | rejoin |
|---|---|---|
| socket cut | **held**, with grace | allowed |
| removed by the host | **cleared** | refused at the door |

A parting defaults to `Dropped` and only a deliberate close overrides it, which is the right default: a crash cannot announce itself. `ReconnectTracker` holds a seat through a drop on purpose, so the whole distinction is the difference between honouring that and clearing it, and nothing in the session layer records which happened.

## AFK is a policy on a number the transport already sees

Last activity is one store per inbound frame, on a path that already runs. Nothing keeps it, so an application either writes its own transport, as here, or invents a second heartbeat over the top of the link plane's existing one.

**A probe is not activity, and only the transport can tell.** `LinkDriver::inbound` answers a `Ping` itself and returns `Consumed`; that distinction is invisible above the session layer. An AFK timeout written against decoded ops is correct only by accident, and one written against frames would never fire at all. Both halves are asserted: silence removes you, and talking across twice the timeout does not.

## The griefer floods

Per-connection inbound rate, with the kick threshold as policy. The claim to falsify was that **the flood degrades the flooder's connection before it degrades anyone else's tick**, and it holds: the flooder's excess is shed on the connection that sent it, and the session-wide `inbound_dropped` does not move while the flood runs. A bystander keeps its seat and keeps receiving the table.

Throttling is deliberately out of scope, per the entry: surviving a flooder without ejecting them is a backpressure mechanism these meters do not force.

## Drain finishes the story

The host ends the party: everyone is told, then closed, in seat order. `delivered == drained`, `silent closes == 0`. That is a graceful server restart, and it is the same flush semantic as the kick applied room-wide.

## Extraction this earns

On top of everything [`door_policy`](../door_policy/) already named:

- **last activity per connection**, one relaxed store where the frames already arrive, readable per connection
- **per-connection inbound meters** (ops, frames, bytes per window). `TransportStats` counts the *session*, so "who is flooding" and "did it cost anyone else" cannot be answered from it
- **`disconnect_all`**, with the same flush-then-close ordering as a single kick
- **a reason on the parting**, so the session layer can tell `ReconnectTracker` whether a seat survives. This is the one that cannot be worked around cheaply: an application can time its own AFK, but it cannot see the difference between a socket that died and one it closed

## The wire question, answered by need

Is a close reason a `Kind` or an `Op`? **An op.** TCP has no native close vocabulary, so a `Kind` would have to be invented for it there, while the op path already exists on both transports and already carries application meaning. WS has a close frame with a code, and it is the wrong shape anyway: the codes are a fixed registry, and `removed by the host` versus `away from the table` is application vocabulary. The close frame stays what it is, a transport event, and the farewell rides the ops path in front of it.
