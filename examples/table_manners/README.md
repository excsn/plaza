# table_manners

Moderation as a live tool, built from the library's blocks with no transport of its own.

```sh
cargo test -p plaza_example_table_manners
```

A four-seat party with host tools. Every claim is a count, and the counts are asserted rather than printed, because each one is a property that has to hold every time rather than a number to admire.

This example originally shipped a 252-line hand-written TCP transport, because AFK, flood attribution, the kick and the drain were impossible on the shipped one. Those became library primitives, and the rewrite deleted the transport. The last thing the transport could do that the library could not, shedding a flood without ejecting the sender, is `plaza_session::gate` now. What follows is the recipe.

## The blocks, and what each tool sits on

| tool | block it uses | policy that stays here |
|---|---|---|
| kick with a reason | `deregister_agent(&key, farewell)` | who may kick, and what the reason says |
| AFK removal | `agent_idle_for(&key)` | the timeout, and the steward that applies it |
| flood shedding | `rate_limit_inbound(Rate)` | how fast is too fast, and that too fast is not a removal |
| flood attribution | `agent_inbound(&key)` | the window it displays, the tolerance, the removal |
| drain | `disconnect_all(farewell)` | when the party ends |
| seat fate | `Parting::keeps_the_seat()` on the `Left` | a drop holds the seat, everything else clears it |

## A kick says why, and the reason wins the race

The farewell is a `PartyOp` of this crate's own vocabulary, pre-encoded and handed to the close; the library flushes what was queued, writes it last, and shuts the socket. Delivery is asserted from the client's side, on every kick and again on a room-wide drain, which is where delivery is real: the server can order the farewell but cannot watch it land.

## A kick is not a netdrop

The same guest leaving two ways, told apart by one fact the socket cannot supply:

| how they left | seat | rejoin |
|---|---|---|
| socket cut | **held**, with grace | allowed |
| removed by the host | **cleared** | refused |

The parting reason lives in the `Host`, not in any transport. The host initiated every non-drop parting, so it already knows why; a `Left` with no pending reason *is* a netdrop. The first build put this in its hand-written transport because that was the easy place, and it is the wrong one: the transport never interprets a disconnect, and the rewrite onto the shipped transport confirms the division costs nothing.

## AFK is a policy on the session's reading

`agent_idle_for` counts from the last **data** frame, and probes never move it: the control plane answers a `Ping` invisibly, so this reading has to be the session's or it cannot exist. The guest in the test answers every probe for the whole timeout, so the link is alive and measured while the seat is silent, and the removal still fires. Talking across twice the timeout does not. The steward that applies the number is the example's own 200ms loop, which is the division the doc wants: no timers in the session.

## The griefer floods

**Two verdicts, and the party writes both numbers.** The session holds a `Rate` of `FLOOD_OPS` a second with a burst of the same, so a guest over it is refused *at the door*, on its own connection task, before the queue everybody shares. That costs the flooder its own frames and nobody else theirs, and it is what `agent_inbound(&key).shed` counts. The removal is the escalation: past `FLOOD_TOLERANCE` refused frames the steward reads the shedding as a decision rather than a clumsy client and removes the guest with `flooding` as its farewell. A bystander keeps its seat and keeps receiving the table throughout.

**The downgrade this example carried is repaid.** The 252-line hand-written transport shed a flooder's excess before `forward_incoming` and survived a flood without ejecting anyone; the shipped transport had no seam to do that on, so the flood reached the shared controller queue until the steward removed the sender, and removal was the only verdict the party could express. `plaza_session::gate` is that seam, and the test that was impossible then is `a_clumsy_burst_costs_its_own_frames_and_keeps_its_seat`. The observation-seam finding is now one case rather than two: door_policy still needs to *judge* between socket and controller, and this no longer needs anything.

## Drain finishes the story

`disconnect_all` with the same farewell for everyone: told, then closed, the same flush semantic as the kick applied room-wide. That is a graceful server restart in one call.

## The wire question, still answered the same way

The close reason is an `Op`, not a `Kind`. The library agrees: `close_connection` carries pre-encoded application bytes and the close frame stays a transport event, with the farewell riding the ops path in front of it.
