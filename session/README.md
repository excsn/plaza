# `plaza_session`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

Real network transports for [`plaza`](../core/): actix-web WebSockets and length-delimited TCP. Hand one to a `StateController` instead of writing a transport yourself.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza = "0.1"
plaza_session = { version = "0.1", default-features = false, features = ["actix_ws", "json"] }
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

## WebSockets

Construct the session, share it with both the controller and your actix `App`, then hand connections over in the route:

```rust,ignore
let session = ActixWsPlazaSession::<Op, PlayerId>::new();

let (tx, controller) = StateControllerBuilder::new(
  Arc::new(MyLogic), session.clone(), Arc::new(MySnapshotter), MyState::default(),
).build();
tokio::spawn(controller.run());

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ActixWsPlazaSession<Op, PlayerId>>>,
) -> Result<HttpResponse, actix_web::Error> {
  let id = Uuid::new_v4();
  session.handle_connection(&req, stream, Agent::new_human(id))
}
```

`handle_connection` completes the handshake, registers the client, and runs the pump; it deregisters when the socket closes. That route is the whole integration.

## TCP

```rust,ignore
let session = TcpPlazaSession::<Op, PlayerId>::bind(
  "127.0.0.1:9000",
  Arc::new(|peer| Ok(Agent::new_human(id_for(peer)))),
).await?;
```

Binding happens before the accept loop starts, so a port already in use surfaces as an error rather than killing a detached task.

The factory can also say no: return `Err(Refusal)` and the socket is turned away before anything is registered or announced, optionally after one pre-encoded farewell frame. Only rules keyed on what a socket shows (a per-address cap) can fire here; a ban keyed on an account has to wait for the op that names it.

## A tokio runtime is required

Every constructor here spawns the task that decodes inbound frames, so it must be called from inside a tokio runtime. `TcpPlazaSession::bind*` is `async` and therefore already is. The ones worth knowing about are the **synchronous** ones, `ActixWsPlazaSession::{new, with_codec, with_protocol, with_options}` and `TransportSession::{new, with_protocol, with_options}`: called outside a runtime they panic, and the panic names tokio rather than plaza. In an actix `main` you are already inside one; anywhere else, construct inside `Runtime::block_on` or from an async fn.

## Measured latency per connection

Two round trips are measured, both by the server, and neither is a number the client reported. Nothing is added to your protocol for either.

`session.agent_rtt(&id)` is the **transport plane**: the WebSocket adapter timing its own ping frames, underneath everything this crate does. `session.agent_link_rtt(&id)` is the **plaza plane**: a `Kind::Ping` frame going out and its `Pong` coming back, so what it measures is everything a real message goes through, impairment included. Both start with fast probes for the first second, then settle into upkeep.

The gap between them is what plaza and the configured link cost this connection, which is the number worth having while debugging a slow client. On TCP, which has no ping frame of its own, the link plane is the only round trip there is; before it existed, `agent_rtt` was permanently `None` there.

`min_rtt` and `min_link_rtt` are the ones to compare against a budget, because jitter only adds delay, so the smallest sample is the honest estimate of the link.

What you do with it is yours. `horde_playground` uses it to refuse connections that cannot meet its input schedule, which it previously discovered by seating them and then silently dropping every input they sent.

### Answering a probe with your clock

A `Pong` carries the responder's clock, which is what lets a client estimate the offset between the two timelines rather than only the distance between them. Install one and every probe this session answers is stamped with it:

```rust,ignore
let sim_clock = Arc::new(AtomicU64::new(0));
let session = ActixWsPlazaSession::with_options(
  MsgPackCodec,
  SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
    let sim_clock = sim_clock.clone();
    move || sim_clock.load(Ordering::Relaxed)
  }),
);
```

The closure runs on a connection task, so an authoritative clock that lives on the simulation loop is **published** rather than borrowed: store the tick into an `AtomicU64` and close over it, which is what `horde_playground` does. The unit is yours and this crate never reads it as a quantity. Without a clock, `Pong.responder` is `None` and a client can still measure its round trip.

### Sizing the queues, and what a connection may ask for

Every buffer a session owns and every cap it enforces has a default, and every one of them is a guess about a server this crate has never seen. A 16-player room and a 4000-connection relay do not want the same numbers, so nothing here is prescribed: name what your game does and the depths follow, or set any of them yourself.

