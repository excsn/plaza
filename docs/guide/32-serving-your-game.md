# 32. Serving your game

The question this chapter answers: how do browsers, desktop clients, and the friend on your LAN actually connect to the thing you built?

## Two shipped transports, one session underneath

The session crate ships WebSockets (actix) and length-delimited TCP, and both are thin socket pumps over the same `ConnectionManager`, so everything in this guide that says "the session measures X" or "the session closes Y" is true on both. Your `StateLogic` cannot tell them apart, and the in-process session from [chapter 01](01-one-loop-one-truth.md) completes the set: develop in-process, test over TCP, ship over WebSockets, changing only the line that constructs the session.

WebSockets is the browser-facing default. The frame type follows the codec, so JSON goes as text frames a browser or `websocat` can read raw. Joining is transport-implicit: your HTTP route decides who this connection is (mint an ID, read a token, check a ticket) and hands the socket over; refusing is your route answering with an error instead. TCP mirrors the same shape with an `AgentFactory` at the accept loop, including the right to refuse at the door ([chapter 40](40-the-right-to-say-no.md)).

## Hosting the browser page

The `host` module owns the HTTP half of a listen server: serve the static bundle, put the WebSocket route on the same origin so the page connects back to whoever served it, and print an address someone else can actually reach (`lan_address` picks the route the kernel would use, because "it is running" and "here is what to send your friend" are different pieces of information).

Its one insistent feature is cache busting, and the insistence was bought: a browser client is a build product that does not rebuild when the server does, so a cached page from before a wire change loads, appears to run, and fails only on the reshaped messages. That read as a netcode bug and cost two rounds of diagnosis. The defense needs both halves at once, a modification stamp on the bundle and `no-cache` on the assets, because a cached page would keep quoting the old stamp, which is the trap that makes cache busting look broken. The third half is the version handshake from [chapter 30](30-bytes-on-the-wire.md); together they turn "stale client" from a mystery into a reload.

Signals stay with the process on purpose: the host does not catch Ctrl-C for you, after an incident where a graceful actix shutdown left a game window running that could not be killed.

## Rust clients, including wasm

Browser JS needs no client library, as [chapter 30](30-bytes-on-the-wire.md) explained; the example pages read the wire with `JSON.parse`. Rust clients get `plaza_ws`: one `Socket` trait across desktop (tungstenite on a worker thread), browser wasm (a miniquad plugin), and loopback, shaped for a frame loop rather than an async runtime: a non-blocking `poll` draining into a caller-owned buffer, because an `async fn recv()` is the natural Rust API and unusable inside a synchronous render loop.

The loopback deserves its sentence from [chapter 02](02-choosing-your-netcode.md) again, because it is a hosting decision: a listen server's own player connects through a socket pair that serializes and copies bytes exactly as the network would, so the host is never the one client on a privileged path that can never be wrong. What loopback lacks is latency, deliberately, and [chapter 31](31-faking-a-bad-network.md) is how you add it back on purpose.

The playgrounds' role flags are the pattern worth copying for dev ergonomics: one binary serving as `--role host`, `client`, `observer`, or `headless`, so the same build is the server, the player, and the spectator depending on how you launch it.

## Sizing the queues

The session's queues and limits are all configurable through `SessionOptions`, with defaults that suit a small room and an honest note in the docs that a 16-player room and a 4000-connection relay are not the same number. What a full queue does is policy you choose (drop, backpressure, disconnect the laggard), and the stats from [chapter 31](31-faking-a-bad-network.md) tell you which is happening. The workload presets derive coherent numbers from a description of your traffic when you would rather not pick five depths by hand.

## Ripping it apart

The actix host is a convenience for the common same-origin story; any HTTP server that can serve files and upgrade a WebSocket can stand in front of an `ActixWsPlazaSession`... at which point you are most of the way to [chapter 33](33-bring-your-own-socket.md), where the transport itself becomes yours.

## The lab

[pong](../../examples/pong/) is the smallest hosted game: one command, two browser tabs, real sockets at 60Hz. Then [horde_playground](../../examples/horde_playground/) for the full deployment shape: four roles from one binary, a wasm build served with cache busting, and a headless mode for CI.
