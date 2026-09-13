# 32. Serving your game

This chapter covers how browsers, desktop clients and the friend on your LAN connect to what you built.

## The shipped transports

The session crate ships WebSockets (actix) and length-delimited TCP and both are thin socket pumps over the same `ConnectionManager`, so everything in this guide that says "the session measures X" or "the session closes Y" is true on both. Your `StateLogic` cannot tell them apart from each other or from the in-process session in [chapter 01](01-one-loop-one-truth.md), so you can develop in-process, test over TCP and ship over WebSockets, changing only the line that constructs the session.

WebSockets are the default for browsers. The frame type follows the codec, so JSON goes as text frames a browser or `websocat` can read directly. Joining happens in the transport: your HTTP route decides who this connection is (mint an ID, read a token, check a ticket) and hands the socket over; to refuse, your route answers with an error instead. TCP does the same with an `AgentFactory` at the accept loop, which can also refuse a connection ([chapter 40](40-the-right-to-say-no.md)).

## Hosting the browser page

The `host` module handles the HTTP half of a listen server: it serves the static bundle, puts the WebSocket route on the same origin so the page connects back to whoever served it and prints an address someone else can actually reach. `lan_address` picks the route the kernel would use, since a running server does not tell you which address to send your friend.

It also does cache busting. A browser client is a build product that does not rebuild when the server does, so a cached page from before a wire change loads, appears to run and fails only on the reshaped messages. That looked like a netcode bug and cost two rounds of diagnosis. Cache busting needs two parts at once, a modification stamp on the bundle and `no-cache` on the assets, because a cached page would keep quoting the old stamp and make cache busting look broken. The version handshake from [chapter 30](30-bytes-on-the-wire.md) is the third part; with all three, a stale client is detected and fixed by a reload.

The host does not catch Ctrl-C for you. Signal handling stays with your process, after an incident where a graceful actix shutdown left a game window running that could not be killed.

## Rust clients, including wasm

Browser JS needs no client library, as [chapter 30](30-bytes-on-the-wire.md) explained; the example pages read the wire with `JSON.parse`. Rust clients get `plaza_ws`: one `Socket` trait across desktop (tungstenite on a worker thread), browser wasm (a miniquad plugin) and loopback, designed for a frame loop rather than an async runtime. A non-blocking `poll` drains into a caller-owned buffer, because an `async fn recv()`, the usual Rust API, cannot be used inside a synchronous render loop.

As [chapter 02](02-choosing-your-netcode.md) noted, a listen server's own player connects through a socket pair that serializes and copies bytes exactly as the network would, so the host's player takes the same path as every other client. Loopback has no latency by design and [chapter 31](31-faking-a-bad-network.md) shows how to add it.

The playgrounds' role flags are worth copying: one binary runs as `--role host`, `client`, `observer` or `headless`, so the same build is the server, the player or the spectator depending on how you launch it.

## Sizing the queues

The session's queues and limits are all configurable through `SessionOptions`, with defaults that suit a small room; the docs note that a 16-player room and a 4000-connection relay need different numbers. What a full queue does is a policy you choose (drop, backpressure, disconnect the laggard) and the stats from [chapter 31](31-faking-a-bad-network.md) tell you which is happening. The workload presets derive consistent numbers from a description of your traffic if you would rather not pick five depths by hand.

One limit is not derived and applied for you: `rate_limit_inbound` caps how fast a single connection may send, which [chapter 40](40-the-right-to-say-no.md) covers. `Rate::for_workload` computes a rate from the same description your queue depths came from, but enforcing it is a line you write. Every other derived number here sizes a buffer, where a wrong value costs memory. This one refuses traffic, where a wrong value costs a player their move.

## Replacing it

The actix host is a convenience for the common same-origin setup; any HTTP server that can serve files and upgrade a WebSocket can sit in front of an `ActixWsPlazaSession`. From there, [chapter 33](33-bring-your-own-socket.md) covers writing the transport itself.

## The lab

[pong](../../examples/pong/) is the smallest hosted game: one command, two browser tabs, real sockets at 60Hz. Then [horde_playground](../../examples/horde_playground/) for the full deployment setup: four roles from one binary, a wasm build served with cache busting and a headless mode for CI.