The shortest version is to name the workload:

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).workload(&Workload::action())
```

which derives every depth and cap from a handful of answers, and what those answers are is worth knowing because you will want to change them. `Workload` carries the tick rate, peak players, ops per player per tick, how long a client may stall, the join burst, the tick budget, the largest frame, a memory budget, and whether a lost op or a wait is the worse failure. A preset is just a `Workload` with those filled in, so `Workload::action().peak_players(200)` is not an escape hatch out of a preset, it is the same mechanism.

What the seven derive today:

| preset | inbound | decoded | presence | outbound |
|---|---|---|---|---|
| `action` | 48 | 48 | 16 | 4 |
| `horde` | 192 | 192 | 64 | 47 |
| `turn_based` | 16 | 16 | 16 | 4 |
| `social_relay` | 4096 | 4096 | 512 | 4 |
| `spectator` | 8 | 8 | 512 | 4 |
| `lobby` | 8 | 8 | 4096 | 4 |
| `local` | 32 | 32 | 32 | 4 |

The striking column is `outbound`, and it is measured rather than chosen. A stalled client's socket already holds roughly 540 KiB before this crate's queue is what fills, which is over a thousand frames at 512 bytes and fourteen at 40 KiB. So for a small-payload game the outbound queue is nearly a no-op and the kernel is doing the work; it only becomes the binding term once frames are large, which is why `horde` is the one preset that needs a real one. That 540 KiB is this machine's, though, so `Workload::socket_buffer_bytes` is a field: a Linux box behind a real NIC tunes its sockets differently and should say so.

Underneath all of that, every field is still yours:

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .outbound_capacity(512)      // frames one slow client may fall behind by
  .presence_capacity(1024)     // joins and leaves waiting for the controller
  .max_frame_bytes(256 * 1024) // largest inbound frame this build will accept
```

`SessionOptions::queues` holds the depths (`inbound`, `decoded`, `presence`, `outbound`, `conditioner`) and `limits` holds the caps (`max_frame_bytes` for TCP, `max_message_bytes` for WebSocket, `probe_slots`). Set a group wholesale with `.queues(..)` / `.limits(..)`, or one field at a time as above. `ConnectionManager::queues()` and `limits()` read them back, which is where a third-party transport picks them up.

### Measuring, or not

The link plane costs a `Kind::Ping` each way on a schedule, per connection. A build that never reads an RTT should not pay for one:

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).without_probes()
```

An inbound `Ping` is still answered, because refusing would break a peer measuring its own side; what stops is this session originating them. `agent_link_rtt` and `link_rtt` then stay `None`.

The schedule is yours too. `Probes` carries `enabled`, `slots`, `fast_pings`, `fast_interval` and `idle_interval`, set wholesale with `.probes(..)` or through `.probe_schedule(fast_pings, fast, idle)` and `.probe_slots(n)`. The defaults spend eight probes at 125ms before settling to one every five seconds, which puts several samples inside the first second and then keeps an eye on a link that changes later. A LAN server and a global one want different numbers for the same reason the queues do.

`slots` is not one. A probe is answered a round trip after it goes out and the fast phase sends another every 125ms, so on any link slower than that the reply lands after its successor was sent; tracking one at a time discarded every such sample and left the link unmeasured at precisely the latencies worth measuring.

### What a full queue does

Depth is half the decision; the other half is what happens when the depth runs out, and the right answer differs per queue because the producers differ.

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .disconnect_slow_clients()   // outbound: end the connection rather than the frame
  .backpressure_inbound()      // inbound: stop reading that socket while the controller is behind
  .backpressure_presence()     // presence: hold a connection at registration rather than lose a join
```

Or all three at once with `.overflow(Overflow::drop_everywhere())`, which is what ships, or `Overflow::block_where_possible()`.

The three are separate types rather than one shared enum, because the arms that make sense differ. Only a client can be disconnected, so `Disconnect` exists on `outbound` alone: an inbound queue fills because the *controller* is behind, which names nothing a particular client did, and a lost presence event is already a bookkeeping failure that disconnecting would answer by inventing a second one. And there is deliberately no "block everywhere": the outbound fan-out runs under the connection registry's read guard and has no arm that waits, so a uniform setter would quietly mean something different there. Making that a compile error rather than a doc paragraph is the point of the three types.

Two of these have a failure mode worth knowing before you choose them. `backpressure_presence` wedges every connection at registration if the session starts before its controller does and the presence queue fills, which is the exact case dropping exists for. And `backpressure_inbound` is TCP backpressure on one client, which is the point, but a controller that falls behind applies it to every client at once.

