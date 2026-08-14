//! What a flood costs, over a real socket.
//!
//! The unit tests in `gate` walk the bucket past a clock it is handed. This
//! walks it past a client, which is the only way to check the part that
//! matters: that a shed frame never reaches the controller's subscription, and
//! that the connection carrying it is the only one paying.

#![cfg(feature = "tcp")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::SinkExt;
use plaza::agent::Agent;
use plaza::session::{PresenceEvent, Session};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::{Rate, SessionOptions, TcpPlazaSession};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Seat = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Op {
  Shout(u32),
}

async fn with_timeout<T>(fut: impl std::future::Future<Output = T>) -> T {
  tokio::time::timeout(Duration::from_secs(5), fut)
    .await
    .expect("timed out waiting for the transport")
}

async fn session_limited_to(rate: Rate) -> Arc<TcpPlazaSession<Op, Seat>> {
  let next_seat = Arc::new(AtomicU32::new(1));
  let agent_factory: plaza_session::tcp::AgentFactory<Seat> =
    Arc::new(move |_peer| Ok(Agent::new_human(next_seat.fetch_add(1, Ordering::Relaxed))));
  TcpPlazaSession::bind_with_options(
    "127.0.0.1:0",
    agent_factory,
    JsonCodec,
    SessionOptions::default().rate_limit_inbound(rate),
  )
  .await
  .expect("bind on an ephemeral port")
}

async fn connect(session: &TcpPlazaSession<Op, Seat>) -> Framed<TcpStream, LengthDelimitedCodec> {
  let stream = TcpStream::connect(session.local_addr()).await.expect("connect");
  Framed::new(stream, LengthDelimitedCodec::new())
}

fn op_frame(n: u32) -> bytes::Bytes {
  let mut buf = Vec::new();
  plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut buf);
  JsonCodec.encode_into(&vec![Op::Shout(n)], &mut buf).expect("encode");
  buf.into()
}

/// Sends `count` frames as fast as the socket takes them.
async fn flood(client: &mut Framed<TcpStream, LengthDelimitedCodec>, count: u32) {
  for n in 0..count {
    client.send(op_frame(n)).await.expect("send");
  }
}

