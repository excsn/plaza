# `plaza_lobby`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Rooms on a single server for [`plaza`](../core/): creating them, listing them, authorizing joins, and reaping finished ones. Each room is a `StateController` running as its own task in the same process.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.7"
plaza_lobby = "0.7"
```

## What it gives you

| Problem | Piece |
|---|---|
| Building a room of your own game, which the crate cannot know how to do | `RoomFactory`, `InProcessRoomHandle` |
| Creating, listing, joining and reaping rooms around it | `InMemoryLobbyManager` |
| Client-facing message shapes for all of that | the `payloads` module |
| Gating a room behind a code or a password | `RoomSettings::password_hash` plus a verifier you supply |
| Sending a connection to a room whose schedule it can actually meet | `rooms_playable_at`, `routing::best_for` |
| Pairing players who would rather not choose a room | `MatchQueue` |
| Holding a seat between admission and arrival | `SeatReservations` |
| Resolving a connecting player without trusting a URL | `TicketStore`, `MapTicketRegistry`, `CachedTicketRegistry` |

## Scope

Single server. Rooms are in-process tasks, and nothing here coordinates across machines; that remains an application concern.

## Status

Experimental. The API changes.