One place deliberately ignores the presence policy: a departure caused by `disconnect_slow_clients` is announced without waiting, even under `backpressure_presence`. Otherwise a send that disconnects a client would block on the controller hearing about it, and a controller behind on presence is exactly what filled the outbound queue to begin with. Losing one `Left` event is bounded and counted; a stalled fan-out is neither. `LossFree` derives both policies together, so this is the combination it ships with.

Naming a workload picks for you: `LossFree` disconnects and waits, `LatencyFirst` drops everywhere, on the grounds that where ops supersede each other a lost frame costs nothing and where they do not, a client holding a view the server never authored should be told.

Two of these are worth knowing about before you need them. `inbound`, `decoded` and `presence` default to the same number because they used to share a single constructor argument, not because a trickle of joins and every frame from every client are comparable traffic. And `max_frame_bytes` defaults to 8 MiB because that is what `LengthDelimitedCodec` enforces when nobody asks; it is the largest allocation a client can make this server perform, so a build that knows its own frames are small should say so.

## Who is active, and how much they send

Two per-connection readers sit beside the latency family, because the session is the only layer that can keep them honest:

- `idle_for(conn_id)` / `agent_idle_for(&id)`: time since the last **data** frame. Probes do not count, and only the session can promise that: the control plane answers a `Ping` without the application ever seeing it, so an AFK rule written anywhere else either counts probe traffic as presence or never fires. No timer and no timeout ship with it; read it from your own tick and apply your own number.
- `connection_inbound(conn_id)` / `agent_inbound(&id)`: monotonic frame and byte counters per connection. `TransportStats` counts the session as a whole, which can say *that* something floods but never *who*. Windows and thresholds stay yours: diff two readings, or feed a `RateMeter`.

## Impairment belongs to the link

Delay, jitter and loss are properties of a connection, so they are applied where the connection is:

```rust,ignore
session.set_agent_link_profile(&id, LinkProfile::symmetric(DirectionProfile {
  delay: Duration::from_millis(80),
  jitter: Duration::from_millis(20),
  loss: 0.02,
  delivery: Delivery::Reliable,
}));
```

`up` impairs what the client sends, `down` what the server sends; `LinkProfile::symmetric` applies the same each way, so the 80ms above is a 160ms round trip. `set_all_link_profiles` does the same for every live connection, which is what a panel describing one room's conditions wants. A default profile is passthrough and costs nothing: no queue, no allocation, no deadline arithmetic.

### What a loss costs is the link's to say

`loss` is the probability a frame is lost in transit. `delivery` says what that means, and the two answers are different link types rather than two knobs on one.

`Delivery::Reliable` is the default and the truth about both transports here. TCP retransmits, so a lost segment never reaches the application as a missing message: it costs one retransmission timeout, everything queued behind it waits, and what arrives is a latency spike followed by a burst. **Nothing is deleted.** Modelling loss as a deleted frame would describe a link plaza does not have, and an application written against it would carry reconciliation for a case that cannot occur.

`Delivery::Datagram` deletes the frame, which is what a real datagram link does and what the two ends then have to reconcile. Over a WebSocket that is a *simulation* of a transport plaza has yet to grow, and it is worth having deliberately: it is how an application's recovery gets exercised before the channel it was written for exists.

No frame kind is exempt under either model, and none needs to be. Under `Reliable` nothing is lost at all. Under `Datagram` a lost probe costs one sample of the several the session keeps in flight, and a lost `Hello` reads as a peer that declared nothing, which is exactly the case that handshake was built to survive.

**What the link discarded, it reports.** `session.link_dropped()` totals the frames the conditioner threw away, and `agent_link_dropped(&id)` narrows it to one agent. A `Datagram` profile is one of two ways a frame dies; the other is the queue bound, which stands for a socket buffer running out rather than for anything the network did, and refuses only `Ops` frames so a full queue can never wedge a handshake or starve a probe. This is worth reading precisely because an application cannot count it for itself: what the link lost never reaches the application, which is the whole point of losing it.

Two other things it guarantees, each of which an application queue had to remember for itself:

- **Order is preserved.** Release times are made monotone as frames are queued, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall then a burst. That head-of-line blocking is also what makes one retransmission cost more than the frame that paid for it.
- **Everything crosses it.** Including the link-plane probe, which is why `agent_link_rtt` moves when you drag a latency slider and `agent_rtt` does not.

