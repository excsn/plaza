//! End-to-end check of the TCP transport: a framed client's op reaches the
//! controller's subscription, and a server broadcast reaches the client.

#![cfg(feature = "tcp")]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plaza::agent::Agent;
use plaza::session::{MessageTarget, PresenceEvent, Session, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_wire::frame::ProtocolVersion;
use plaza_session::{ConnectionAdmission, ConnectionAdmitter, DirectionProfile, Farewell, LinkProfile, Peer, TcpPlazaSession};
use plaza_wire::frame::{Goodbye, Kind};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;

type PlayerId = Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum TestOp {
  Hello(String),
  Welcome(String),
}

/// Fails the test rather than hanging if the transport never delivers.
async fn with_timeout<T>(fut: impl std::future::Future<Output = T>) -> T {
  tokio::time::timeout(Duration::from_secs(5), fut)
    .await
    .expect("timed out waiting for the transport")
}

/// For waits that depend on the probe schedule and on real sleeps rather than
/// on a frame the test just caused, so a loaded machine does not fail them.
async fn with_patience<T>(fut: impl std::future::Future<Output = T>) -> T {
  tokio::time::timeout(Duration::from_secs(30), fut)
    .await
    .expect("timed out waiting for a measurement")
}

/// Blocks until the agent's frame-path round trip has been measured.
async fn measured(session: &TcpPlazaSession<TestOp, PlayerId>, id: &PlayerId) -> (Duration, u64) {
  with_patience(async {
    loop {
      if let Some(measured) = session.manager().agent_link_rtt(id) {
        return measured;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
}

/// Blocks until the transport has registered a connection.
async fn registered(session: &TcpPlazaSession<TestOp, PlayerId>) {
  with_patience(async {
    loop {
      if session.manager().connection_count() > 0 {
        return;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
}

#[tokio::test]
async fn tcp_client_op_reaches_controller_and_broadcast_reaches_client() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory)
      .await
      .expect("bind should succeed on an ephemeral port");
  let addr = session.local_addr();

  // The controller's view of inbound traffic.
  let incoming = session.subscribe_to_incoming_messages();
  let presence = session.on_presence_change();

  // Connect a raw framed client.
  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let codec = JsonCodec;

  // The join fires as soon as the connection is registered.
  match with_timeout(presence.recv()).await.expect("presence event") {
    PresenceEvent::Joined { agent, .. } => assert_eq!(agent.id(), Some(&player_id)),
    other => panic!("expected a join, got {:?}", other),
  }

  // Client -> server: a kind tag, then the encoded ops.
  let op_frame = plaza_wire::frame::encode_ops(&codec, &[TestOp::Hello("hi".into())]).unwrap();
  client.send(op_frame.into()).await.expect("client send");

  let received = with_timeout(incoming.recv()).await.expect("incoming message");
  assert_eq!(received.from.id(), Some(&player_id));
  assert_eq!(received.ops, vec![TestOp::Hello("hi".into())]);

  // Server -> client: a broadcast should arrive encoded on the socket.
  session
    .send_message(
      MessageTarget::All,
      SessionMessage::system(vec![TestOp::Welcome("hello back".into())]),
    )
    .await
    .expect("broadcast");

  // The frame is one tag byte and one encoded document, so a client reads the
  // kind without parsing and then decodes the body in a single pass.
  let frame = with_timeout(client.next()).await.expect("frame").expect("frame ok");
  let (tag, body) = plaza_wire::frame::split(&frame).expect("a non-empty frame");
  assert_eq!(plaza_wire::frame::Kind::from_byte(tag), Some(plaza_wire::frame::Kind::Ops));
  let ops: Vec<TestOp> = codec.decode(body).expect("decode body");
  assert_eq!(ops, vec![TestOp::Welcome("hello back".into())]);

  // And the bytes themselves are readable: ops appear as objects, not as arrays
  // of byte values. Worth asserting on the wire rather than only through a
  // round trip, because a symmetric double-encoding would round trip fine and
  // still be unreadable to every other language.
  assert_eq!(frame.len(), body.len() + 1, "framing costs exactly one byte");
  let text = String::from_utf8(body.to_vec()).expect("json is utf-8");
  assert!(text.contains("Welcome"), "ops are readable in the document: {text}");
  assert!(!text.contains("[["), "no nested byte arrays: {text}");
}

/// Answers probes and reports ops, which is all a client has to do to be
/// measurable.
async fn answer_probes(
  client: &mut Framed<TcpStream, LengthDelimitedCodec>,
  frames: usize,
) -> Vec<Vec<u8>> {
  let mut others = Vec::new();
  for _ in 0..frames {
    let frame = with_timeout(client.next()).await.expect("frame").expect("frame ok");
    let (tag, body) = plaza_wire::frame::split(&frame).expect("non-empty");
    match plaza_wire::frame::Kind::from_byte(tag) {
      Some(plaza_wire::frame::Kind::Ping) => {
        let reply = plaza_wire::frame::answer_ping(&JsonCodec, body, Some(777)).expect("answerable");
        client.send(reply.into()).await.expect("pong");
      }
      _ => others.push(frame.to_vec()),
    }
  }
  others
}

/// Answers probes until the link has a measurement, then returns it.
///
/// A fixed frame count is not enough after a profile change: probes launched
/// under the old profile are deliberately discarded when their pong arrives,
/// and how many of those are sitting in the client's buffer depends on
/// scheduler timing, so any fixed budget can be spent entirely on answers the
/// server throws away. Bounded by `with_patience` like every other wait here.
async fn answer_until_measured(
  session: &TcpPlazaSession<TestOp, PlayerId>,
  id: &PlayerId,
  client: &mut Framed<TcpStream, LengthDelimitedCodec>,
) -> (Duration, u64) {
  with_patience(async {
    loop {
      if let Some(measured) = session.manager().agent_link_rtt(id) {
        return measured;
      }
      let arrived = tokio::time::timeout(Duration::from_millis(50), client.next()).await;
      if let Ok(Some(Ok(frame))) = arrived {
        let (tag, body) = plaza_wire::frame::split(&frame).expect("non-empty");
        if plaza_wire::frame::Kind::from_byte(tag) == Some(plaza_wire::frame::Kind::Ping) {
          let reply = plaza_wire::frame::answer_ping(&JsonCodec, body, Some(777)).expect("answerable");
          client.send(reply.into()).await.expect("pong");
        }
      }
    }
  })
  .await
}

#[tokio::test]
async fn a_tcp_connection_measures_a_round_trip_it_has_no_ping_frame_for() {
  // The gap this closes: the WebSocket transport times its own ping frame and
  // TCP has none, so before the probe frame existed `agent_rtt` was permanently
  // None here.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory)
      .await
      .expect("bind");
  let addr = session.local_addr();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());

  answer_probes(&mut client, 3).await;

  let (rtt, samples) = measured(&session, &player_id).await;
  assert!(samples > 0, "the server timed its own probe");
  assert!(rtt < Duration::from_secs(1), "a loopback round trip is fast: {rtt:?}");
}

#[tokio::test]
async fn an_impaired_link_delays_every_frame_including_the_probe() {
  // Impairment belongs to the link, so the measured round trip moves with it.
  // An application that kept its own delay queue could not show this, because
  // the probe would not be in the queue.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory)
      .await
      .expect("bind");
  let addr = session.local_addr();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());

  let one_way = Duration::from_millis(60);
  registered(&session).await;
  session.manager().set_agent_link_profile(
    &player_id,
    LinkProfile::symmetric(DirectionProfile::delayed(one_way)),
  );

  let (rtt, _) = answer_until_measured(&session, &player_id, &mut client).await;
  assert!(
    rtt >= one_way * 2,
    "a probe rides the impairment in both directions: {rtt:?}"
  );
}

#[tokio::test]
async fn a_probe_never_delays_what_the_application_sent() {
  // Control frames share the socket with ops, so the ordering worth asserting
  // is that inserting them does not disturb what the application queued.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory)
      .await
      .expect("bind");
  let addr = session.local_addr();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  registered(&session).await;

  for n in 0..4 {
    session
      .send_message(
        MessageTarget::All,
        SessionMessage::system(vec![TestOp::Welcome(format!("{n}"))]),
      )
      .await
      .expect("broadcast");
  }

  let mut seen = Vec::new();
  while seen.len() < 4 {
    for frame in answer_probes(&mut client, 1).await {
      let (_, body) = plaza_wire::frame::split(&frame).expect("non-empty");
      if let Ok(ops) = JsonCodec.decode::<Vec<TestOp>>(body) {
        seen.extend(ops);
      }
    }
  }

  let expected: Vec<TestOp> = (0..4).map(|n| TestOp::Welcome(format!("{n}"))).collect();
  assert_eq!(seen, expected, "ops arrive in order, probes interleaved or not");
}

#[tokio::test]
async fn bind_failure_is_reported_to_the_caller() {
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(|_peer| Ok(Agent::new_human(Uuid::new_v4())));

  let first: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory.clone())
      .await
      .expect("first bind");

  // Binding the same port again must surface an error rather than silently
  // killing a detached accept task.
  let result: Result<Arc<TcpPlazaSession<TestOp, PlayerId>>, _> =
    TcpPlazaSession::bind(first.local_addr().to_string(), agent_factory).await;

  assert!(result.is_err(), "re-binding a live port should fail");
}

#[tokio::test]
async fn a_hello_is_dispatched_as_a_version_and_an_unknown_kind_is_skipped() {
  // The two properties the kind byte exists for. Without dispatch it is a
  // reserved byte that never becomes anything; without skip-unknown, adding a
  // kind later breaks every deployed client.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind_with_protocol("127.0.0.1:0", agent_factory, JsonCodec, ProtocolVersion(42))
      .await
      .expect("bind");
  let addr = session.local_addr();
  let incoming = session.subscribe_to_incoming_messages();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let codec = JsonCodec;

  // The server speaks first: a client learns what it is talking to without
  // asking, which is what makes the handshake symmetric.
  let greeting = with_timeout(client.next()).await.expect("frame").expect("ok");
  let (tag, body) = plaza_wire::frame::split(&greeting).expect("non-empty");
  assert_eq!(plaza_wire::frame::Kind::from_byte(tag), Some(plaza_wire::frame::Kind::Hello));
  assert_eq!(codec.decode::<ProtocolVersion>(body).expect("version"), ProtocolVersion(42));

  // A Hello: decoded as a version, not as ops, and never reaches the controller.
  let mut hello = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Hello, &mut hello);
  codec.encode_into(&ProtocolVersion(42), &mut hello).unwrap();
  client.send(hello.into()).await.expect("send hello");

  // A kind this build has never heard of: skipped, connection survives.
  let unknown = vec![200u8, b'{', b'}'];
  client.send(unknown.into()).await.expect("send unknown");

  // Ops still arrive after both, which is what "skipped, not fatal" means.
  let mut ops = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut ops);
  codec.encode_into(&vec![TestOp::Hello("hi".into())], &mut ops).unwrap();
  client.send(ops.into()).await.expect("send ops");

  let received = with_timeout(incoming.recv()).await.expect("incoming");
  assert_eq!(
    received.ops,
    vec![TestOp::Hello("hi".into())],
    "the ops frame is the only one the controller sees"
  );
  assert_eq!(
    session.manager().protocol(&player_id),
    Some(ProtocolVersion(42)),
    "the Hello was decoded as a version and recorded"
  );
}

#[tokio::test]
async fn a_disagreeing_client_is_reported_and_left_connected() {
  // A version is a build hash, so a peer that merely recompiled is
  // indistinguishable here from one whose shapes changed and this layer cannot
  // tell which it has. So it records the number and keeps serving; whether the
  // mismatch is fatal, cosmetic or worth telling the client to reload is the
  // application's and this is where it reads it.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind_with_protocol("127.0.0.1:0", agent_factory, JsonCodec, ProtocolVersion(42))
      .await
      .expect("bind");
  let addr = session.local_addr();
  let incoming = session.subscribe_to_incoming_messages();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let codec = JsonCodec;
  let _greeting = with_timeout(client.next()).await.expect("frame").expect("ok");

  assert_eq!(
    session.manager().protocol(&player_id),
    None,
    "declaring nothing is not a mismatch, it is every client older than the handshake"
  );

  let mut hello = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Hello, &mut hello);
  codec.encode_into(&ProtocolVersion(7), &mut hello).unwrap();
  client.send(hello.into()).await.expect("send hello");

  let mut ops = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut ops);
  codec.encode_into(&vec![TestOp::Hello("still here".into())], &mut ops).unwrap();
  client.send(ops.into()).await.expect("send ops");

  let received = with_timeout(incoming.recv()).await.expect("incoming");
  assert_eq!(
    received.ops,
    vec![TestOp::Hello("still here".into())],
    "a disagreeing client is still served: refusing it here would drop clients that are fine"
  );
  assert_eq!(
    session.manager().protocol(&player_id),
    Some(ProtocolVersion(7)),
    "and the application can read what it actually said, which is the whole mechanism"
  );
}

