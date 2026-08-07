# 41. Rooms, lobbies, and travel

The question this chapter answers: how do many matches share one server, and how do players move between them without their stuff falling off the cart?

## A room is a controller; a lobby is a directory with opinions

Plaza's answer to "many matches" is unglamorous and load-bearing: a room is just [chapter 01](01-one-loop-one-truth.md)'s loop again, its own session and its own controller, spawned on demand. The lobby crate manages the directory: create, list, join, reap. The seam between them is the part worth studying, because it is the crate's main design argument: the lobby holds rooms only through a handle that names *neither the room's op type nor its state type*, the two things a room in another process could not supply. Your application spawns the room (implementing `RoomFactory`), keeps its own map of room id to command channel for speaking the room's vocabulary, and hands the lobby the anonymous handle. The lobby talks to rooms exclusively through the generic controller commands, which is why nothing breaks when a room someday lives elsewhere.

If a channel, a document, or a workspace sounds like your version of this, it is; nothing in the crate knows the room contains a game.

## What the lobby decides, and what it refuses to

The crate's mechanism-versus-policy ledger, in the same spirit as [chapter 40](40-the-right-to-say-no.md): capacity is checked at the lobby and re-checked by the room, because the two checks are not atomic and the room's is the real one. Passwords hash however you say (the verifier is swappable; the default is a plain compare and the docs say so). Reaping polls; *when* to reap is your timer. And placement tickets are the crate's sharpest self-description: this is placement, not authentication. Plaza has no authentication story for tickets to be consistent with, so the crate provides the bookkeeping and not the secret.

Two details of the ticket that are easy to get wrong and are worth stating once. **The room is checked before the ticket is spent**, because spending first and comparing afterwards burns a ticket the room had no claim on, and since `issue` mints a counter that would let anyone destroy anyone else's placement by presenting a guess at the wrong door. And **a window on the ticket has to be shorter than the window on the reservation**, because redemption is two steps in two places: the route spends the ticket, then the session comes up, then the room's logic consumes the reservation. Equal windows look obviously correct and strand a client that dialled at the edge, holding a spent ticket and seated as a spectator.

## Two shapes of lobby, and they are not the same shape

[lobby_world](../../examples/lobby_world/) places players into **standing** rooms: arenas exist, quick-match finds one with a free seat and the right latency budget. [parlour_game](../../examples/parlour_game/) **creates a room per match**: players pair, a table is spawned for exactly them, and it dies with that group rather than outliving it. Both go through the same seam, but the second is what most matchmade games actually want, and three things follow from it that the first never surfaces.

Note the seam is per *group*, not per hand. A settled match deals another after an intermission, because sending three people who want to keep playing back through the queue is a worse answer than keeping the room they are already in. The room still dies when they drift off and the reaper collects it, which is what "per match" was ever protecting.

Reservations die with the room, so an abandoned placement costs nothing and the reservation window only earns its keep for standing rooms. Room lifetime becomes the reaper's problem rather than a capacity question. And the client must **hold its lobby socket open until it is seated at the table**: closing it on `Placed`, the obvious move once you have an endpoint, makes the lobby emit `AgentLeft`, which withdraws the reservation it just issued, and the player arrives as a spectator. Two sockets, separate lifetimes, and the first gates the second. That one is invisible to single-socket tests and was found only by driving both.

## Admission by measurement

The lobby is where latency admission earns its keep, on an argument [lobby_world](../../examples/lobby_world/) states cleanly: a room can only say yes or no, a lobby can say *where*. A room that refuses a 190ms player has produced a sad player; a lobby that routes them to the 200ms-budget room has produced a match. The number it routes on must be one the *server* measured on its own socket ([chapter 31](31-faking-a-bad-network.md)'s spoof-proofing), and the refusal carries both numbers, measured and allowed, so a client can be told something actionable instead of "no". The three layers stay separate on purpose: measuring needs the socket, deciding needs the rule, routing needs the set of rooms, and collapsing any two puts a number where nothing can check it.

## Travel with baggage

A wallet cannot live on the `Agent`, which is identity only, and cannot live in a room's state, which is what leaving destroys. So it lives in a registry the lobby and every room share, keyed by the lobby-issued id, and survives exactly the trips it should: room to room yes, leaving the world no. The general rule: anything that must survive travel belongs to a scope that outlives the trip.

The subtle travel bug is reservation withdrawal, and it earns italics in the crate docs: a room hop reserves the new seat *then* closes the old socket, so the departure event arrives after the promise was made, and an arena that inferred "cancel my reservation" from a closing socket would cancel the very hop in progress. Cancellation must be the lobby's word, never the transport's; this is [chapter 12](12-players-come-and-go.md)'s "the transport never interprets a disconnect" biting at lobby scale.

## Rooms die politely

An idle room's teardown reuses [chapter 40](40-the-right-to-say-no.md)'s drain wholesale: occupants hear a farewell op, then their sockets close, then the room's controller is told to shut down, and the reaper collects the finished handle on a later pass. A room ending and a guest being removed are the same mechanism at different scopes, and nobody's last experience of your server is a silent EOF.

## Ripping it apart

The in-memory lobby manager is the prescription; the factory trait, the ticket store, the reservations, and the match queue are the blocks, each holding no timers and spawning nothing, driven from your own logic. A matchmaking service of your own replaces the manager and keeps every block; a remote-process room replaces the handle implementation and keeps the seam.

`TicketStore` is the most worked example of that promise, because it already has the seam a remote room needs. `MapTicketRegistry` holds a `HashMap` and sweeps expiry from `issue`; `CachedTicketRegistry`, behind the `cache` feature, hands that job to `fibre_cache`'s janitor and its shards. Neither works across processes, because another process does not have the map, and that is the point: the third implementation is yours, verifying a signed token and building the ticket from its claims while storing nothing at all. Redemption stops being a lookup and becomes a verification, and the route above it does not change.

## The lab

[lobby_world](../../examples/lobby_world/): four browser tabs, each assigned a different simulated link, so the room lists genuinely differ per tab; create a room, quick-match into one with bot seats, watch your wallet follow you between arenas, and leave a dynamic room idle to see the reaper drain it politely. Then [parlour_game](../../examples/parlour_game/) for the room-per-match shape, where the lobby runs JSON and each table runs named MessagePack on the same server, and whose [Flutter client](../../flutter/parlour_client/) plays a match to completion over the two sockets. Then [horde_playground](../../examples/horde_playground/) with `--rooms` for placement at scale.
