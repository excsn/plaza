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
use plaza_session::TcpPlazaSession;
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

#[tokio::test]
async fn tcp_client_op_reaches_controller_and_broadcast_reaches_client() {
  let player_id = Uuid::new_v4();
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(move |_peer| Agent::new_human(player_id));

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
    PresenceEvent::Joined(agent) => assert_eq!(agent.id(), Some(&player_id)),
    other => panic!("expected a join, got {:?}", other),
  }

  // Client -> server: a kind tag, then the encoded ops.
  let mut op_frame = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut op_frame);
  codec.encode_into(&vec![TestOp::Hello("hi".into())], &mut op_frame).unwrap();
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

#[tokio::test]
async fn bind_failure_is_reported_to_the_caller() {
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(|_peer| Agent::new_human(Uuid::new_v4()));

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
    Arc::new(move |_peer| Agent::new_human(player_id));

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
