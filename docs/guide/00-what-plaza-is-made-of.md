# 00. What plaza is made of

This chapter explains how plaza is structured and what that structure lets you do.

## Blocks and prescriptions

Plaza has two layers.

The bottom layer is **blocks**: small, single-purpose pieces that each solve one problem completely and know nothing about each other, such as a seat table, a delta baseline, an RTT estimator, a spatial grid or a connection close. Each block is a plain type you own and drive; almost none of them spawn tasks, hold timers or read the clock on their own. Time is passed in as a parameter.

The top layer is **prescriptions**: ready-made answers for the common cases, built from the blocks using only their public API. The `StateController` loop, `PredictedPlayer` and the shipped WebSocket and TCP transports are all prescriptions. So are the examples. Most have a README explaining them and the smallest explain themselves in their module docs.

You can take any prescription apart and rebuild it your own way without losing anything. A prescription uses only the public API of the blocks it assembles, so your version has the same access plaza's does. [Chapter 33](33-bring-your-own-socket.md) builds a whole transport outside the workspace using only the published API. The [`LinkDriver`](../../session/API_REFERENCE.md) docs say the same: it is a convenience and does not limit what a transport can do.

## Where the blocks came from

Nearly every block was extracted from an example that had to write it by hand first. The module docs describe the bug that led to each one: the seat that remembered its previous occupant, the delta stream that never re-sent a lost despawn, the deadline that closed a socket mid-farewell. Those stories explain why each API looks the way it does. If a block seems oddly specific, its doc names the example that needed it.

Plaza extracts a piece into a crate once a second example needs it. Until then it stays in the example that wrote it.

## Mechanism and policy

Plaza handles mechanism: delivering frames, tracking who is connected, measuring round trips, counting what each connection sends and closing sockets cleanly. Policy is up to you: which duplicate login wins, what a ban list contains, how long counts as AFK, when a room shuts down. Where a default would pick a policy for everyone, plaza ships no default and the docs say so. For example, there is no "kick idle players after N seconds" option. Instead there is a reader that reports how long each player has been idle and a close call that takes your reason. [Chapter 40](40-the-right-to-say-no.md) covers all of it.

## Apps other than games

This guide says "player", "match" and "world" because games use every part of plaza at once. Nothing in plaza is game-specific. If you are building a collaborative app, most of this guide covers things you already do under other names:

| Your app's word | This guide's word |
|---|---|
| participant, user | player |
| document, board, order book | world state |
| edit, bid, message | op |
| optimistic UI update | client-side prediction |
| the server rejected my edit | reconciliation |
| who is online, who is typing | presence |
| channel, workspace, document room | room |
| moderation, rate limiting, session expiry | governance |
| graceful deploy | drain |

The examples include apps: [shared_counter](../../examples/shared_counter/) is the hello world, [typing_indicator](../../examples/typing_indicator/) is presence with timeouts and [auction_floor](../../examples/auction_floor/) arbitrates contested writes fairly. The guide's [front page](README.md) has a suggested reading order for app builders.

## What plaza does not do

- **Persistence.** Plaza state lives in memory for the lifetime of a controller. Databases, saves and event logs are yours.
- **Identity and auth.** An `Agent` is an ID and nothing else. Where the ID comes from (a token, a cookie, a counter) is up to you. Plaza treats two connections as the same returning player exactly when you give them the same ID.
- **Matchmaking as a service, ban storage, appeal flows.** The lobby crate gives you rooms and placement mechanics; deciding who plays with whom is up to you.
- **An opinion about your engine or your renderer.** The client blocks are runtime-free and wasm-safe so they can run inside whatever loop you already have.

## Further reading

Plaza's netcode vocabulary comes from Gabriel Gambetta's Fast-Paced Multiplayer series and Glenn Fiedler's Gaffer on Games articles. The guide does not repeat that material. [Chapter 20](20-hiding-the-wire.md) says where to read it and maps each concept to the plaza block that implements it.

## How to read this guide

Each chapter ends with a lab: a runnable example, usually with a toggle or a slider, that lets you see the chapter's claims hold or fail. Run the labs.

When you want the full inventory rather than the explanations, [the parts bin](90-the-parts-bin.md) lists every block in every crate with one line on when to use it.
