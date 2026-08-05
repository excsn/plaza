# 41. Rooms, lobbies, and travel

The question this chapter answers: how do many matches share one server, and how do players move between them without their stuff falling off the cart?

## A room is a controller; a lobby is a directory with opinions

Plaza's answer to "many matches" is unglamorous and load-bearing: a room is just [chapter 01](01-one-loop-one-truth.md)'s loop again, its own session and its own controller, spawned on demand. The lobby crate manages the directory: create, list, join, reap. The seam between them is the part worth studying, because it is the crate's main design argument: the lobby holds rooms only through a handle that names *neither the room's op type nor its state type*, the two things a room in another process could not supply. Your application spawns the room (implementing `RoomFactory`), keeps its own map of room id to command channel for speaking the room's vocabulary, and hands the lobby the anonymous handle. The lobby talks to rooms exclusively through the generic controller commands, which is why nothing breaks when a room someday lives elsewhere.

If a channel, a document, or a workspace sounds like your version of this, it is; nothing in the crate knows the room contains a game.

## What the lobby decides, and what it refuses to

The crate's mechanism-versus-policy ledger, in the same spirit as [chapter 40](40-the-right-to-say-no.md): capacity is checked at the lobby and re-checked by the room, because the two checks are not atomic and the room's is the real one. Passwords hash however you say (the verifier is swappable; the default is a plain compare and the docs say so). Reaping polls; *when* to reap is your timer. And placement tickets are the crate's sharpest self-description: this is placement, not authentication. Plaza has no authentication story for tickets to be consistent with, so the crate provides the bookkeeping and not the secret.

## Admission by measurement

The lobby is where latency admission earns its keep, on an argument [lobby_world](../../examples/lobby_world/) states cleanly: a room can only say yes or no, a lobby can say *where*. A room that refuses a 190ms player has produced a sad player; a lobby that routes them to the 200ms-budget room has produced a match. The number it routes on must be one the *server* measured on its own socket ([chapter 31](31-faking-a-bad-network.md)'s spoof-proofing), and the refusal carries both numbers, measured and allowed, so a client can be told something actionable instead of "no". The three layers stay separate on purpose: measuring needs the socket, deciding needs the rule, routing needs the set of rooms, and collapsing any two puts a number where nothing can check it.

## Travel with baggage

A wallet cannot live on the `Agent`, which is identity only, and cannot live in a room's state, which is what leaving destroys. So it lives in a registry the lobby and every room share, keyed by the lobby-issued id, and survives exactly the trips it should: room to room yes, leaving the world no. The general rule: anything that must survive travel belongs to a scope that outlives the trip.

The subtle travel bug is reservation withdrawal, and it earns italics in the crate docs: a room hop reserves the new seat *then* closes the old socket, so the departure event arrives after the promise was made, and an arena that inferred "cancel my reservation" from a closing socket would cancel the very hop in progress. Cancellation must be the lobby's word, never the transport's; this is [chapter 12](12-players-come-and-go.md)'s "the transport never interprets a disconnect" biting at lobby scale.

## Rooms die politely

An idle room's teardown reuses [chapter 40](40-the-right-to-say-no.md)'s drain wholesale: occupants hear a farewell op, then their sockets close, then the room's controller is told to shut down, and the reaper collects the finished handle on a later pass. A room ending and a guest being removed are the same mechanism at different scopes, and nobody's last experience of your server is a silent EOF.

## Ripping it apart

The in-memory lobby manager is the prescription; the factory trait, the ticket registry, the reservations, and the match queue are the blocks, each holding no timers and spawning nothing, driven from your own logic. A matchmaking service of your own replaces the manager and keeps every block; a remote-process room replaces the handle implementation and keeps the seam.

## The lab

[lobby_world](../../examples/lobby_world/): four browser tabs, each assigned a different simulated link, so the room lists genuinely differ per tab; create a room, quick-match into one with bot seats, watch your wallet follow you between arenas, and leave a dynamic room idle to see the reaper drain it politely. Then [horde_playground](../../examples/horde_playground/) with `--rooms` for placement at scale.
