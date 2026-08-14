# `plaza_session`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Real network transports for [`plaza`](../core/): actix-web WebSockets and length-delimited TCP. Hand one to a `StateController` instead of writing a transport yourself.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.7"
plaza_session = { version = "0.7", default-features = false, features = ["actix_ws", "json"] }
```

| Feature | Default | Gives you |
|---|---|---|
| `actix_ws` | yes | `ActixWsPlazaSession`: WebSockets on actix-web 4 |
| `tcp` | yes | `TcpPlazaSession`: length-delimited TCP |
| `actix_host` | no | `host::Host`: the listen-server HTTP layer, serving a browser client from the same origin as the socket |
| `json` | yes | `JsonCodec`, and the codec a session type falls back to when it names none |
| `msgpack` | no | `MsgPackCodec` and `MsgPackNamedCodec` |

The shared connection manager compiles either way.

Turning off `json` drops `serde_json` from the build, which is why the transport features are worth naming explicitly rather than reaching for `default-features = false` alone. It costs the zero-argument constructors (`new`, `bind`) and the default type parameter, so a session type names its codec: `ActixWsPlazaSession<Op, PlayerId, MsgPackCodec>`. Note that `actix_ws` brings `serde_json` back regardless, since actix-web depends on it; a build that truly excludes it is TCP or a transport of your own.

## What it gives you

| Problem | Piece |
|---|---|
| Serving a browser client over WebSockets | `ActixWsPlazaSession` |
| A native client over a length-delimited byte stream | `TcpPlazaSession` |
| Turning a socket away before anything is allocated or announced | `AgentFactory` returning `Refusal` |
| Knowing how far away a client is, without asking it | `agent_rtt` (the socket) and `agent_link_rtt` (a real frame's journey) |
| Stamping a reply with your simulation clock, so a client can fit the offset | `SessionOptions::clock` |
| Testing at 200ms with 5% loss without leaving your desk | `LinkProfile`, `DirectionProfile`, `Delivery` |
| Sizing every queue and cap from what your game actually does | `Workload` and its seven presets |
| Deciding what a full queue costs: a frame, a connection, or a stall | `Overflow` |
| Ending a session with a reason in your own vocabulary | `close_connection`, `deregister_agent`, `disconnect_all` |
| Bounding a session that must expire | `set_deadline` |
| Knowing who is idle, and who is flooding | `agent_idle_for`, `agent_inbound` |
| Making a flood cost the client that sent it, and nobody else | `SessionOptions::rate_limit_inbound` with a `Rate` (see `gate`) |
| Telling a stale client it is stale | `ProtocolVersion` in a `Hello`, read back with `protocol` |
| Serving a wasm bundle from the same origin as the socket | `host::Host` (feature `actix_host`) |
| The whole simulation stack behind that | `host::SimHost` |
| Putting plaza on QUIC, Steam, or something stranger | `TransportSession` + `LinkDriver` |

## Status

Experimental. The API changes.
