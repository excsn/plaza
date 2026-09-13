# 12. Players come and go

This chapter covers what happens when someone drops mid-match, comes back or never comes back, who takes the empty seat meanwhile and what happens when the match itself ends.

## One ordered stream of arrivals and departures

Presence in plaza is a single stream of `Joined` and `Left` events, because separate join and leave channels would let a leave overtake the join that preceded it under load. A client that drops and instantly reconnects must never have its departure applied after its return. Your logic receives these as `AgentJoined` and `AgentLeft` inputs like any other event, in the order the transport saw them.

Each event also carries the connection it was about, because an agent may hold several connections at once (a reconnect that overlaps the dying socket, a second device) and acting on "this player" sometimes means acting on "that specific socket". [Chapter 40](40-the-right-to-say-no.md) relies on this heavily.

## Drops and departures

The transport reports one fact: the socket closed. It never decides whether that means "gone forever" or "back in ten seconds"; the lobby learned why the hard way. Your logic decides, using [`ReconnectTracker`](../../core/API_REFERENCE.md): call `on_disconnect` from `AgentLeft`, `on_reconnect` from `AgentJoined` (it answers whether this is a genuine return) and `expired(now)` from your tick to learn who ran out of grace, then decide what happens to them. It holds no timers and spawns nothing, only a record of who left and when.

It requires one thing of you: a returning player must arrive with the *same* agent ID, which means deriving IDs from something durable (a token, a ticket) rather than minting one per connection. Plaza treats two connections as the same player exactly when they carry the same ID.

## Seats

Games with a fixed number of seats need a map from player to seat. [`SeatTable`](../../server_utils/API_REFERENCE.md) exists because of a specific bug: a rejoining player was seated as if fresh in an arena already in progress, inheriting a stale delta baseline. It looked exactly like packet loss, but the cause was a seat remembering its previous occupant. So `seat()` does not return a bare index: it returns `Seating::Fresh` (per-seat state belongs to a previous occupant, reset it), `Seating::Existing` (a rejoin, resetting would destroy live state) or `Seating::Full` (a normal outcome rather than an error). Returning `Some(index)` for both fresh and existing would allow the original bug, so the enum keeps them apart.

Whether a *kicked* player's seat is kept is a separate question from a dropped player's and it belongs to governance: a drop holds the seat, a removal clears it and only the application knows which happened because the application ordered the removal. [Chapter 40](40-the-right-to-say-no.md) covers the rest.

## Bots in empty seats

An empty seat is worse than a mediocre opponent, so several examples fill vacant seats with bots after a grace period. Two details make this work. First, seat assignment is re-decided every tick with a simple rule: a person outranks a bot, so a joining human replaces a bot and a bot covers for a leaving human. Second, as [chapter 01](01-one-loop-one-truth.md) explained, bots submit ops the same way humans do and read the same filtered views, so they play the real game.

## When the match ends

Everything above is about one player leaving. A match ending is the same question at a larger scope and it is easy to skip: a match that reaches its final state still needs a way to continue. A table that reaches its last round and sits in `Finished` or a survival run that reaches `Lost` and stays there leaves everyone looking at a board that will never change again. The only way out is a reload. Three examples in this repository did exactly that until recently and none of them looked broken, because a game stuck in its final state looks the same as a working game that has gone quiet.

The fix is about fifteen lines and reuses machinery the game already has:

**Schedule the intermission where you schedule everything else.** [card_table](../../examples/card_table/) and [parlour_game](../../examples/parlour_game/) put a `Rematch` event on the same `TickEventScheduler` their turn timeout already runs on, so a match ending needs no new timer, no task and no clock read. The standings stay up during the intermission so players have time to read the result.

**Carry the epoch.** The scheduled event takes an [`Epoch`](../../core/API_REFERENCE.md) at the moment it is armed, exactly as the turn timeout does: an opaque token naming one occupancy of a phase, which the phase bumps whenever it changes. If a player arrives during the intermission and fills the table, the deal moves the phase and the pending rematch no longer matches, so it drops itself. Nothing has to remember to cancel it, which matters because a cancellation done by hand is easy to forget.

**Decide what is kept.** A rematch keeps the roster and zeroes the scores; a new roster drops them. Those are `reset_all_scores` and `clear_all_scores` and the per-player pair is `reset_player_score` against `forget_player`. Using the wrong one leaves a leaderboard full of zeroes belonging to people who left, a slow leak that only shows up in a room that runs for hours.

**Do not deal to an empty table.** Both card examples skip the deal when the seats emptied during the intermission and leave it to the next arrival, because no arriving player can join a hand dealt to nobody.

**Under lockstep, the clients have to agree on the restart.** [seed_defense](../../examples/seed_defense/) cannot just reset its own field: every client is simulating too and a server that quietly starts over has diverged from all of them. Instead it sends the fresh field as an ordinary `Snapshot`, which already means "stop computing and adopt this", so a new run needs no new op and no new agreement rule. The tick keeps running throughout, because it is the *session's* clock and every scheduled build and announced wave is keyed to it; restarting the clock would invalidate all of them.

## Presence in apps

Without the game, this chapter is about who is online and whether they are active, which every collaborative tool shows. [typing_indicator](../../examples/typing_indicator/) is that feature as a small app: keystrokes reschedule a game-time timeout that flips a user back to idle, with time advanced virtually so the demo never waits out a real timeout. In a game, the same pattern of one timeout reset by activity is an AFK rule.

## Replacing it

Everything here is optional and independent: presence events are the input and the tracker, the seat table and any bot policy are separate blocks you drive from your own logic. If your app has no seats, use neither the seat table nor bots; if your grace rule is unusual (grace scaled by rank, say), the tracker's expiry tells you who ran out and leaves what happens next to you.

## The lab

[pong](../../examples/pong/): open two tabs, close one mid-rally and watch a bot take the paddle; rejoin and take it back, then let someone reach the winning score and watch the board reset by itself. Then [card_table](../../examples/card_table/) for seats with hidden state attached, where fresh versus existing visibly matters and a finished match deals another. When you reach [chapter 41](41-rooms-lobbies-and-travel.md), lobby_world covers presence at lobby scale, including why seat reservations must be withdrawn only when the lobby says so and never inferred from a closing socket.