#[tokio::test]
async fn a_link_slower_than_the_probe_interval_is_still_measured() {
    // The probe schedule is 125ms in its fast phase. A round trip longer than
    // that means a pong lands after the next probe went out, so a single
    // outstanding slot discards every sample and such a link is never measured.
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let addr = session.local_addr();
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(addr).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  registered(&session).await;

  let one_way = Duration::from_millis(100);
  session.manager().set_agent_link_profile(
    &player_id,
    LinkProfile::symmetric(DirectionProfile::delayed(one_way)),
  );

  let (rtt, _) = answer_until_measured(&session, &player_id, &mut client).await;
  assert!(rtt >= one_way * 2, "a 200ms link measures 200ms, not nothing: {rtt:?}");
}

#[tokio::test]
async fn a_refused_socket_hears_the_farewell_and_registers_nothing() {
  let admitted = Uuid::new_v4();
  let admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> = {
    let admissions = admissions.clone();
    Arc::new(move |_peer| {
      if admissions.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
        Ok(Agent::new_human(admitted))
      } else {
        Err(Farewell::new(4403).with_detail(b"full".to_vec()))
      }
    })
  };

  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let addr = session.local_addr();
  let _presence = session.on_presence_change();

  let first = TcpStream::connect(addr).await.expect("connect");
  let _first = Framed::new(first, LengthDelimitedCodec::new());
  registered(&session).await;

  let second = TcpStream::connect(addr).await.expect("connect");
  let mut second = Framed::new(second, LengthDelimitedCodec::new());

  let frame = with_timeout(second.next()).await.expect("a goodbye").expect("frame ok");
  let goodbye = plaza_wire::frame::decode_goodbye(&JsonCodec, &frame).expect("a goodbye frame");
  assert_eq!(goodbye.code, 4403);
  assert_eq!(goodbye.detail.as_deref(), Some(&b"full"[..]));

  assert!(
    with_timeout(second.next()).await.is_none(),
    "the refused socket closes after the goodbye"
  );
  assert_eq!(session.manager().connection_count(), 1, "nothing was registered for it");
  assert_eq!(session.manager().stats().refused(), 1);
}