/// Blocks until this connection has been charged for `frames` inbound frames,
/// so an assertion about what was shed is not racing the reader task.
async fn charged(session: &TcpPlazaSession<Op, Seat>, conn_id: plaza::session::ConnectionId, frames: u64) {
  with_timeout(async {
    loop {
      if session
        .manager()
        .connection_inbound(conn_id)
        .is_some_and(|volume| volume.frames >= frames)
      {
        return;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await;
}

async fn joined(presence: &plaza::session::SessionReceiver<PresenceEvent<Seat>>) -> plaza::session::ConnectionId {
  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Joined { conn_id, .. } => conn_id,
    other => panic!("expected a join, got {other:?}"),
  }
}

#[tokio::test]
async fn a_flood_is_shed_at_the_burst_and_the_connection_stays() {
  let session = session_limited_to(Rate::per_second(4.0).burst(8)).await;
  let presence = session.on_presence_change();
  let incoming = session.subscribe_to_incoming_messages();

  let mut client = connect(&session).await;
  let conn_id = joined(&presence).await;

  flood(&mut client, 40).await;
  charged(&session, conn_id, 40).await;

  let volume = session.manager().connection_inbound(conn_id).expect("live");
  assert_eq!(volume.frames, 40, "every frame arrived and was counted");
  assert!(
    volume.shed >= 30,
    "a burst of 8 at 4/sec cannot pay for 40 frames in a moment: shed {}",
    volume.shed
  );
  assert_eq!(
    volume.frames - volume.shed,
    session.manager().stats().inbound(),
    "what was not shed is exactly what the controller was offered"
  );
  assert_eq!(session.manager().stats().inbound_shed(), volume.shed);

  // The connection is still up and still speaks, once its bucket refills.
  assert_eq!(session.manager().connection_count(), 1);
  tokio::time::sleep(Duration::from_millis(600)).await;
  client.send(op_frame(99)).await.expect("send");
  let heard = with_timeout(incoming.recv()).await.expect("delivery");
  assert!(heard.ops.iter().any(|op| matches!(op, Op::Shout(_))));
}

#[tokio::test]
async fn a_shed_frame_never_reaches_the_controller() {
  // The whole point of judging before the shared queue: not that the flooder is
  // punished, but that the ops behind it are not.
  let session = session_limited_to(Rate::per_second(1.0).burst(2)).await;
  let presence = session.on_presence_change();
  let incoming = session.subscribe_to_incoming_messages();

  let mut client = connect(&session).await;
  let conn_id = joined(&presence).await;

  flood(&mut client, 20).await;
  charged(&session, conn_id, 20).await;

  let mut delivered = 0;
  while incoming.try_recv().is_ok() {
    delivered += 1;
  }
  assert!(
    delivered <= 3,
    "a burst of 2 at 1/sec should not deliver 20 batches: {delivered}"
  );
}

#[tokio::test]
async fn a_disconnecting_rate_ends_the_connection_that_exceeded_it() {
  let session = session_limited_to(Rate::per_second(2.0).burst(4).disconnecting()).await;
  let presence = session.on_presence_change();
  let _incoming = session.subscribe_to_incoming_messages();

  let mut client = connect(&session).await;
  let _conn_id = joined(&presence).await;

  // The socket may close under the writer part way through, which is the point.
  for n in 0..40 {
    if client.send(op_frame(n)).await.is_err() {
      break;
    }
  }

  match with_timeout(presence.recv()).await.expect("presence") {
    PresenceEvent::Left { .. } => {}
    other => panic!("expected a leave, got {other:?}"),
  }
}

#[tokio::test]
async fn one_connection_flooding_does_not_spend_another_s_budget() {
  // A per-connection bucket is the difference between a rate limit and a
  // session-wide throttle that the loudest client sets for everybody.
  let session = session_limited_to(Rate::per_second(4.0).burst(8)).await;
  let presence = session.on_presence_change();
  let _incoming = session.subscribe_to_incoming_messages();

  let mut loud = connect(&session).await;
  let loud_conn = joined(&presence).await;
  let mut quiet = connect(&session).await;
  let quiet_conn = joined(&presence).await;

  flood(&mut loud, 40).await;
  charged(&session, loud_conn, 40).await;

  quiet.send(op_frame(1)).await.expect("send");
  charged(&session, quiet_conn, 1).await;

  assert!(session.manager().connection_inbound(loud_conn).expect("live").shed > 0);
  assert_eq!(
    session.manager().connection_inbound(quiet_conn).expect("live").shed,
    0,
    "the quiet client pays nothing for the loud one"
  );
}

#[tokio::test]
async fn without_a_rate_nothing_is_shed() {
  let next_seat = Arc::new(AtomicU32::new(1));
  let agent_factory: plaza_session::tcp::AgentFactory<Seat> =
    Arc::new(move |_peer| Ok(Agent::new_human(next_seat.fetch_add(1, Ordering::Relaxed))));
  let session: Arc<TcpPlazaSession<Op, Seat>> =
    TcpPlazaSession::bind_with_options("127.0.0.1:0", agent_factory, JsonCodec, SessionOptions::default())
      .await
      .expect("bind");
  let presence = session.on_presence_change();
  let _incoming = session.subscribe_to_incoming_messages();

  let mut client = connect(&session).await;
  let conn_id = joined(&presence).await;
  flood(&mut client, 40).await;
  charged(&session, conn_id, 40).await;

  assert_eq!(session.manager().connection_inbound(conn_id).expect("live").shed, 0);
  assert_eq!(session.manager().stats().inbound_shed(), 0);
}
