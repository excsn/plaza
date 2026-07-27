//! End-to-end check of the TCP transport: a framed client's op reaches the
//! controller's subscription, and a server broadcast reaches the client.

#![cfg(feature = "tcp")]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plaza::agent::Agent;
use plaza::session::{MessageTarget, PresenceEvent, Session, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestSnapshot {
  value: u32,
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

  let session: Arc<TcpPlazaSession<TestOp, PlayerId, TestSnapshot>> =
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

  // Client -> server: one encoded op per frame.
  let op_bytes = codec.encode(&TestOp::Hello("hi".into())).unwrap();
  client.send(op_bytes.into()).await.expect("client send");

  let received = with_timeout(incoming.recv()).await.expect("incoming message");
  match received {
    SessionMessage::Ops { from, ops } => {
      assert_eq!(from.id(), Some(&player_id));
      assert_eq!(ops, vec![TestOp::Hello("hi".into())]);
    }
    other => panic!("expected Ops, got {:?}", other),
  }

  // Server -> client: a broadcast should arrive encoded on the socket.
  session
    .send_message(
      MessageTarget::All,
      SessionMessage::Ops {
        from: Agent::system(),
        ops: vec![TestOp::Welcome("hello back".into())],
      },
    )
    .await
    .expect("broadcast");

  // The envelope is encoded **once**, as a whole, so one decode yields typed ops
  // rather than byte arrays that need a second pass. That is what lets a non-Rust
  // client read the document, and what pong's browser client has always assumed.
  let frame = with_timeout(client.next()).await.expect("frame").expect("frame ok");
  let msg: SessionMessage<TestOp, PlayerId, TestSnapshot> = codec.decode(&frame).expect("decode envelope");
  match msg {
    SessionMessage::Ops { ops, .. } => assert_eq!(ops, vec![TestOp::Welcome("hello back".into())]),
    other => panic!("expected Ops, got {:?}", other),
  }

  // And the bytes themselves are readable: ops appear as objects, not as arrays
  // of byte values. Worth asserting on the wire rather than only through a
  // round trip, because a symmetric double-encoding would round trip fine and
  // still be unreadable to every other language.
  let text = String::from_utf8(frame.to_vec()).expect("json is utf-8");
  assert!(text.contains("Welcome"), "ops are readable in the document: {text}");
  assert!(!text.contains("[["), "no nested byte arrays: {text}");
}

#[tokio::test]
async fn bind_failure_is_reported_to_the_caller() {
  let agent_factory: plaza_session::tcp::AgentFactory<PlayerId> =
    Arc::new(|_peer| Agent::new_human(Uuid::new_v4()));

  let first: Arc<TcpPlazaSession<TestOp, PlayerId, TestSnapshot>> =
    TcpPlazaSession::bind("127.0.0.1:0", agent_factory.clone())
      .await
      .expect("first bind");

  // Binding the same port again must surface an error rather than silently
  // killing a detached accept task.
  let result: Result<Arc<TcpPlazaSession<TestOp, PlayerId, TestSnapshot>>, _> =
    TcpPlazaSession::bind(first.local_addr().to_string(), agent_factory).await;

  assert!(result.is_err(), "re-binding a live port should fail");
}