/// Reads frames until an `Ops` frame arrives, skipping probes.
async fn next_ops_frame(
  client: &mut Framed<TcpStream, LengthDelimitedCodec>,
) -> Option<Vec<TestOp>> {
  loop {
    let frame = with_timeout(client.next()).await?.ok()?;
    let (tag, body) = plaza_wire::frame::split(&frame)?;
    if plaza_wire::frame::Kind::from_byte(tag) == Some(plaza_wire::frame::Kind::Ops) {
      return JsonCodec.decode(body).ok();
    }
  }
}

/// Reads frames until a `Goodbye` arrives, skipping probes and ops.
async fn next_goodbye(client: &mut Framed<TcpStream, LengthDelimitedCodec>) -> Option<Goodbye> {
  loop {
    let frame = with_timeout(client.next()).await?.ok()?;
    if let Some(goodbye) = plaza_wire::frame::decode_goodbye(&JsonCodec, &frame) {
      return Some(goodbye);
    }
  }
}

#[tokio::test]
async fn a_close_delivers_the_goodbye_then_ends_the_session() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());

  let conn_id = match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { agent, conn_id } => {
      assert_eq!(agent.id(), Some(&player_id));
      conn_id
    }
    other => panic!("expected a join, got {other:?}"),
  };
  assert_eq!(
    session.manager().connections_of(&player_id),
    vec![conn_id],
    "the id on the event is the one the registry resolves"
  );

  assert!(session.manager().close_connection(conn_id, Farewell::new(4000).with_detail(b"removed by the host".to_vec())));

  let heard = next_goodbye(&mut client).await.expect("the goodbye");
  assert_eq!(heard.code, 4000);
  assert_eq!(heard.detail.as_deref(), Some(&b"removed by the host"[..]));
  loop {
    match with_timeout(client.next()).await {
      None => break,
      Some(Err(_)) => break,
      Some(Ok(_)) => continue,
    }
  }

  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Left { agent_id, conn_id: left } => {
      assert_eq!(agent_id, player_id);
      assert_eq!(left, conn_id, "the departure names the connection that closed");
    }
    other => panic!("expected a leave, got {other:?}"),
  }
  assert!(session.manager().connections_of(&player_id).is_empty());
  assert!(
    !session.manager().close_connection(conn_id, Farewell::new(1000)),
    "a second close finds nobody to order"
  );
}

