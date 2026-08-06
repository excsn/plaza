# table_manners

Moderation as a live tool, built from the library's blocks with no transport of its own.

```sh
cargo test -p plaza_example_table_manners
```

A four-seat party with host tools. Every claim is a count, and the counts are asserted rather than printed, because each one is a property that has to hold every time rather than a number to admire.

This example originally shipped a 252-line hand-written TCP transport, because AFK, flood attribution, the kick and the drain were impossible on the shipped one. Those became library primitives, and the rewrite deleted the transport. What follows is the recipe.

## The blocks, and what each tool sits on

| tool | block it uses | policy that stays here |
|---|---|---|
| kick with a reason | `deregister_agent(&key, farewell)` | who may kick, and what the reason says |
| AFK removal | `agent_idle_for(&key)` | the timeout, and the steward that applies it |
| flood attribution | `agent_inbound(&key)` | the window, the threshold, the removal |
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

`agent_inbound` answers "who", which the session-wide `TransportStats` cannot; the window and the threshold are the host's. The flooder is removed with `flooding` as its farewell, the shared queue drops nothing, and a bystander keeps its seat and keeps receiving the table.

**One honest downgrade from the first build.** The hand-written transport shed the flooder's excess before `forward_incoming`, surviving the flood without ejecting anyone; on the shipped transport the flood reaches the shared controller queue until the steward removes the sender. The isolation assertions still hold at this scale, but shedding-without-ejecting needs to stand between the socket and the controller, and there is still nowhere to stand. That is the observation-seam finding again, now with a second case: door_policy needs to *judge* there, this needs to *shed* there.

## Drain finishes the story

`disconnect_all` with the same farewell for everyone: told, then closed, the same flush semantic as the kick applied room-wide. That is a graceful server restart in one call.

## The wire question, still answered the same way

The close reason is an `Op`, not a `Kind`. The library agrees: `close_connection` carries pre-encoded application bytes and the close frame stays a transport event, with the farewell riding the ops path in front of it.
