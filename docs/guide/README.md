# The plaza guide

This is the learning layer of plaza's documentation. The [project README](../../README.md) tells you what plaza is in ninety seconds. Each crate's README and API_REFERENCE tell you exactly what to type. This guide is for the part in between: how plaza approaches the problems of multiplayer games and realtime apps, why the pieces are shaped the way they are, and where the seams are when you want to replace one.

Every chapter answers one question, reads in one sitting, and names a runnable example as its lab. The labs are not decoration: when a chapter claims something, the lab is where you watch the claim be true, and most of them let you watch it be false too, with a slider.

## The chapters

**Foundations**, or what you are standing on:

- [00. What plaza is made of](00-what-plaza-is-made-of.md), why it is blocks with prescriptions on top, what it refuses to do, and how to read this guide if you are building an app rather than a game.
- [01. One loop, one truth](01-one-loop-one-truth.md), where your game's state lives and the one place it is allowed to change.
- [02. Choosing your netcode](02-choosing-your-netcode.md), which model fits your game before any mechanics matter.
- [03. A solver in the loop](03-a-solver-in-the-loop.md), what a physics engine costs each netcode family, and why the answer decides its configuration.

**The world and its players**, or what everyone agrees is happening:

- [10. What each player sees](10-what-each-player-sees.md), snapshots, joining late, and secrets that never touch the wire.
- [11. Keeping the pipe small](11-keeping-the-pipe-small.md), relevance, deltas, and aggregation, or how to afford fifty players.
- [12. Players come and go](12-players-come-and-go.md), drops, rejoins, seats kept warm, and bots filling empty chairs.

**The fight with latency**, or why it feels instant anyway:

- [20. Hiding the wire](20-hiding-the-wire.md), prediction and reconciliation for the entity you control.
- [21. Everyone else's ghosts](21-everyone-elses-ghosts.md), interpolation, extrapolation, and shots that land where you aimed.

**Plumbing**, or the bytes and the sockets:

- [30. Bytes on the wire](30-bytes-on-the-wire.md), what a frame is, and how to ship an update without stranding last week's clients.
- [31. Faking a bad network](31-faking-a-bad-network.md), testing at 200ms with 5% loss without leaving your desk.
- [32. Serving your game](32-serving-your-game.md), browsers, desktops, and the friend on your LAN.
- [33. Bring your own socket](33-bring-your-own-socket.md), when your transport is QUIC, Steam, or something stranger.

**Running a service**, or the parts that are not the game:

- [40. The right to say no](40-the-right-to-say-no.md), admission, kicks, bans, timeouts, and graceful goodbyes.
- [41. Rooms, lobbies, and travel](41-rooms-lobbies-and-travel.md), many matches on one server, and players moving between them.

And one appendix:

- [90. The parts bin](90-the-parts-bin.md), every block in every crate, one line each, sorted by the itch it scratches.

## Reading paths

**Building a game, starting fresh:** read in order. The numbering is the dependency order; nothing forward-references.

**Building an app** (collaborative tool, auction, chat, dashboard): read 00 first, it has a vocabulary map written for you. Then 01, 10, 12, 30, 32, 40, 41. Skim the 20s until your users complain about latency; when they do, come back, because your optimistic UI is our client-side prediction and the chapter will read like a description of your own roadmap.

**Already know netcode** and just want to know where plaza puts things: read 00 for the block-and-prescription contract, then go straight to [the parts bin](90-the-parts-bin.md) and follow links downward.

## A note on the word "player"

This guide says "player" where a precise document would say "connected agent", because precision is not the same thing as clarity and this is the clarity layer. Plaza itself does not care whether the agent is a person in a deathmatch or a bidder at an auction, and [chapter 00](00-what-plaza-is-made-of.md) spells out the translation.