#[tokio::test]
async fn a_close_with_no_detail_still_closes() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let conn_id = match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { conn_id, .. } => conn_id,
    other => panic!("expected a join, got {other:?}"),
  };

  assert!(session.manager().close_connection(conn_id, Farewell::new(1000)));
  loop {
    match with_timeout(client.next()).await {
      None | Some(Err(_)) => break,
      Some(Ok(_)) => continue,
    }
  }
  assert_eq!(session.manager().connection_count(), 0);
}

#[tokio::test]
async fn deregister_alone_is_bookkeeping_not_a_close() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let conn_id = match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { conn_id, .. } => conn_id,
    other => panic!("expected a join, got {other:?}"),
  };

  session.manager().deregister(conn_id).await;
  assert_eq!(session.manager().connection_count(), 0, "the registry forgot it");

  // The socket is still the task's: it keeps probing, so the client hears
  // frames rather than an EOF.
  match tokio::time::timeout(Duration::from_millis(500), client.next()).await {
    Ok(None) | Ok(Some(Err(_))) => panic!("deregister must not close the socket"),
    Ok(Some(Ok(_))) | Err(_) => {}
  }
}

#[tokio::test]
async fn deregister_agent_closes_every_connection_the_agent_holds() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let first = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut first = Framed::new(first, LengthDelimitedCodec::new());
  let second = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut second = Framed::new(second, LengthDelimitedCodec::new());
  for _ in 0..2 {
    with_timeout(presence.recv()).await.expect("join");
  }
  assert_eq!(session.manager().connections_of(&player_id).len(), 2);

  assert_eq!(session.manager().deregister_agent(&player_id, Farewell::new(4001)), 2);

  for client in [&mut first, &mut second] {
    assert_eq!(next_goodbye(client).await.expect("goodbye").code, 4001);
    loop {
      match with_timeout(client.next()).await {
        None | Some(Err(_)) => break,
        Some(Ok(_)) => continue,
      }
    }
  }
  for _ in 0..2 {
    match with_timeout(presence.recv()).await.expect("leave") {
      PresenceEvent::Left { agent_id, .. } => assert_eq!(agent_id, player_id),
      other => panic!("expected a leave, got {other:?}"),
    }
  }
  assert!(session.manager().connections_of(&player_id).is_empty());
}

