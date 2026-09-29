# 41. Rooms, lobbies and travel

This chapter covers how many matches share one server and how players move between them without losing what they carry.

## Rooms and the lobby

A room is [chapter 01](01-one-loop-one-truth.md)'s loop again, with its own session and its own controller, spawned on demand. The lobby crate manages the directory: create, list, join and reap. The seam between them is the crate's main design decision: the lobby holds rooms only through a handle that names *neither the room's op type nor its state type*, the two things a room in another process could not supply. Your application spawns the room (implementing `RoomFactory`), keeps its own map of room id to command channel for sending the room's own ops and hands the lobby the anonymous handle. The lobby talks to rooms only through the generic controller commands, so a room could later run in another process without breaking the lobby.

A room works just as well as a channel, a document or a workspace; nothing in the crate knows the room contains a game.

## What the lobby decides

The crate splits mechanism from policy the same way as [chapter 40](40-the-right-to-say-no.md). Capacity is checked at the lobby and re-checked by the room, because the two checks are not atomic and the room's is the one that counts. Passwords hash however you say (the verifier is swappable; the default is a plain compare and the docs say so). Reaping polls; *when* to reap is up to your timer. Placement tickets handle placement only and do no authentication: `issue` mints a counter, which stops a client naming another player but is not a secret. Plaza verifies no credential itself (a `ConnectionAdmitter` decides what one proves, [chapter 40](40-the-right-to-say-no.md)), so the crate provides the bookkeeping and leaves the secret to you: mint a signed, expiring token and record it with `issue_with`.

Two details of the ticket are easy to get wrong. **The room is checked before the ticket is spent**, because spending first and comparing afterwards burns a ticket the room had no claim on. Since `issue` mints a counter, that would let anyone destroy anyone else's placement by presenting a guessed ticket at the wrong room. **The ticket's window has to be shorter than the reservation's window**, because redemption is two steps in two places: the route spends the ticket, then the session comes up, then the room's logic consumes the reservation. Equal windows look correct but strand a client that connected at the edge of the window, holding a spent ticket and seated as a spectator.

## Two shapes of lobby

[lobby_world](../../examples/lobby_world/) places players into **standing** rooms: arenas exist and quick-match finds one with a free seat and the right latency budget. [parlour_game](../../examples/parlour_game/) **creates a room per match**: players pair, a table is spawned for exactly them and it closes when that group leaves. Both go through the same seam, but the second is what most matchmade games want and it raises three issues the first never does.

The room lasts per *group* rather than per hand. A settled match deals another after an intermission, because sending three people who want to keep playing back through the queue is worse than keeping the room they are already in. The room still closes when they leave and the reaper collects it, which is all "per match" was meant to guarantee.

Reservations end with the room, so an abandoned placement costs nothing and the reservation window only matters for standing rooms. Room lifetime becomes the reaper's job rather than a capacity question. The client must also **hold its lobby socket open until it is seated at the table**: closing it on `Placed`, the obvious thing to do once you have an endpoint, makes the lobby emit `AgentLeft`, which withdraws the reservation it just issued. The player then arrives as a spectator. The two sockets have separate lifetimes and the first has to stay open until the second is seated. Single-socket tests cannot see this; only a test that drives both sockets does.

## Admission by measurement

Latency admission is most useful in the lobby. As [lobby_world](../../examples/lobby_world/) explains, a room can only accept or refuse a player, while a lobby can choose *where* to put them. A room that refuses a 190ms player just turns them away; a lobby can route them to the 200ms-budget room instead. The number it routes on must be one the *server* measured on its own socket ([chapter 31](31-faking-a-bad-network.md) explains why a client's report cannot be trusted) and the refusal carries both numbers, measured and allowed, so a client can be told something actionable instead of just "no". Measuring, deciding and routing stay in separate layers on purpose: measuring needs the socket, deciding needs the rule and routing needs the set of rooms. Combining any two puts a number where nothing can check it.

## Carrying state between rooms

A wallet cannot live on the `Agent`, which is identity only. It cannot live in a room's state either, since leaving the room destroys that. So it lives in a registry the lobby and every room share, keyed by the lobby-issued id. It survives moving from room to room and is dropped when the player leaves the world. In general, keep anything that must survive a move in a scope that lasts longer than the move.

The hard travel bug is reservation withdrawal, which the crate docs put in italics: a room hop reserves the new seat *then* closes the old socket, so the departure event arrives after the reservation was made and an arena that took a closing socket to mean "cancel my reservation" would cancel the hop in progress. Only the lobby may cancel a reservation, never the transport. This is [chapter 12](12-players-come-and-go.md)'s rule that the transport never interprets a disconnect, applied at lobby scale.

## Closing a room

An idle room's teardown reuses [chapter 40](40-the-right-to-say-no.md)'s drain: occupants are told why (lobby_world sends a `Closed` op, then `disconnect_all` writes a `Goodbye` whose detail repeats the reason), then their sockets close, then the room's controller is told to shut down and the reaper collects the finished handle on a later pass. Closing a room uses the same mechanism as removing a guest, so no player's connection ends in a silent EOF.

## Replacing it

The in-memory lobby manager is the prescription; the factory trait, the ticket store, the reservations and the match queue are the blocks, each holding no timers and spawning nothing, driven from your own logic. A matchmaking service of your own replaces the manager and keeps every block; a remote-process room replaces the handle implementation and keeps the seam.

`TicketStore` is the most complete example of this, because it already has the seam a remote room needs. `MapTicketRegistry` holds a `HashMap` and sweeps expiry from `issue`; `CachedTicketRegistry`, behind the `cache` feature, hands that job to `fibre_cache`'s janitor and its shards. Neither works across processes, because another process does not have the map. That is deliberate: the third implementation is yours, verifying a signed token and building the ticket from its claims while storing nothing at all. Redemption becomes a verification instead of a lookup and the route above it does not change.

## The lab

[lobby_world](../../examples/lobby_world/): four browser tabs, each assigned a different simulated link, so the room lists differ per tab; create a room, quick-match into one with bot seats, watch your wallet follow you between arenas and leave a dynamic room idle to see the reaper drain it. Then [parlour_game](../../examples/parlour_game/) for the room-per-match shape, where the lobby runs JSON and each table runs compact MessagePack on the same server and whose [Flutter client](../../flutter/parlour_client/) plays a match to completion over the two sockets. Then [horde_playground](../../examples/horde_playground/) with `--rooms` for placement at scale.
