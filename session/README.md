# `plaza_session`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Real network transports for [`plaza`](../core/): actix-web WebSockets and length-delimited TCP. Hand one to a `StateController` instead of writing a transport yourself.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.1"
plaza_session = { version = "0.1", default-features = false, features = ["actix_ws"] }
```

| Feature | Default | Gives you |
|---|---|---|
| `actix_ws` | yes | `ActixWsPlazaSession`: WebSockets on actix-web 4 |
| `tcp` | yes | `TcpPlazaSession`: length-delimited TCP |
| `actix_host` | no | `host::Host`: the listen-server HTTP layer, serving a browser client from the same origin as the socket |

The shared connection manager and codec compile either way.

## WebSockets

Construct the session, share it with both the controller and your actix `App`, then hand connections over in the route:

```rust,ignore
let session = ActixWsPlazaSession::<Op, PlayerId, Snapshot>::new();

let (tx, controller) = StateControllerBuilder::new(
  Arc::new(MyLogic), session.clone(), Arc::new(MySnapshotter), MyState::default(),
).build();
tokio::spawn(controller.run());

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ActixWsPlazaSession<Op, PlayerId, Snapshot>>>,
) -> Result<HttpResponse, actix_web::Error> {
  let id = Uuid::new_v4();
  session.handle_connection(&req, stream, Agent::new_human(id))
}
```

`handle_connection` completes the handshake, registers the client, and runs the pump; it deregisters when the socket closes. That route is the whole integration.

## TCP

```rust,ignore
let session = TcpPlazaSession::<Op, PlayerId, Snapshot>::bind(
  "127.0.0.1:9000",
  Arc::new(|peer| Agent::new_human(id_for(peer))),
).await?;
```

Binding happens before the accept loop starts, so a port already in use surfaces as an error rather than killing a detached task.

## Measured latency per connection

The WebSocket adapter times its own ping frames, so `session.agent_rtt(&id)` gives you a measured round trip and a sample count with **nothing added to your protocol**. Fast probes for the first second, then upkeep.

Two things worth knowing. The server times its own probe rather than trusting a reported number, which is what makes it usable for anything that gates entry: a client can only make itself look worse. And `min_rtt` is the one to compare against a budget, because jitter only adds delay, so the smallest sample is the honest estimate of the link.

What you do with it is yours. `horde_playground` uses it to refuse connections that cannot meet its input schedule, which it previously discovered by seating them and then silently dropping every input they sent.

## Wire format

Everything is encoded through `WireCodec`. `JsonCodec` is the default: readable from a browser console or `websocat`. Supply your own for MessagePack or bincode:

```rust,ignore
let session = ActixWsPlazaSession::with_codec(MyMsgPackCodec);
```

## Hosting a browser client (feature `actix_host`)

A listen server serves its own client: one process binds a port, serves a wasm or JS bundle from it, and puts the WebSocket route on the same origin, so the page connects back to whoever served it. `Host` owns the parts of that which are the same every time and easy to get subtly wrong. Your routes go in the closure, which is where the WebSocket lives, so none of this knows anything about the state being shared.

```rust,ignore
Host::new("0.0.0.0:8080")
  .serve_dir(Some("static".to_owned()))
  .cache_bust("client.wasm")
  .run(move |cfg| {
    cfg.route("/ws", web::get().to(ws_route));
  })
  .await
```

`serve_dir` preflights the directory (a missing bundle should fail at startup, not per request), `announce(false)` silences the banner, `ws_path` changes what the banner prints. Signals are left to the process: actix catching Ctrl-C for a graceful shutdown while a game window keeps running is why a windowed host could not be killed.

**The cache busting is not optional, and it is the subtle part.** A browser client is a build product that does **not** rebuild when the server does, so a page built before a wire change still loads, still appears to run, and only the messages whose shape changed are rejected. That reads as a netcode bug and is a deployment one. Two halves have to be there together: `cache_bust` stamps the asset's modification time into the URL in a dynamically served `index.html`, read per request so rebuilding the client reaches an already running host without a restart, and static assets are served `no-cache`, which is what makes the stamp effective. A cached page keeps quoting the old stamp, which is the trap that makes cache busting look like it does not work.

The third half is not here, because it belongs on the wire: see [`plaza_wire::build`](../wire/), which derives a protocol version by hashing the sources that define your messages, so a client can announce what it was built against and be told to reload.

`lan_address()` returns a local address somebody else could actually reach, and `init_logging()` turns on a console subscriber once (a convenience for binaries; a library or an application with its own subscriber should not call it).

## How it is put together

Both transports wrap one `TransportSession` and share everything that is not socket I/O: the connection registry, message targeting, serialization, and the task that turns raw bytes into typed messages. The per-protocol modules are just pumps, which is why adding a third is small: see the end of the API reference.

Outbound sends use `try_send`, so a client that has stopped reading is skipped rather than stalling the controller for everyone else. Inbound traffic is awaited, so a busy controller applies backpressure instead of discarding ops.

## Status

Experimental. The API changes.
