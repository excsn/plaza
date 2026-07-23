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
  session.handle_connection(&req, stream, Agent::new_human(id, "player"))
}
```

`handle_connection` completes the handshake, registers the client, and runs the pump; it deregisters when the socket closes. That route is the whole integration.

## TCP

```rust,ignore
let session = TcpPlazaSession::<Op, PlayerId, Snapshot>::bind(
  "127.0.0.1:9000",
  Arc::new(|peer| Agent::new_human(id_for(peer), "player")),
).await?;
```

Binding happens before the accept loop starts, so a port already in use surfaces as an error rather than killing a detached task.

## Wire format

Everything is encoded through `WireCodec`. `JsonCodec` is the default: readable from a browser console or `websocat`. Supply your own for MessagePack or bincode:

```rust,ignore
let session = ActixWsPlazaSession::with_codec(MyMsgPackCodec);
```

## How it is put together

Both transports wrap one `TransportSession` and share everything that is not socket I/O: the connection registry, message targeting, serialization, and the task that turns raw bytes into typed messages. The per-protocol modules are just pumps, which is why adding a third is small: see the end of the API reference.

Outbound sends use `try_send`, so a client that has stopped reading is skipped rather than stalling the controller for everyone else. Inbound traffic is awaited, so a busy controller applies backpressure instead of discarding ops.

## Status

Experimental. The API changes.
