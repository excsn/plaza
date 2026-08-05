# 00. What plaza is made of

The question this chapter answers: why is plaza shaped the way it is, and what does that shape promise you?

## Blocks, with prescriptions on top

Plaza is two layers, and the layering is the whole design.

The bottom layer is **blocks**: small, single-purpose pieces that each solve one problem completely and know nothing about each other. A seat table. A delta baseline. An RTT estimator. A spatial grid. A connection close. Each block is a plain type you own and drive; almost none of them spawn tasks, hold timers, or read the clock behind your back. Time is a parameter, not an ambient authority.

The top layer is **prescriptions**: assembled answers for the common cases, built from the blocks using only their public surfaces. The `StateController` loop is a prescription. `PredictedPlayer` is a prescription. The shipped WebSocket and TCP transports are prescriptions. So are the examples, which are prescriptions with a README explaining themselves.

The promise that makes this more than architecture talk: **any prescription can be ripped apart and rebuilt your way, and you lose nothing by doing it.** A prescription never uses private access to the blocks it assembles, so your reassembly stands on the same floor plaza's does. [Chapter 33](33-bring-your-own-socket.md) is the proof in the hardest case, a whole transport written outside the workspace against the published seam alone, and the crate docs for [`LinkDriver`](../../session/API_REFERENCE.md) state the contract outright: it is a convenience, not a ceiling.

## Extracted, not speculated

Plaza's blocks were not designed in advance and then hoped useful. Nearly every one was extracted from an example that had to hand-write it first, and the module docs carry the incident that forced it: the seat that remembered its previous occupant, the delta stream that never re-mentioned a lost despawn, the deadline that closed a socket mid-farewell. When you read a plaza doc and it tells you a story, that story is the reason the API has the shape it has. If a block seems oddly specific, an example bled for that specificity, and the doc will say where.

The same rule bounds what plaza ships: a piece with one consumer stays in that consumer. Extraction happens when something needs the same shape twice.

## Mechanism below, policy above, no defaults in between

Plaza owns *mechanism*: delivering frames, resolving who is connected, measuring round trips, counting what each connection sends, closing sockets cleanly. Plaza never owns *why*: which duplicate login wins, what a ban list contains, how long AFK is, when a room dies. Where a default would decide policy for everyone, plaza deliberately ships none, and the docs say so at each spot. If you go looking for the "kick idle players after N seconds" option, you will not find N; you will find a reader that tells you how idle each player is and a close that takes your reason with it. [Chapter 40](40-the-right-to-say-no.md) walks the whole surface.

## Not just games

This guide says "player", "match", and "world" because games are the demanding case: they stress every part of this at once. Nothing about plaza is game-specific, and if you are building a collaborative app, most of this guide describes things you already do under different names:

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

The examples include real apps: [shared_counter](../../examples/shared_counter/) is the hello world, [typing_indicator](../../examples/typing_indicator/) is presence with timeouts, [auction_floor](../../examples/auction_floor/) is contested writes arbitrated fairly. The suggested app-builder reading path is in [the guide's front page](README.md).

## What plaza deliberately does not do

- **Persistence.** Plaza state lives in memory for the lifetime of a controller. Databases, saves, and event logs are yours.
- **Identity and auth.** An `Agent` is an ID and nothing else. Where the ID comes from (a token, a cookie, a counter) is yours, and plaza will faithfully treat whatever you mint as the same returning player, or not, exactly as you derive it.
- **Matchmaking as a service, ban storage, appeal flows.** The lobby crate gives you rooms and placement mechanics; who plays with whom is policy.
- **An opinion about your engine or your renderer.** The client blocks are runtime-free and wasm-safe precisely so they can live inside whatever loop you already have.

## Lineage

Plaza's netcode vocabulary comes from the writing that taught everyone: Gabriel Gambetta's Fast-Paced Multiplayer series and Glenn Fiedler's Gaffer on Games articles. The guide does not re-teach that theory; [chapter 20](20-hiding-the-wire.md) tells you exactly where to read it and then maps each concept to the block that implements it here.

## How to read this guide

Each chapter ends with a lab: a runnable example that makes the chapter's claims observable, and usually falsifiable, with a toggle or a slider. Run the labs. A guide you only read is a guide you will misremember.

When you want the full inventory rather than the story, [the parts bin](90-the-parts-bin.md) lists every block in every crate with one line on when to reach for it.