## Ending a session

A departure the server initiates goes through `close_connection`:

```rust,ignore
let farewell = session.encode_message(SessionMessage::system(vec![Op::Kicked { why }]))?;
for conn_id in session.manager().connections_of(&player) {
  session.manager().close_connection(conn_id, Some(farewell.clone()));
}
```

The connection task flushes what was queued, writes the farewell last, and closes the socket; the departure then arrives on the presence stream as an ordinary `Left`, so game logic keeps one disconnect story whether the cable was pulled or the host said go. The farewell is an op of your own vocabulary, not a transport code: "removed by the host" and "away from the table" are application words, and neither transport has (or needs) a close vocabulary of its own.

`deregister` is not a close. It removes the connection from the registry and nothing else; the socket belongs to the connection task, and only an order through `close_connection` reaches it. `PresenceEvent` carries the `conn_id` at join and leave, and `connections_of(&id)` resolves an agent to its live connections, so a rule that decides "this one goes" always has a handle to act on.

Which connection goes is policy, and it stays yours: a duplicate login can refuse the newcomer or kick the older session with the same two calls, and the library ships no default.

`deregister_agent(&id, farewell)` closes every connection an agent holds, and `disconnect_all(farewell)` drains the room through the same path: everyone told, then closed. A drain differs from a kick only in who it names.

`set_deadline(conn_id, after, farewell)` bounds a session instead of ending it now: the connection task enforces the deadline in its own loop and expiry goes through the same flush-then-farewell close. Setting it again replaces it, which is how a renewal extends a session; what stamps, renews, or revokes it is yours.

## Wire format

Everything is encoded through `WireCodec`. `JsonCodec` is the default: readable from a browser console or `websocat`. Supply your own for MessagePack or bincode:

```rust,ignore
let session = ActixWsPlazaSession::with_codec(MyMsgPackCodec);
```

A version declared here is announced to every client as a `Hello` before anything else, so a stale build hears about the skew on connect instead of mis-decoding one variant at a time. It matters most for a client that ships separately from the server: a page can be forced to reload and an installed app cannot.

```rust,ignore
let session = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
```

`examples/lobby_world` does exactly this, deriving `PROTOCOL` in its `build.rs` from the file that defines its ops.

### Who decides what a mismatch means

This layer carries the number and stops. It records what a peer declared, keeps serving it, and lets you read it back with `session.protocol(&id)`. It does not refuse the connection and does not warn, because a version is a build hash: a peer that merely recompiled is indistinguishable from one whose shapes changed, and neither this crate nor a log line can tell them apart.

The decision is the game's, and it is a real one. Refuse the seat, serve a degraded stream, show a banner, or tell the client to reload:

```rust,ignore
if let Some(theirs) = session.protocol(&id) {
  if theirs != ProtocolVersion(PROTOCOL) {
    // Your op, your policy. Six of the examples do exactly this.
    return vec![TargetedOp::new_system_to(id, vec![Op::Outdated { server: PROTOCOL, client: theirs.0 }])];
  }
}
```

That is the layering: the `Hello` is how a version gets across, and an op like `Op::Outdated` is how your game answers. The handshake deliberately cannot answer for you, and an application-level op deliberately does not need to carry the version itself once the handshake does.

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

For a delta-streaming simulation, `SimHost` is the whole stack behind that `Host`: the session with your protocol version and a simulation clock for its pongs, the controller with join snapshots off, a fixed-step driver, and a `/ws` route that numbers its connections. You hand it a codec, a protocol, a state and a closure that builds your `StateLogic` from the wiring (session handle, clock slot, a `link_sink()` for impairment panels). It is a prescription built from the blocks around it, so anything it decides for you (no join snapshot, numbered `u64` agents, `run_fixed`) is unmade by using those blocks directly.

## How it is put together

Both transports wrap one `TransportSession` and share everything that is not socket I/O: the connection registry, message targeting, serialization, and the task that turns raw bytes into typed messages. The per-protocol modules are just pumps, which is why adding a third is small: see the end of the API reference.

By default outbound sends use `try_send`, so a client that has stopped reading loses the frame rather than stalling the controller for everyone else, and inbound ops are dropped rather than blocking a connection task on a controller that is behind. The deserialize bridge between them awaits, so a slow controller backs the pipe up before either of those bites. All three are settable: see the sizing section above.

## Status

Experimental. The API changes.
