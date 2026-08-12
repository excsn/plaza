# `plaza_ws`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

One client-side WebSocket interface across a desktop, a browser, and in-process. [`plaza_session`](../session/) covers the server and is tokio/actix by construction, so it cannot help a client and least of all a browser one. This is the other half.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_ws = { version = "0.6", features = ["native", "loopback"] }
```

## What it gives you

| Problem | Piece |
|---|---|
| A WebSocket in a frame loop, with nowhere to put a future | `Socket::poll`, draining into a buffer you own |
| One API across desktop, browser and in-process | `connect`, `connect_boxed`, the `Socket` trait |
| A host that plays, without giving its own player a privileged path | `loopback::pair` |
| Everything a `plaza_session` client would otherwise write by hand | `pump::FramePump` |
| Round trip, server-time estimate and byte counters, already wired | `FramePump::timeline`, its byte counters |
| Trimming a resumed tab's backlog before parsing it | `trim_backlog`, between `drain` and `digest` |
| Telling a stale page it is stale | `Arrival::Mismatch`, `mismatch_message` |
| Testing a client with no network at all | `scripted::ScriptedSocket` |

## Backends

| feature | where | underneath | dependencies |
|---|---|---|---|
| `loopback` | anywhere | in-process channels | none |
| `native` | desktop | `tungstenite` on a worker thread | `tungstenite` |
| `miniquad` | browser, under macroquad | our own JS plugin | none |

`connect` picks whichever real backend this build has for its target, and the choice is never ambiguous: `native` exists only off wasm and `miniquad` only on it, so enabling both (the normal shape for a crate shipping a desktop and a browser client) still leaves exactly one per target. `connect_boxed` is the same choice as a `Box<dyn Socket>`, and it exists in every build: with no backend it reports "no socket backend compiled in" at runtime, because an offline teaching build still has to compile its connect path.

They compose. A listen-server that also plays enables `native` **and** `loopback` and talks to both through the same trait.