#[tokio::test]
async fn disconnect_all_is_the_same_close_for_everyone() {
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(|_peer| Ok(Agent::new_human(Uuid::new_v4())));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let mut clients = Vec::new();
  for _ in 0..3 {
    let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
    clients.push(Framed::new(stream, LengthDelimitedCodec::new()));
    with_timeout(presence.recv()).await.expect("join");
  }

  assert_eq!(session.manager().disconnect_all(Farewell::new(1001).with_detail(b"room closed".to_vec())), 3);

  for client in &mut clients {
    let goodbye = next_goodbye(client).await.expect("goodbye");
    assert_eq!((goodbye.code, goodbye.detail.as_deref()), (1001, Some(&b"room closed"[..])));
    loop {
      match with_timeout(client.next()).await {
        None | Some(Err(_)) => break,
        Some(Ok(_)) => continue,
      }
    }
  }
  with_patience(async {
    while session.manager().connection_count() > 0 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await;
}

#[tokio::test]
async fn activity_counts_data_and_never_probes() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();
  let _incoming = session.subscribe_to_incoming_messages();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let conn_id = match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { conn_id, .. } => conn_id,
    other => panic!("expected a join, got {other:?}"),
  };

  // A connection that only answers probes is idle: the round trips are being
  // measured, and the seat is still silent.
  answer_probes(&mut client, 3).await;
  let idle_before = session.manager().idle_for(conn_id).expect("live");
  assert!(
    idle_before >= Duration::from_millis(200),
    "probe traffic moved the activity stamp: {idle_before:?}"
  );
  let volume = session.manager().connection_inbound(conn_id).expect("live");
  assert_eq!(volume.frames, 0, "pongs are not data");

  let mut op_frame = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut op_frame);
  JsonCodec
    .encode_into(&vec![TestOp::Hello("still here".into())], &mut op_frame)
    .unwrap();
  let sent = op_frame.len();
  client.send(op_frame.into()).await.expect("send");

  let volume = with_patience(async {
    loop {
      let volume = session.manager().connection_inbound(conn_id).expect("live");
      if volume.frames > 0 {
        return volume;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await;
  assert_eq!(volume.frames, 1);
  assert_eq!(volume.bytes, sent as u64);

  let idle_after = session.manager().idle_for(conn_id).expect("live");
  assert!(idle_after < idle_before, "one op moved the stamp: {idle_after:?}");
  let agent_idle = session.manager().agent_idle_for(&player_id).expect("connected");
  assert!(agent_idle <= idle_after + Duration::from_millis(50));
  assert_eq!(session.manager().agent_inbound(&player_id), volume);
}

#[tokio::test]
async fn a_deadline_closes_unless_renewed() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Ok(Agent::new_human(player_id)));
  let session: Arc<TcpPlazaSession<TestOp, PlayerId>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory).await.expect("bind");
  let presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let conn_id = match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { conn_id, .. } => conn_id,
    other => panic!("expected a join, got {other:?}"),
  };

  let farewell = Farewell::new(4002).with_detail(b"credit spent".to_vec());
  assert!(session.manager().set_deadline(conn_id, Some(Duration::from_millis(700)), farewell.clone()));

  // A renewal half way through replaces the deadline, so the original expiry
  // passes with the session still up.
  tokio::time::sleep(Duration::from_millis(350)).await;
  assert!(session.manager().set_deadline(conn_id, Some(Duration::from_millis(700)), farewell));
  tokio::time::sleep(Duration::from_millis(450)).await;
  assert_eq!(session.manager().connection_count(), 1, "renewed past the first expiry");

  let heard = with_patience(next_goodbye(&mut client)).await.expect("the goodbye");
  assert_eq!(heard.code, 4002);
  assert_eq!(heard.detail.as_deref(), Some(&b"credit spent"[..]));
  loop {
    match with_timeout(client.next()).await {
      None | Some(Err(_)) => break,
      Some(Ok(_)) => continue,
    }
  }
  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Left { agent_id, .. } => assert_eq!(agent_id, player_id),
    other => panic!("expected a leave, got {other:?}"),
  }
}

