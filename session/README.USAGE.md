# Usage Guide: plaza_session

How to put a real network transport under a `plaza` `StateController`: standing a session up on WebSockets or TCP, sizing what it holds, measuring each connection, limiting what each connection sends and is sent, impairing a link, ending a session, admitting a connection by credential, hosting a browser client and writing a transport of your own.

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
*   [Limiting How Fast a Client Sends](#limiting-how-fast-a-client-sends)
    *   [Setting a Rate](#setting-a-rate)
    *   [Deriving a Rate From a Workload](#deriving-a-rate-from-a-workload)
    *   [Shedding a Frame or Closing the Connection](#shedding-a-frame-or-closing-the-connection)
    *   [Seeing Who Was Shed](#seeing-who-was-shed)
    *   [Judging the Rate in Your Own Transport](#judging-the-rate-in-your-own-transport)
*   [Budgeting What a Client Is Sent](#budgeting-what-a-client-is-sent)
    *   [Giving Every Connection a Budget](#giving-every-connection-a-budget)
    *   [Changing One Connection's Budget](#changing-one-connections-budget)
    *   [Skipping a Recipient in the Snapshot Pass](#skipping-a-recipient-in-the-snapshot-pass)
    *   [Seeing What Was Withheld](#seeing-what-was-withheld)
*   [Impairing a Link](#impairing-a-link)
    *   [Setting a Profile](#setting-a-profile)
    *   [What a Loss Costs](#what-a-loss-costs)
    *   [What the Link Reports](#what-the-link-reports)
*   [Ending a Session](#ending-a-session)
    *   [Closing One Connection](#closing-one-connection)
    *   [Kicking an Agent, Draining a Room](#kicking-an-agent-draining-a-room)
    *   [Bounding a Session With a Deadline](#bounding-a-session-with-a-deadline)
    *   [Choosing a Close Code](#choosing-a-close-code)
*   [Admitting a Connection by Credential](#admitting-a-connection-by-credential)
    *   [Writing an Admitter](#writing-an-admitter)
    *   [Binding TCP With an Admitter](#binding-tcp-with-an-admitter)
    *   [Admitting on a WebSocket Route](#admitting-on-a-websocket-route)
    *   [Presenting a Credential From the Client](#presenting-a-credential-from-the-client)
    *   [Settling a Duplicate Login](#settling-a-duplicate-login)
    *   [Bounding the Wait for a Credential](#bounding-the-wait-for-a-credential)
    *   [Counting What the Door Did](#counting-what-the-door-did)
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
*   **`Farewell`**: the close code and optional detail every server-initiated close carries, written to the client as a `Kind::Goodbye` frame.
*   **`Rate`**: a per-connection token bucket on inbound frames, judged before the queue every connection shares.
*   **`OutboundBudget`**: how much one connection may be sent, which a snapshot pass asks about before it builds a frame for that recipient.
*   **`ConnectionAdmitter`**: your code that reads a client's credential and answers with the `Agent` to register or a `Farewell` to refuse.
*   **Pending connection**: a socket that has completed its handshake and waits, unregistered, for its credential.

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

`handle_connection` completes the handshake, registers the client and runs the pump; it deregisters when the socket closes. Nothing beyond that route is needed.

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
    Err(Farewell::new(BANNED).with_detail("this address is banned".as_bytes()))
  } else {
    Ok(Agent::new_human(id_for(peer)))
  }
})
```

A refusal happens **before** `register`: nothing is allocated, announced or snapshotted and no presence event fires. Only rules keyed on what a socket shows can fire here; a ban keyed on an account has to wait for the op that names it.

## A Tokio Runtime Is Required

Every constructor spawns the task that decodes inbound frames.

*   `TcpPlazaSession::bind*` is `async`, so it already is inside one.
*   `ActixWsPlazaSession::{new, with_codec, with_protocol, with_options}` and `TransportSession::{new, with_protocol, with_options}` are **synchronous** and called outside a runtime they panic with a message naming tokio rather than plaza.

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

This layer records what a peer declared, keeps serving it and lets you read it back. It does not refuse and does not warn: a version is a build hash, so a peer that merely recompiled is indistinguishable here from one whose shapes changed.

```rust,ignore
if let Some(theirs) = session.protocol(&id) {
  if theirs != ProtocolVersion(PROTOCOL) {
    return vec![TargetedOp::new_system_to(id, vec![
      Op::Outdated { server: PROTOCOL, client: theirs.0 },
    ])];
  }
}
```

The game decides what to do and answers with an op of its own: refuse the seat, serve a degraded stream, show a banner or tell the client to reload.

## Configuring a Session

### Naming a Workload

Every depth and cap has a default that knows nothing about your server. The shortest way to replace all of them is to describe what your application does.

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
| `action` | 32 | 32 | 16 | 4 |
| `horde` | 128 | 128 | 64 | 47 |
| `turn_based` | 16 | 16 | 16 | 4 |
| `social_relay` | 4096 | 4096 | 512 | 4 |
| `spectator` | 8 | 8 | 512 | 4 |
| `lobby` | 8 | 8 | 4096 | 4 |
| `local` | 32 | 32 | 32 | 4 |

The `outbound` column is measured rather than chosen: a stalled client's socket already holds roughly 540 KiB before this crate's queue is what fills, which is over a thousand frames at 512 bytes and 13 at 40 KiB. For a small-payload game the outbound queue is nearly a no-op and the kernel buffer absorbs the backlog. The queue only matters once frames are large, which is why `horde` is the one preset needing a real one.

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
  .limits(Limits { max_frame_bytes: 256 * 1024, max_message_bytes: 1024 * 1024, ..Limits::default() })
```

Read them back where a third-party transport picks them up:

```rust,ignore
let depth = manager.queues().outbound;
let cap = manager.limits().max_frame_bytes;
```

### What a Full Queue Does

Besides its depth, each queue needs a policy for when it runs out. The right answer differs per queue because the producers differ.

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

Two have a failure mode worth knowing before you choose them. `backpressure_presence` wedges every connection at registration if the session starts before its controller and the presence queue fills, which is the exact case dropping exists for. `backpressure_inbound` is meant to apply TCP backpressure to one client, but a controller that falls behind applies it to every client at once.

One place deliberately ignores the presence policy: a departure caused by `disconnect_slow_clients` is announced without waiting, even under `backpressure_presence`, because a send that disconnects a client must not block on the controller hearing about it.

## Measuring Latency

### The Two Planes

Both are measured by the server and neither is a number the client reported. Nothing is added to your protocol for either.

```rust,ignore
let transport = session.agent_rtt(&id);        // the socket's own ping, under everything
let link = session.agent_link_rtt(&id);        // a Kind::Ping frame, through the conditioner
```

The gap between them is what plaza and the configured link cost this connection. On TCP there is no transport-plane ping, so the link plane is the only round trip there is.

Compare the **minimum** against a budget rather than the mean. Jitter only ever adds delay, so the smallest sample is the closest to the real round trip.

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

The closure runs on a connection task, so the simulation loop stores its clock into a shared atomic for the closure to read. The unit is yours and this crate never reads it as a quantity. Without a clock, `Pong.responder` is `None` and a client can still measure its round trip.

### Turning Probes Off

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).without_probes()
```

An inbound `Ping` is still answered, since refusing would break a peer measuring its own side. What stops is this session originating them and `agent_link_rtt` then stays `None`.

### Changing the Probe Schedule

```rust,ignore
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .probe_schedule(8, Duration::from_millis(125), Duration::from_secs(5))
  .probe_slots(16)
```

The defaults spend eight probes at 125 ms before settling to one every five seconds, which puts several samples inside the first second and then keeps measuring in case the link changes later. A LAN server and a global one want different numbers.

`slots` is not one of those. A probe is answered a round trip after it goes out and the fast phase sends another every 125 ms, so on any link slower than that the reply lands after its successor was sent. Tracking one at a time discards every such sample, so a link slower than 125 ms goes unmeasured.

## Watching Connections

### Who Is Idle

```rust,ignore
if let Some(idle) = session.manager().agent_idle_for(&id) {
  if idle > Duration::from_secs(300) {
    kick(id);
  }
}
```

Time since the last **data** frame. Probes do not count and only the session can promise that: the control plane answers a `Ping` without the application ever seeing it, so an AFK rule written anywhere else either counts probe traffic as presence or never fires. No timer and no timeout ship with it.

### Who Is Sending How Much

```rust,ignore
let volume = session.manager().agent_inbound(&id);   // monotonic frames and bytes
let delta = volume.frames - last.frames;
```

`TransportStats` counts the session as a whole, so it can show that something is flooding but not which connection. Windows and thresholds stay yours: diff two readings or feed a `plaza_server_utils::RateMeter`.

## Limiting How Fast a Client Sends

### Setting a Rate

```rust,ignore
use plaza_session::Rate;

SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .rate_limit_inbound(Rate::per_second(60.0).burst(120))
```

A `Rate` is a token bucket: `per_sec` frames a second sustained and `burst` frames at once from a full bucket. `Rate::per_second` alone gives a burst of one second's worth. There is no default, so a session without a rate admits everything.

The builder only sets `Limits::inbound_rate`, so setting the field does the same:

```rust,ignore
let mut options = SessionOptions::with_protocol(ProtocolVersion(PROTOCOL));
options.limits.inbound_rate = Some(Rate::per_second(60.0));

let current = session.manager().limits().inbound_rate;   // read it back
```

The gate runs on each connection's task before the inbound queue every connection shares, so a flood costs the flooder and nobody else. It counts frames and never reads their content. Frame size is already capped by `max_frame_bytes` and `max_message_bytes`, so the rate times the cap is a byte ceiling.

### Deriving a Rate From a Workload

```rust,ignore
let workload = Workload::action();
SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .workload(&workload)
  .rate_limit_inbound(Rate::for_workload(&workload))
```

`workload` derives every queue and cap but does not derive a rate, because a rate is the one setting that refuses traffic. It replaces the whole `Limits` with `Limits::for_workload`, which sets `inbound_rate` and `outbound_budget` to `None`, so a rate or budget set before `.workload()` is wiped. Call `.workload()` first and set the rate after it. `Rate::for_workload` takes `tick_rate * ops_per_player_per_tick`, multiplies it by `RATE_HEADROOM` (4.0) and floors it at `MIN_INBOUND_RATE` (5.0) frames a second. The headroom is large on purpose: too high still bounds a flood, too low drops input an honest client believes arrived.

### Shedding a Frame or Closing the Connection

```rust,ignore
Rate::per_second(60.0)                   // Over::Shed: drop the frame, keep the connection
Rate::per_second(600.0).disconnecting()  // Over::Close: end the connection
```

Shed is right when the traffic is an eager client and the next op supersedes the dropped one. The dropped frames are ops the client believes arrived, so a stream where each op matters exactly once wants `disconnecting()` or a rate it will never hit. Pick `Close` for a rate no honest build can reach. WebSocket closes with 1008 (policy violation). TCP has no close code, so it writes a Goodbye carrying `Farewell::new(POLICY_VIOLATION)` and then closes the connection.

### Seeing Who Was Shed

```rust,ignore
let manager = session.manager();
let total = manager.stats().inbound_shed();       // the whole session
let theirs = manager.agent_inbound(&id).shed;     // one agent, monotonic

if theirs - last_shed > 50 {
  manager.deregister_agent(&id, Farewell::new(FLOODING).with_detail("slow down".as_bytes()));
}
```

`inbound_shed` names one connection that went over its rate and costs only that connection. `inbound_dropped` means the controller fell behind and costs whoever happened to be sending. When both climb, look at the shed first. A shed frame still counts toward `frames` and still resets `agent_idle_for`, since it did arrive.

### Judging the Rate in Your Own Transport

`LinkDriver::inbound` applies the rate and adds two outcomes to the match:

```rust,ignore
use plaza_session::admission::POLICY_VIOLATION;
use plaza_session::control::Inbound;

match driver.inbound(frame, Instant::now()) {
  Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
  Inbound::Reply(reply) => socket.write(reply).await?,
  Inbound::Consumed | Inbound::Shed => {}
  Inbound::Eject => {
    flush_and_close(Farewell::new(POLICY_VIOLATION)).await?;
    break;
  }
}
```

A frame held by an upstream `LinkProfile` is judged when the link releases it, inside `due`. Check `driver.ejected()` after `due` and close the same way.

A transport that skips `LinkDriver` asks the manager for each inbound data frame. The result is `#[must_use]` and a refused frame must not be forwarded:

```rust,ignore
use plaza_session::Verdict;

match manager.record_inbound_activity(conn_id, frame.len()) {
  Verdict::Admit => manager.forward_incoming(agent.clone(), frame).await,
  Verdict::Shed => {}
  Verdict::Close => {
    flush_and_close(Farewell::new(POLICY_VIOLATION)).await?;
    break;
  }
}
```

The full surface is in [module `gate`](API_REFERENCE.md#10-module-gate).

## Budgeting What a Client Is Sent

### Giving Every Connection a Budget

```rust,ignore
use plaza_session::OutboundBudget;

SessionOptions::with_protocol(ProtocolVersion(PROTOCOL))
  .budget_outbound(OutboundBudget::bytes_per_second(64.0 * 1024.0).and_frames_per_second(20.0))
```

`bytes_per_second` and `frames_per_second` each set one bound and leave the other unbounded. The `and_` forms add the second and then both have to hold. `burst(Duration)` sets how much unspent credit a connection may bank, one second of its rate by default. There is no default budget: a connection without one is always owed a frame.

As with the rate, the builder sets a field on `Limits`:

```rust,ignore
options.limits.outbound_budget = Some(OutboundBudget::frames_per_second(10.0).burst(Duration::from_millis(500)));
```

### Changing One Connection's Budget

```rust,ignore
let manager = session.manager();
manager.set_outbound_budget(conn_id, Some(OutboundBudget::frames_per_second(5.0)));
let given = manager.set_agent_outbound_budget(&id, Some(OutboundBudget::bytes_per_second(10.0 * 1024.0)));
manager.set_agent_outbound_budget(&id, None);   // unbounded again, debt included

let current = manager.outbound_budget(conn_id);
```

A new budget starts with a full burst. This is where a client that declared what its link can carry gets its budget, after the server has clamped the declaration to something it will honour.

### Skipping a Recipient in the Snapshot Pass

The transport never withholds a frame. `broadcast` still queues everything it is handed and charges each recipient's credit for it. The budget answers one question, asked by a `SnapshotProvider` before it builds for a recipient:

```rust,ignore
async fn create_snapshot(
  &self,
  state: &ArenaState,
  target: Option<&Agent<PlayerId>>,
  _context: Option<SnapshotContext>,
) -> Result<Option<RoomOp>, SnapshotError<PlayerId>> {
  let viewer = target.and_then(|agent| agent.id());
  if let Some(viewer) = viewer
    && !self.manager.agent_owed(viewer)
  {
    return Ok(None);
  }
  Ok(Some(RoomOp::Snapshot(Box::new(state.view_for(viewer)))))
}
```

`connection_owed(conn_id)` is the same question for one connection. An agent is owed a frame when any of its connections is. A connection that is gone is owed nothing.

Credit runs negative. A frame bigger than what is left still goes and the connection is owed nothing until the debt refills, so 10 KiB a second at 2 KiB a frame comes out as five whole frames a second. Skipping a delta-stream recipient costs latency and never correctness, since the next frame it gets carries everything since its acknowledged baseline.

### Seeing What Was Withheld

```rust,ignore
let theirs = manager.agent_outbound(&id).withheld;     // times this agent was asked for and not owed
let total = manager.stats().outbound_withheld();       // the whole session
```

`withheld` climbs only when a snapshot pass asks, so it counts frames that were never built. The sent counts cannot show those. The full surface is in [module `budget`](API_REFERENCE.md#11-module-budget).

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

`loss` is the probability a frame is lost. `delivery` says what that means. Each of its two variants models a different kind of link.

*   **`Delivery::Reliable`**, the default and what both transports here actually do. The frame arrives one retransmission timeout late and everything behind it waits. **Nothing is deleted**, because on a reliable stream a lost segment never reaches the application as a missing message.
*   **`Delivery::Datagram`**, where the frame is gone and the two ends reconcile. Over a WebSocket this is a deliberate simulation of a transport plaza does not yet have, useful for exercising recovery before the channel it is for exists.

No frame kind is exempt under either model. Under `Reliable` nothing is lost at all; under `Datagram` a lost probe costs one sample of the several in flight and a lost `Hello` reads as a peer that declared nothing.

### What the Link Reports

```rust,ignore
let total = session.link_dropped();
let theirs = session.agent_link_dropped(&id);
```

An application cannot count these itself, because what the link loses never reaches it.

Two guarantees the conditioner makes:

*   **Order is preserved.** Release times are made monotone as frames queue, so a delayed frame holds up everything behind it and a jitter spike arrives as a stall then a burst.
*   **Everything crosses it**, the link-plane probe included, which is why `agent_link_rtt` moves when you drag a latency slider and `agent_rtt` does not.

## Ending a Session

### Closing One Connection

```rust,ignore
let farewell = Farewell::new(KICKED).with_detail(why.as_bytes());
for conn_id in session.manager().connections_of(&player) {
  session.manager().close_connection(conn_id, farewell.clone());
}
```

The connection task flushes what was queued, writes the farewell as a `Kind::Goodbye` frame and closes the socket with its code where the transport has one. The departure then arrives on the presence stream as an ordinary `Left`, so game logic handles a kick the same way as a pulled cable.

The code and the detail are yours. WebSocket leaves 4000 to 4999 to applications and the session never reads the detail, so encode it however your protocol does.

`deregister` does **not** close the socket. It removes the connection from the registry and nothing else; the socket belongs to the connection task and only an order through `close_connection` reaches it.

### Kicking an Agent, Draining a Room

```rust,ignore
let closed = session.manager().deregister_agent(&id, farewell.clone());
let drained = session.manager().disconnect_all(Farewell::new(1001).with_detail("server shutting down".as_bytes()));
```

Each connection gets the farewell and is then closed. `disconnect_all` is the same close as `deregister_agent`, applied to every live connection.

Choosing which connection to close is up to you: a duplicate login can refuse the newcomer or kick the older session with the same two calls.

### Bounding a Session With a Deadline

```rust,ignore
session.manager().set_deadline(conn_id, Some(Duration::from_secs(600)), farewell.clone());
session.manager().set_deadline(conn_id, Some(Duration::from_secs(600)), farewell.clone());  // renew
session.manager().set_deadline(conn_id, None, farewell);                                    // clear
```

The connection task enforces it in its own loop and expiry goes through the same flush-then-farewell close. Setting again replaces it, which is how a renewal extends a session. What stamps, renews or revokes it is yours.

### Choosing a Close Code

```rust,ignore
use plaza_session::admission::{MESSAGE_TOO_BIG, POLICY_VIOLATION};
use plaza_wire::frame::Goodbye;

const KICKED: u16 = 4001;

Farewell::new(KICKED).with_detail("kicked by a moderator".as_bytes());
Farewell::new(1001).with_detail("server shutting down".as_bytes());
Farewell::credential_expected();   // Goodbye::CREDENTIAL_EXPECTED, 4401
Farewell::credential_timeout();    // Goodbye::CREDENTIAL_TIMEOUT, 4408
```

`Farewell` is the one close vocabulary: `close_connection`, `set_deadline`, `deregister_agent`, `disconnect_all`, an `AgentFactory` refusal and an admitter refusal all take one. The `code` is a WebSocket close code whatever the transport. On TCP the `Goodbye` frame is all the client hears. Your own codes go in 4000 to 4999 and the session never picks one for you.

The session sends four codes by itself:

| code | constant | when |
|---|---|---|
| 1008 | `POLICY_VIOLATION` | a connection went over an inbound rate built with `disconnecting()` |
| 1009 | `MESSAGE_TOO_BIG` | a credential frame was larger than `Limits::max_credential_bytes` |
| 4401 | `Goodbye::CREDENTIAL_EXPECTED` | an `Ops` frame arrived before the credential |
| 4408 | `Goodbye::CREDENTIAL_TIMEOUT` | no credential arrived within `Limits::credential_timeout` |

The client libraries stop reconnecting on a 4xxx code by default, because a server that refused a credential once will refuse it again. Let the code say what class of close it is and the detail say what to do next. Neither should explain the reason in enough detail to serve as an oracle for someone probing your rules.

## Admitting a Connection by Credential

### Writing an Admitter

A browser or mobile client cannot set a header on its WebSocket upgrade, so its identity arrives after the connection as a `Kind::Credential` frame. A `ConnectionAdmitter` reads it and decides:

```rust,ignore
use async_trait::async_trait;
use plaza_session::{ConnectionAdmission, ConnectionAdmitter, Farewell, Peer, WireCodec};

struct Doorman {
  door: Arc<Door>,
  manager: OnceLock<Arc<ConnectionManager<AgentKey>>>,
}

#[async_trait]
impl ConnectionAdmitter<AgentKey> for Doorman {
  async fn admit(&self, credential: &[u8], peer: &Peer) -> ConnectionAdmission<AgentKey> {
    let Ok(account) = JsonCodec.decode::<Account>(credential) else {
      return ConnectionAdmission::Refused(Farewell::new(4400).with_detail("unreadable credential".as_bytes()));
    };
    match self.door.admit(peer.addr.map(|a| a.ip()), account) {
      Ok(key) => ConnectionAdmission::Admitted(Agent::new_human(key)),
      Err(reason) => ConnectionAdmission::Refused(Farewell::new(reason.code()).with_detail(reason.as_str().as_bytes())),
    }
  }
}
```

`credential` is the frame body exactly as it arrived. Plaza verifies nothing, so a token, a signed ticket or an account number is all the same to it. `Admitted` registers the connection as that agent and the join fires from there. `Refused` writes the goodbye and closes without anything having been registered or announced. `peer.addr` is the socket's address when the transport knows it, for per-address rules and audit.

### Binding TCP With an Admitter

```rust,ignore
let doorman = Arc::new(Doorman::new(door.clone()));
let session = TcpPlazaSession::<Op, AgentKey>::bind_with_admitter(
  "0.0.0.0:9000",
  doorman.clone(),
  JsonCodec,
  SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)),
).await?;
doorman.attach(session.manager().clone());
```

`bind_with_admitter` replaces the `AgentFactory`: every accepted socket waits for its credential instead of registering at once. The admitter exists before the session it admits into, so one that needs the `ConnectionManager` takes it afterwards through a `OnceLock`, which is what `attach` does in `examples/door_policy`.

### Admitting on a WebSocket Route

```rust,ignore
async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ActixWsPlazaSession<Op, AgentKey>>>,
  doorman: web::Data<Arc<Doorman>>,
) -> Result<HttpResponse, actix_web::Error> {
  session.admit_connection(&req, stream, doorman.get_ref().clone())
}
```

`admit_connection` completes the handshake and holds the socket on its own task until the admitter answers. Use `handle_connection` instead when the route can resolve identity before the upgrade, from a cookie or a query string.

Anything the upgrade request carries, such as a room in the URL or a forwarded-for header behind a proxy, is the route's to capture. Build the admitter per request and close over it:

```rust,ignore
let room = req.match_info().get("room").map(str::to_owned);
session.admit_connection(&req, stream, Arc::new(RoomDoor { room, directory: directory.clone() }))
```

### Presenting a Credential From the Client

```rust,ignore
use plaza_ws::pump::FramePump;

let mut pump = FramePump::connect(&url, MsgPackCodec, PROTOCOL)?.credential(token);
```

The pump sends the credential straight after its `Hello` and before anything the application sends. Under a text codec the bytes must be text. A refusal arrives as `Arrival::Closed` with the goodbye's `code` and `detail`. `closed.refused()` is true for a 4xxx code.

### Settling a Duplicate Login

An admitter holding the manager can close the older session before it admits the newer one:

```rust,ignore
if let Some(manager) = self.manager.get() {
  for old in evicted {
    manager.deregister_agent(
      &old,
      Farewell::new(SIGNED_IN_ELSEWHERE).with_detail("signed in from somewhere else".as_bytes()),
    );
  }
}
ConnectionAdmission::Admitted(Agent::new_human(key))
```

This works because the older connection is registered and the newcomer is not yet. Refusing the newcomer instead is a `Refused` with a code of your own. `examples/door_policy` runs both policies alongside a ban, a per-address cap and a seat limit, all judged before anything registers.

### Bounding the Wait for a Credential

```rust,ignore
let mut options = SessionOptions::with_protocol(ProtocolVersion(PROTOCOL));
options.limits.credential_timeout = Duration::from_secs(2);   // default 5 s
options.limits.pending_connections = Some(256);                 // default Some(1024); None for no cap
options.limits.max_credential_bytes = 1024;                     // default 4096
```

These three have no one-call builders, so set them on `limits`. The timer starts after the handshake, so it only fires for a socket that never presents, which is closed with 4408. A credential over `max_credential_bytes` closes with 1009.

Over `pending_connections`, TCP accepts the socket and closes it at once and the WebSocket route answers 503 before the upgrade. The client sees a failed connect rather than a refusal.

Nothing crosses a pending socket. A `Hello` is kept and recorded once the connection is admitted, probes are neither answered nor forwarded, an unknown frame kind is skipped and an `Ops` frame closes the socket with 4401. The server sends nothing but a goodbye.

### Counting What the Door Did

```rust,ignore
let stats = session.manager().stats();
gauge("pending", stats.pending());       // waiting for a credential right now
gauge("admitted", stats.admitted());
gauge("refused", stats.refused());       // admitter and factory refusals alike
gauge("timed_out", stats.timed_out());   // closed with 4408
gauge("over_cap", stats.over_cap());     // turned away because pending_connections was full
```

The full surface, including the `Pending` state machine a custom transport can reuse, is in [module `admission`](API_REFERENCE.md#12-module-admission).

## Hosting a Browser Client

### Serving the Bundle

One process binds a port, serves a wasm or JS bundle from it and puts the WebSocket route on the same origin, so the page connects back to whoever served it.

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

Cache busting is required. A browser client is a build product that does not rebuild when the server does, so a page built before a wire change still loads, still appears to run and only the messages whose shape changed are rejected. This looks like a netcode bug even though the cause is a stale page.

Two parts have to be present together. `cache_bust` stamps the asset's modification time into a dynamically served `index.html`, read per request so rebuilding the client reaches an already running host without a restart. Static assets are also served `no-cache`. Without that, a cached page keeps quoting the old stamp and cache busting appears not to work.

A third part is on the wire: [`plaza_wire::build`](../wire/) derives a protocol version by hashing the sources that define your messages, so a client can announce what it was built against and be told to reload.

### The Whole Simulation Stack

For a delta-streaming simulation, `SimHost` is everything needed to take a `StateLogic` to a listening server.

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

It decides three things for you, each of which you can undo by using the blocks directly: joiners get no snapshot; connections are numbered `u64` agents on a `/ws` route it registers itself; the driver is `run_fixed`. The one with a named alternative is the driver:

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
      Inbound::Consumed | Inbound::Shed => {}
      Inbound::Eject => { flush_and_close(Farewell::new(POLICY_VIOLATION)).await?; break; }
    },
    outbound = to_client_rx.recv() => {
      if let Some(frame) = driver.outbound(outbound?, Instant::now()) {
        socket.write(frame).await?;
      }
    }
    _ = sleep_until(driver.deadline().unwrap_or_else(far_future)), if driver.deadline().is_some() => {
      for frame in driver.due(Instant::now()) { socket.write(frame).await?; }
      for frame in driver.take_forwarded() { manager.forward_incoming(agent.clone(), frame).await; }
      if driver.ejected() { flush_and_close(Farewell::new(POLICY_VIOLATION)).await?; break; }
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

Delegate the three `Session` methods to the inner `TransportSession`. Its `send_message` already calls `disconnect_overflowed` after `broadcast`. If you call `ConnectionManager::broadcast` directly, pass what it returns to `disconnect_overflowed` yourself.

What you still write is framing and enforcing `Limits::max_frame_bytes` with it.

**Answering probes.** A `Kind::Ping` handed to `forward_incoming` is answered by nobody: the bridge drops it and warns once per agent and the client measuring its round trip waits forever. `LinkDriver` answers probes, so this only goes wrong if you bypass it and do not answer them yourself.

`examples/foreign_soil` is a working transport built this way, in a crate with no privileged access and neither shipped transport compiled in. Its connection loop is 68 lines. 15 of them read or write the socket and the rest register the connection and call `LinkDriver`.

### Assembling the Pieces Yourself

`LinkDriver` is optional. `Conditioner`, `ProbeState` and `LinkHandle` are public and each is useful alone.

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

The usual reason to do this is a transport whose link really reorders frames. The shipped conditioner releases in order because a byte stream never reorders, so a datagram transport keeps the probe plane and writes its own release queue.

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

Counters live on `TransportStats` and the three drop counts stay separate because they mean different things. An outbound drop is usually benign for a stream of absolute state. An inbound drop is player input the client believes arrived. A presence drop is a correctness failure from a single occurrence: a lost join leaves the controller with a client it never heard of, a lost leave leaves it holding a seat forever.

```rust,ignore
let stats = session.manager().stats();
gauge("inbound_dropped", stats.inbound_dropped());
gauge("presence_dropped", stats.presence_dropped());
```
