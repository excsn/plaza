# 12. Players come and go

The question this chapter answers: what happens when someone drops mid-match, comes back, or never comes back, and who sits in the empty chair meanwhile?

## One ordered stream of arrivals and departures

Presence in plaza is a single stream of `Joined` and `Left` events, and the singleness is the point: separate join and leave channels would let a leave overtake the join that preceded it under load, and a client that drops and instantly reconnects must never have its departure applied after its return. Your logic receives these as `AgentJoined` and `AgentLeft` inputs like any other event, in the order the transport saw them.

Each event also carries the connection it was about, because an agent may hold several connections at once (a reconnect that overlaps the dying socket, a second device), and acting on "this player" sometimes means acting on "that specific socket". [Chapter 40](40-the-right-to-say-no.md) leans on this hard.

## A drop is not a departure, unless you say so

The transport reports one fact: the socket closed. Whether that means "gone forever" or "back in ten seconds" is an interpretation, and plaza's position, learned the hard way in the lobby, is that **the transport never interprets a disconnect**. Your logic decides, and the block for deciding is [`ReconnectTracker`](../../core/API_REFERENCE.md): call `on_disconnect` from `AgentLeft`, `on_reconnect` from `AgentJoined` (it answers whether this is a genuine return), and `expired(now)` from your tick to learn who ran out of grace, so the consequence stays yours. It holds no timers and spawns nothing; it is a memory, not a machine.

The one requirement it places on you is upstream: a returning player must arrive with the *same* agent ID, which means deriving IDs from something durable (a token, a ticket) rather than minting one per connection. Plaza will faithfully treat whatever you mint as whoever you say it is.

## Seats, and the bug that made them a block

Games with bounded seats need a map from "who" to "which chair", and [`SeatTable`](../../server_utils/API_REFERENCE.md) exists because of a specific bug worth telling: a rejoining player was seated as if fresh in a warm arena, inheriting a stale delta baseline, and the symptom looked exactly like packet loss. It was a seat that remembered. So `seat()` refuses to answer with a bare index: it returns `Seating::Fresh` (per-seat state belongs to a previous occupant, reset it), `Seating::Existing` (a rejoin, resetting would destroy live state), or `Seating::Full` (a real outcome, not an error). Collapsing fresh and existing into `Some(index)` is precisely the bug the enum makes unwritable.

Whether a *kicked* player's seat survives is a different question from a dropped player's, and it belongs to governance: a drop holds the seat warm, a removal clears it, and only the application knows which happened because the application ordered the removal. [Chapter 40](40-the-right-to-say-no.md) finishes that story.

## Bots keep the room warm

An empty chair is a worse experience than a mediocre opponent, so several examples fill vacant seats with bots after a grace period, and two details make the pattern respectable rather than a hack. First, seat assignment is re-decided every tick with a simple rule: a person outranks a bot, so a joining human displaces a bot seamlessly and a leaving human is covered by one. Second, as [chapter 01](01-one-loop-one-truth.md) insisted, the bots submit ops through the same door and read the same filtered views as humans, so the game they keep warm is the real game.

## Presence is also an app feature

Strip the game away and this chapter is "who is online and are they active", which is a product feature in every collaborative tool. [typing_indicator](../../examples/typing_indicator/) is that feature as a micro-app: keystrokes reschedule a game-time timeout that flips a user back to idle, with time advanced virtually so the demo never waits out a real timeout. The same shape, one timeout reset by activity, is an AFK rule when a game wears it.

## Ripping it apart

Everything here is optional and independent: presence events are raw material, and the tracker, the seat table, and any bot policy are separate blocks you drive from your own logic. If your app has no seats, use neither; if your grace rule is exotic (grace scaled by rank, say), the tracker's expiry hands you the who and keeps its hands off the what-now.

## The lab

[pong](../../examples/pong/): open two tabs, close one mid-rally, and watch a bot take the paddle; rejoin and take it back. Then [card_table](../../examples/card_table/) for seats with hidden state attached, where fresh-versus-existing visibly matters. When you reach [chapter 41](41-rooms-lobbies-and-travel.md), lobby_world revisits presence at lobby scale, including why seat reservations must be withdrawn by the lobby's word and never inferred from a closing socket.