struct Doorman {
  id: PlayerId,
}

#[async_trait::async_trait]
impl ConnectionAdmitter<PlayerId> for Doorman {
  async fn admit(&self, credential: &[u8], peer: &Peer) -> ConnectionAdmission<PlayerId> {
    assert!(peer.addr.is_some(), "the transport knows the socket's address");
    if credential == b"open sesame" {
      ConnectionAdmission::Admitted(Agent::new_human(self.id))
    } else {
      ConnectionAdmission::Refused(Farewell::new(4403).with_detail(b"not you".to_vec()))
    }
  }
}

async fn admitted_session(options: plaza_session::SessionOptions) -> (Arc<TcpPlazaSession<TestOp, PlayerId>>, PlayerId) {
  let id = Uuid::new_v4();
  let session = TcpPlazaSession::bind_with_admitter("127.0.0.1:0", Arc::new(Doorman { id }), JsonCodec, options)
    .await
    .expect("bind");
  (session, id)
}

fn raw_frame(kind: Kind, body: &[u8]) -> bytes::Bytes {
  let mut buf = Vec::new();
  plaza_wire::frame::begin(kind, &mut buf);
  buf.extend_from_slice(body);
  buf.into()
}

fn hello_frame(version: u32) -> bytes::Bytes {
  let mut buf = Vec::new();
  plaza_wire::frame::begin(Kind::Hello, &mut buf);
  JsonCodec.encode_into(&ProtocolVersion(version), &mut buf).unwrap();
  buf.into()
}

