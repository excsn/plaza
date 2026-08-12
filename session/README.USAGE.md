# Usage Guide: plaza_session

How to put a real network transport under a `plaza` `StateController`: standing a session up on WebSockets or TCP, sizing what it holds, measuring each connection, impairing a link, ending a session, hosting a browser client, and writing a transport of your own.

## Table of Contents

*   [Core Concepts](#core-concepts)
*   [Quick Start](#quick-start)
    *   [WebSockets on actix-web](#websockets-on-actix-web)
    *   [Length-Delimited TCP](#length-delimited-tcp)
*   [A Tokio Runtime Is Required](#a-tokio-runtime-is-required)
*   [Choosing a Wire Format](#choosing-a-wire-format)
    *   [Declaring a Protocol Version](#declaring-a-protocol-version)
    *   [Deciding What a Mismatch Means](#deciding-what-a-mismatch-means)
*   [Configuring a Session](#configuring-a-session)
    *   [Naming a Workload](#naming-a-workload)
    *   [Setting Depths and Caps by Hand](#setting-depths-and-caps-by-hand)
    *   [What a Full Queue Does](#what-a-full-queue-does)
*   [Measuring Latency](#measuring-latency)
    *   [The Two Planes](#the-two-planes)
    *   [Answering a Probe With Your Clock](#answering-a-probe-with-your-clock)
    *   [Turning Probes Off](#turning-probes-off)
    *   [Changing the Probe Schedule](#changing-the-probe-schedule)
*   [Watching Connections](#watching-connections)
    *   [Who Is Idle](#who-is-idle)
    *   [Who Is Sending How Much](#who-is-sending-how-much)
*   [Impairing a Link](#impairing-a-link)
    *   [Setting a Profile](#setting-a-profile)
    *   [What a Loss Costs](#what-a-loss-costs)
    *   [What the Link Reports](#what-the-link-reports)
*   [Ending a Session](#ending-a-session)
    *   [Closing One Connection](#closing-one-connection)
    *   [Kicking an Agent, Draining a Room](#kicking-an-agent-draining-a-room)
    *   [Bounding a Session With a Deadline](#bounding-a-session-with-a-deadline)
*   [Hosting a Browser Client](#hosting-a-browser-client)
    *   [Serving the Bundle](#serving-the-bundle)
    *   [Cache Busting](#cache-busting)
    *   [The Whole Simulation Stack](#the-whole-simulation-stack)
*   [Writing Another Transport](#writing-another-transport)
    *   [The Connection Loop](#the-connection-loop)
    *   [Assembling the Pieces Yourself](#assembling-the-pieces-yourself)
*   [Error Handling](#error-handling)

## Core Concepts

*   **`Session`**: the `plaza` trait a `StateController` sends through. This crate implements it over real sockets.
*   **`TransportSession`**: the complete implementation, wrapped by both shipped adapters. Owns the codec and the deserialize bridge.
*   **`ConnectionManager`**: the connection registry plus the notification channels the controller consumes. Everything that is not socket I/O.
*   **`ConnectionId`**: one socket. An `Agent` may hold several at once (a reconnect overlapping the old socket, a second device).
*   **`Agent`**: who is connected, in your own id type. Assigned by the route or by an `AgentFactory`.
*   **`OutboundFrame`**: one fully encoded message, kind tag then body, refcounted so a broadcast to N recipients costs a refcount bump each.
*   **`WireCodec`**: how values become bytes. `JsonCodec` ships; supply your own for MessagePack, bincode or anything else.
*   **`ProtocolVersion`**: what a build declares in its `Hello`, so a skewed peer learns on connect instead of mis-decoding.
*   **Transport plane**: the round trip the socket's own ping measures, underneath everything this crate does.
*   **Plaza plane**: the round trip a `Kind::Ping` frame measures, through the codec and the conditioner, which is what a real message pays.
*   **`LinkProfile`**: delay, jitter and loss for one connection, applied where the connection is. `up` is what the client sends, `down` what the server sends.
*   **`Workload`**: what your application does, in terms you already know, which every queue depth and cap is derived from.
*   **`Overflow`**: what each queue does when it is full, per queue, because the producers differ.
*   **`ConnectionOrder`**: a close or a deadline delivered to a connection task on its own channel.

## Quick Start

### WebSockets on actix-web

Construct the session, share it with both the controller and your actix `App`, then hand connections over in the route.

```rust,ignore
use plaza_session::ActixWsPlazaSession;

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

`handle_connection` completes the handshake, registers the client and runs the pump; it deregisters when the socket closes. That route is the whole integration.

### Length-Delimited TCP

```rust,ignore
use plaza_session::TcpPlazaSession;

let session = TcpPlazaSession::<Op, PlayerId>::bind(
  "127.0.0.1:9000",
  Arc::new(|peer| Ok(Agent::new_human(id_for(peer)))),
).await?;
```

Binding happens before the accept loop starts, so a port already in use surfaces as `SessionLayerError::Bind` rather than killing a detached task.

The factory can also refuse:

```rust,ignore
Arc::new(|peer| {
  if banned(peer.ip()) {
    Err(Refusal::saying(farewell.clone()))
  } else {
    Ok(Agent::new_human(id_for(peer)))
  }
})
```

A refusal happens **before** `register`: nothing is allocated, announced or snapshotted, and no presence event fires. Only rules keyed on what a socket shows can fire here; a ban keyed on an account has to wait for the op that names it.

## A Tokio Runtime Is Required

Every constructor spawns the task that decodes inbound frames.

*   `TcpPlazaSession::bind*` is `async`, so it already is inside one.
*   `ActixWsPlazaSession::{new, with_codec, with_protocol, with_options}` and `TransportSession::{new, with_protocol, with_options}` are **synchronous**, and called outside a runtime they panic with a message naming tokio rather than plaza.

In an actix `main` you are already inside one. Anywhere else, construct inside `Runtime::block_on` or from an async fn.

## Choosing a Wire Format

```rust,ignore
let session = ActixWsPlazaSession::with_codec(MsgPackCodec);
```

`JsonCodec` is the default and readable from a browser console or `websocat`. Outbound frame type follows the codec: `WireCodec::is_text()` decides, so JSON sends text frames and a binary codec sends binary.

### Declaring a Protocol Version

```rust,ignore
let session = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
```

The version is announced to every client as a `Hello` before anything else, so a stale build hears about the skew on connect instead of mis-decoding one variant at a time. Derive `PROTOCOL` in a `build.rs` with [`plaza_wire::build`](../wire/) rather than maintaining a constant by hand.

### Deciding What a Mismatch Means

This layer records what a peer declared, keeps serving it, and lets you read it back. It does not refuse and does not warn: a version is a build hash, so a peer that merely recompiled is indistinguishable here from one whose shapes changed.

```rust,ignore
if let Some(theirs) = session.protocol(&id) {
  if theirs != ProtocolVersion(PROTOCOL) {
    return vec![TargetedOp::new_system_to(id, vec![
      Op::Outdated { server: PROTOCOL, client: theirs.0 },
    ])];
  }
}
```

Refuse the seat, serve a degraded stream, show a banner or tell the client to reload. The `Hello` is how a version gets across; an op of yours is how the game answers.

## Configuring a Session

### Naming a Workload

Every depth and cap has a default, and every default is a guess about a server this crate has never seen. The shortest way to replace all of them is to say what your application does.

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .workload(&Workload::action())
```

Seven presets ship: `action`, `horde`, `turn_based`, `social_relay`, `spectator`, `lobby`, `local`. Each is a `Workload` literal, so changing one field is the same mechanism as writing your own:

```rust,ignore
let mut workload = Workload::action();
workload.peak_players = 200;
workload.socket_buffer_bytes = 2 * 1024 * 1024;   // a tuned Linux box, not this laptop
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).workload(&workload)
```

What the presets derive today:

| preset | inbound | decoded | presence | outbound |
|---|---|---|---|---|
| `action` | 48 | 48 | 16 | 4 |
| `horde` | 192 | 192 | 64 | 47 |
| `turn_based` | 16 | 16 | 16 | 4 |
| `social_relay` | 4096 | 4096 | 512 | 4 |
| `spectator` | 8 | 8 | 512 | 4 |
| `lobby` | 8 | 8 | 4096 | 4 |
| `local` | 32 | 32 | 32 | 4 |

The striking column is `outbound`, and it is measured rather than chosen: a stalled client's socket already holds roughly 540 KiB before this crate's queue is what fills, which is over a thousand frames at 512 bytes and fourteen at 40 KiB. For a small-payload game the outbound queue is nearly a no-op and the kernel is doing the work. It becomes the binding term only once frames are large, which is why `horde` is the one preset needing a real one.

### Setting Depths and Caps by Hand

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .outbound_capacity(512)       // frames one slow client may fall behind by
  .presence_capacity(1024)      // joins and leaves waiting for the controller
  .max_frame_bytes(256 * 1024)  // largest inbound frame this build will accept
```

Or a whole group at once:

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .queues(Queues { inbound: 256, decoded: 256, presence: 64, outbound: 16, conditioner: 1024 })
  .limits(Limits { max_frame_bytes: 256 * 1024, max_message_bytes: 1024 * 1024 })
```

Read them back where a third-party transport picks them up:

```rust,ignore
let depth = manager.queues().outbound;
let cap = manager.limits().max_frame_bytes;
```

### What a Full Queue Does

Depth is half the decision. The other half is what happens when it runs out, and the right answer differs per queue because the producers differ.

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .disconnect_slow_clients()   // outbound: end the connection rather than the frame
  .backpressure_inbound()      // inbound: stop reading that socket while the controller is behind
  .backpressure_presence()     // presence: hold at registration rather than lose a join
```

Or all at once:

```rust,ignore
.overflow(Overflow::drop_everywhere())      // what ships
.overflow(Overflow::block_where_possible()) // waits at the two queues that have an arm to wait on
```

Two have a failure mode worth knowing before you choose them. `backpressure_presence` wedges every connection at registration if the session starts before its controller and the presence queue fills, which is the exact case dropping exists for. `backpressure_inbound` is TCP backpressure on one client, which is the point, but a controller that falls behind applies it to every client at once.

One place deliberately ignores the presence policy: a departure caused by `disconnect_slow_clients` is announced without waiting, even under `backpressure_presence`, because a send that disconnects a client must not block on the controller hearing about it.

## Measuring Latency

### The Two Planes

Both are measured by the server, and neither is a number the client reported. Nothing is added to your protocol for either.

```rust,ignore
let transport = session.agent_rtt(&id);        // the socket's own ping, under everything
let link = session.agent_link_rtt(&id);        // a Kind::Ping frame, through the conditioner
```

The gap between them is what plaza and the configured link cost this connection. On TCP there is no transport-plane ping, so the link plane is the only round trip there is.

Compare the **minimum** against a budget, not the mean: jitter only ever adds delay, so the smallest sample is the honest estimate.

```rust,ignore
let (smoothed, min, samples) = session.connection_rtt(conn_id)?;
if min > budget {
  refuse(id);
}
```

### Answering a Probe With Your Clock

A `Pong` carries the responder's clock, which lets a client estimate the offset between two timelines rather than only the distance between them.

```rust,ignore
let sim_clock = Arc::new(AtomicU64::new(0));
let session = ActixWsPlazaSession::with_options(
  MsgPackCodec,
  SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
    let sim_clock = sim_clock.clone();
    move || sim_clock.load(Ordering::Relaxed)
  }),
);

// On the simulation loop, once a tick:
sim_clock.store(state.tick_ms, Ordering::Relaxed);
```

The closure runs on a connection task, so an authoritative clock is **published** rather than borrowed. The unit is yours and this crate never reads it as a quantity. Without a clock, `Pong.responder` is `None` and a client can still measure its round trip.

### Turning Probes Off

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).without_probes()
```

An inbound `Ping` is still answered, since refusing would break a peer measuring its own side. What stops is this session originating them, and `agent_link_rtt` then stays `None`.

### Changing the Probe Schedule

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .probe_schedule(8, Duration::from_millis(125), Duration::from_secs(5))
  .probe_slots(16)
```

The defaults spend eight probes at 125 ms before settling to one every five seconds, which puts several samples inside the first second and then keeps an eye on a link that changes later. A LAN server and a global one want different numbers.

`slots` is not one of those. A probe is answered a round trip after it goes out and the fast phase sends another every 125 ms, so on any link slower than that the reply lands after its successor was sent. Tracking one at a time discards every such sample, leaving the link unmeasured at precisely the latencies worth measuring.

## Watching Connections

### Who Is Idle

```rust,ignore
if let Some(idle) = session.manager().agent_idle_for(&id) {
  if idle > Duration::from_secs(300) {
    kick(id);
  }
}
```

Time since the last **data** frame. Probes do not count, and only the session can promise that: the control plane answers a `Ping` without the application ever seeing it, so an AFK rule written anywhere else either counts probe traffic as presence or never fires. No timer and no timeout ship with it.

### Who Is Sending How Much

```rust,ignore
let volume = session.manager().agent_inbound(&id);   // monotonic frames and bytes
let delta = volume.frames - last.frames;
```

`TransportStats` counts the session as a whole, which can say *that* something floods but never *who*. Windows and thresholds stay yours: diff two readings, or feed a `plaza_server_utils::RateMeter`.

## Impairing a Link

### Setting a Profile

```rust,ignore
session.set_agent_link_profile(&id, LinkProfile::symmetric(DirectionProfile {
  delay: Duration::from_millis(80),
  jitter: Duration::from_millis(20),
  loss: 0.02,
  delivery: Delivery::Reliable,
}));

session.set_all_link_profiles(profile);   // what a room-conditions panel wants
```

`symmetric` applies the same each way, so the 80 ms above is a 160 ms round trip. A default profile is passthrough and costs nothing: no queue, no allocation, no deadline arithmetic.

Setting an agent or all-connection profile also clears that connection's link readings, because `agent_link_rtt` reports a minimum and a minimum taken under the old link would outlive it.

### What a Loss Costs

`loss` is the probability a frame is lost. `delivery` says what that means, and the two are different link types rather than two knobs on one.

*   **`Delivery::Reliable`**, the default and the truth about both transports here. The frame arrives one retransmission timeout late and everything behind it waits. **Nothing is deleted**, because on a reliable stream a lost segment never reaches the application as a missing message.
*   **`Delivery::Datagram`**, where the frame is gone and the two ends reconcile. Over a WebSocket this is a deliberate simulation of a transport plaza does not yet have, useful for exercising recovery before the channel it is for exists.

No frame kind is exempt under either model. Under `Reliable` nothing is lost at all; under `Datagram` a lost probe costs one sample of the several in flight, and a lost `Hello` reads as a peer that declared nothing.

### What the Link Reports

```rust,ignore
let total = session.link_dropped();
let theirs = session.agent_link_dropped(&id);
```

Worth reading precisely because an application cannot count it for itself: what the link lost never reaches the application.

Two guarantees the conditioner makes:

*   **Order is preserved.** Release times are made monotone as frames queue, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall then a burst.
*   **Everything crosses it**, the link-plane probe included, which is why `agent_link_rtt` moves when you drag a latency slider and `agent_rtt` does not.

## Ending a Session

### Closing One Connection

```rust,ignore
let farewell = session.encode_message(SessionMessage::system(vec![Op::Kicked { why }]))?;
for conn_id in session.manager().connections_of(&player) {
  session.manager().close_connection(conn_id, Some(farewell.clone()));
}
```

The connection task flushes what was queued, writes the farewell last and closes the socket. The departure then arrives on the presence stream as an ordinary `Left`, so game logic keeps one disconnect story whether the cable was pulled or the host said go.

The farewell is an op of your own vocabulary, not a transport code. Neither transport has a close vocabulary of its own, and "removed by the host" is an application word.

`deregister` is **not** a close. It removes the connection from the registry and nothing else; the socket belongs to the connection task, and only an order through `close_connection` reaches it.

### Kicking an Agent, Draining a Room

```rust,ignore
let closed = session.manager().deregister_agent(&id, Some(farewell.clone()));
let drained = session.manager().disconnect_all(Some(goodbye));
```

Everyone told, then closed. A drain differs from a kick only in who it names.

Which connection goes is policy and stays yours: a duplicate login can refuse the newcomer or kick the older session with the same two calls.

### Bounding a Session With a Deadline

```rust,ignore
session.manager().set_deadline(conn_id, Some(Duration::from_secs(600)), Some(farewell));
session.manager().set_deadline(conn_id, Some(Duration::from_secs(600)), None);  // renew
session.manager().set_deadline(conn_id, None, None);                            // clear
```

The connection task enforces it in its own loop and expiry goes through the same flush-then-farewell close. Setting again replaces it, which is how a renewal extends a session. What stamps, renews or revokes it is yours.

## Hosting a Browser Client

### Serving the Bundle

One process binds a port, serves a wasm or JS bundle from it, and puts the WebSocket route on the same origin, so the page connects back to whoever served it.

```rust,ignore
Host::new("0.0.0.0:8080")
  .serve_dir(Some("static".to_owned()))
  .cache_bust("client.wasm")
  .run(move |cfg| {
    cfg.route("/ws", web::get().to(ws_route));
  })
  .await
```

`serve_dir` preflights the directory at startup rather than per request. `announce(false)` silences the banner, `ws_path` changes what it prints. Signals are left to the process: actix catching Ctrl-C for a graceful shutdown while a game window keeps running is why a windowed host could not be killed.

```rust,ignore
if let Some(addr) = plaza_session::host::lan_address() {
  println!("tell your friend: http://{addr}:8080");
}
```

### Cache Busting

**Not optional, and the subtle part.** A browser client is a build product that does not rebuild when the server does, so a page built before a wire change still loads, still appears to run, and only the messages whose shape changed are rejected. That reads as a netcode bug and is a deployment one.

Two halves have to be present together: `cache_bust` stamps the asset's modification time into a dynamically served `index.html`, read per request so rebuilding the client reaches an already running host without a restart, and static assets are served `no-cache`, which is what makes the stamp effective. A cached page keeps quoting the old stamp, which is the trap that makes cache busting look like it does not work.

The third half is on the wire: [`plaza_wire::build`](../wire/) derives a protocol version by hashing the sources that define your messages, so a client can announce what it was built against and be told to reload.

### The Whole Simulation Stack

For a delta-streaming simulation, `SimHost` is everything between "I have a `StateLogic`" and "it is listening".

```rust,ignore
SimHost::new(bind, Duration::from_millis(SIM_STEP_MS))
  .serve_dir(static_dir)
  .cache_bust("my_game.wasm")
  .run(MsgPackCodec, PROTOCOL, Arena::new(initial), |wiring| {
    ArenaLogic::new(controls, view)
      .with_link(wiring.link_sink())          // where a panel's impairment sliders go
      .with_clock(wiring.sim_clock.clone())   // store your tick into this each step
  })
  .await
```

It decides three things for you, and each is unmade by using the blocks directly: joiners get no snapshot, connections are numbered `u64` agents on a `/ws` route it registers itself, and the driver is `run_fixed`. The one with a named alternative is the driver:

```rust,ignore
SimHost::measured(bind, tick_hz)   // delivers measured elapsed time instead of fixed steps
```

Use it only for logic that integrates over elapsed time and lets clients absorb the difference as corrections.

## Writing Another Transport

### The Connection Loop

```rust,ignore
let session = TransportSession::with_options(name, codec, options);
let manager = session.manager();

// Per connection:
let (tx, rx) = plaza::session::session_channel(manager.queues().outbound);
let conn_id = manager.register(agent.clone(), tx).await;
let mut driver = LinkDriver::new(manager, conn_id, codec).expect("registered");
let mut orders = manager.take_orders(conn_id).expect("once");

loop {
  tokio::select! {
    inbound = socket.read_frame() => match driver.inbound(inbound?, Instant::now()) {
      Inbound::Reply(reply) => socket.write(reply).await?,
      Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
      Inbound::Consumed => {}
    },
    outbound = to_client_rx.recv() => {
      if let Some(frame) = driver.outbound(outbound?, Instant::now()) {
        socket.write(frame).await?;
      }
    }
    _ = sleep_until(driver.deadline().unwrap_or_else(far_future)), if driver.deadline().is_some() => {
      for frame in driver.due(Instant::now()) { socket.write(frame).await?; }
      for frame in driver.take_forwarded() { manager.forward_incoming(agent.clone(), frame).await; }
    }
    order = orders.recv() => match order? {
      ConnectionOrder::Close { farewell } => { flush_and_close(farewell).await?; break; }
      ConnectionOrder::Deadline { after, farewell } => arm(after, farewell),
    }
  }
}
manager.deregister(conn_id).await;
```

The orders must be their own `select!` arm: the outbound arm is disabled the moment `deregister` drops the sender, which is exactly when a close must still work.

Delegate the three `Session` methods to the inner `TransportSession`, and after `broadcast` call `disconnect_overflowed` with what it returned.

What you still write is framing, and enforcing `Limits::max_frame_bytes` with it. That is what a transport is.

**Answer probes or say why not.** A `Kind::Ping` handed to `forward_incoming` is answered by nobody: the bridge drops it and warns once per connection, and the client measuring its round trip waits forever. `LinkDriver` handles this, so the only way to get it wrong is to bypass it and forget.

`examples/foreign_soil` is a working transport built this way, in a crate with no privileged access and neither shipped transport compiled in. Its connection loop is 65 lines, about 25 of them reading and writing a socket.

### Assembling the Pieces Yourself

`LinkDriver` is a convenience, not a ceiling. `Conditioner`, `ProbeState` and `LinkHandle` are public and each is useful alone.

```rust,ignore
let link = manager.link_handle(conn_id).expect("registered");
let mut probe = ProbeState::new(manager.probes());
let mut up = Conditioner::new(conn_id, manager.queues().conditioner);
let mut down = Conditioner::new(conn_id ^ DOWN_SEED_FLIP, manager.queues().conditioner);
let mut generation = link.generation();

// The fast path, per frame:
if !link.impaired() && down.is_empty() {
  socket.write(frame).await?;
}

// A profile that moved invalidates the probes straddling it:
if link.generation() != generation {
  generation = link.generation();
  probe.forget_in_flight();
}

let wake = control::earliest(next_probe, control::earliest(up.next_release(), down.next_release()));
```

The case to expect is a transport whose link genuinely reorders: the shipped conditioner releases monotonically because a byte stream does not, so a datagram transport keeps the probe plane and writes its own release queue.

## Error Handling

The transport error type is `SessionLayerError`, deliberately non-generic because these concern sockets and wire formats rather than application agent ids.

```rust,ignore
match TcpPlazaSession::<Op, PlayerId>::bind(addr, factory).await {
  Ok(session) => session,
  Err(SessionLayerError::Bind { addr, source }) => {
    eprintln!("cannot bind {addr}: {source}");
    return;
  }
  Err(e) => return eprintln!("{e}"),
}
```

*   `Bind { addr, source }`: the listener could not be created.
*   `Serialization { transport, context, source }` / `Deserialization { .. }`: a codec failure, with the transport and call site that hit it.
*   `ClientSendFailed { transport, conn_id, reason }`: a frame could not be handed to a connection.

`impl From<SessionLayerError> for PlazaError<ID>` maps serialization and deserialization onto the matching `PlazaError` variants and everything else onto `PlazaError::Session`, so the `#[source]` chain stays readable.

A malformed body of any kind is a per-message problem: it is logged and dropped, never a disconnect. An unknown frame tag is skipped with a `trace!` and the connection carries on.

Counters live on `TransportStats`, and the three drop counts stay separate because they mean different things. An outbound drop is usually benign for a stream of absolute state. An inbound drop is player input the client believes arrived. A presence drop is a correctness failure from a single occurrence: a lost join leaves the controller with a client it never heard of, a lost leave leaves it holding a seat forever.

```rust,ignore
let stats = session.stats();
gauge("inbound_dropped", stats.inbound_dropped());
gauge("presence_dropped", stats.presence_dropped());
```