#[tokio::test]
async fn a_socket_registers_only_once_its_credential_is_admitted() {
  let (session, id) = admitted_session(plaza_session::SessionOptions::default()).await;
  let presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  client.send(hello_frame(9)).await.unwrap();
  tokio::time::sleep(Duration::from_millis(100)).await;
  assert_eq!(session.manager().connection_count(), 0, "a hello alone registers nothing");
  assert_eq!(session.manager().stats().pending(), 1);

  client.send(raw_frame(Kind::Credential, b"open sesame")).await.unwrap();
  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { agent, .. } => assert_eq!(agent.id(), Some(&id)),
    other => panic!("expected a join, got {other:?}"),
  }
  assert_eq!(session.manager().connection_count(), 1);
  assert_eq!(session.manager().stats().pending(), 0);
  assert_eq!(session.manager().stats().admitted(), 1);
  assert_eq!(
    session.manager().protocol(&id),
    Some(ProtocolVersion(9)),
    "the hello heard while pending is recorded on admission"
  );
}

#[tokio::test]
async fn a_refused_credential_hears_a_goodbye_and_registers_nothing() {
  let (session, _) = admitted_session(plaza_session::SessionOptions::default()).await;
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  client.send(raw_frame(Kind::Credential, b"wrong")).await.unwrap();

  let goodbye = next_goodbye(&mut client).await.expect("a goodbye");
  assert_eq!(goodbye.code, 4403);
  assert_eq!(goodbye.detail.as_deref(), Some(&b"not you"[..]));
  assert!(with_timeout(client.next()).await.is_none(), "closed after the goodbye");
  assert_eq!(session.manager().connection_count(), 0);
  assert_eq!(session.manager().stats().refused(), 1);
  assert_eq!(session.manager().stats().pending(), 0);
}

#[tokio::test]
async fn ops_before_the_credential_close_the_socket() {
  let (session, _) = admitted_session(plaza_session::SessionOptions::default()).await;
  let _presence = session.on_presence_change();

  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  let mut ops = Vec::new();
  plaza_wire::frame::begin(Kind::Ops, &mut ops);
  JsonCodec.encode_into(&vec![TestOp::Hello("early".into())], &mut ops).unwrap();
  client.send(ops.into()).await.unwrap();

  assert_eq!(next_goodbye(&mut client).await.expect("a goodbye").code, Goodbye::CREDENTIAL_EXPECTED);
  assert!(with_timeout(client.next()).await.is_none());
  assert_eq!(session.manager().connection_count(), 0);
}

#[tokio::test]
async fn a_socket_that_presents_nothing_times_out() {
  let mut options = plaza_session::SessionOptions::default();
  options.limits.credential_timeout = Duration::from_millis(200);
  let (session, _) = admitted_session(options).await;
  let _presence = session.on_presence_change();

  // Timed from before the connect: the server's timer cannot start earlier
  // than that, so a goodbye inside the timeout means the timer fired early.
  let started = std::time::Instant::now();
  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut client = Framed::new(stream, LengthDelimitedCodec::new());
  assert_eq!(next_goodbye(&mut client).await.expect("a goodbye").code, Goodbye::CREDENTIAL_TIMEOUT);
  assert!(started.elapsed() >= Duration::from_millis(200), "goodbye after {:?}", started.elapsed());
  assert!(with_timeout(client.next()).await.is_none());
  assert_eq!(session.manager().stats().timed_out(), 1);
  assert_eq!(session.manager().stats().pending(), 0);
}

#[tokio::test]
async fn the_pending_cap_turns_a_socket_away_before_reading_anything() {
  let mut options = plaza_session::SessionOptions::default();
  options.limits.pending_connections = Some(1);
  let (session, id) = admitted_session(options).await;
  let presence = session.on_presence_change();

  let first = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut first = Framed::new(first, LengthDelimitedCodec::new());
  with_patience(async {
    while session.manager().stats().pending() < 1 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await;

  let second = TcpStream::connect(session.local_addr()).await.expect("connect");
  let mut second = Framed::new(second, LengthDelimitedCodec::new());
  assert!(
    with_timeout(second.next()).await.is_none(),
    "over the cap the socket is closed with nothing on it"
  );
  assert_eq!(session.manager().stats().over_cap(), 1);

  first.send(raw_frame(Kind::Credential, b"open sesame")).await.unwrap();
  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { agent, .. } => assert_eq!(agent.id(), Some(&id)),
    other => panic!("expected a join, got {other:?}"),
  }
  assert_eq!(session.manager().stats().pending(), 0, "the slot is free once admitted");
}
