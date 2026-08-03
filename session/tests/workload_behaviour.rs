//! Whether a preset does what its name claims.
//!
//! `Workload` derives depths and policies, and the depths are measured in
//! `benches/saturation.rs`. What is left is the behaviour those policies buy,
//! which is a claim about outcomes rather than about numbers: a preset whose
//! label cannot be asserted is decoration.

#![cfg(feature = "tcp")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::SinkExt;
use plaza::agent::Agent;
use plaza::session::{MessageTarget, Session, SessionMessage};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::{SessionOptions, TcpPlazaSession, Workload};
use plaza_wire::frame;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Seat = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Op(String);

async fn bind(workload: &Workload) -> Arc<TcpPlazaSession<Op, Seat>> {
  let next_seat = Arc::new(AtomicU32::new(1));
  let agent_factory: plaza_session::tcp::AgentFactory<Seat> =
    Arc::new(move |_peer| Agent::new_human(next_seat.fetch_add(1, Ordering::Relaxed)));
  TcpPlazaSession::bind_with_options(
    "127.0.0.1:0",
    agent_factory,
    JsonCodec,
    SessionOptions::default().workload(workload),
  )
  .await
  .expect("bind on an ephemeral port")
}

async fn one_connection(session: &TcpPlazaSession<Op, Seat>) {
  for _ in 0..200 {
    if session.manager().connection_count() == 1 {
      return;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  panic!("the connection never registered");
}

fn ops_frame(payload: usize) -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  JsonCodec
    .encode_into(&vec![Op("x".repeat(payload))], &mut buf)
    .expect("an op encodes");
  buf
}

/// Sends until the socket stops accepting, which under backpressure is the
/// point of the exercise, so it is bounded by a timeout rather than a count.
async fn push_until_stalled(client: &mut Framed<TcpStream, LengthDelimitedCodec>, frame: Vec<u8>, count: usize) {
  let _ = tokio::time::timeout(Duration::from_secs(2), async {
    for _ in 0..count {
      if client.send(frame.clone().into()).await.is_err() {
        return;
      }
    }
  })
  .await;
}

/// Broadcasts until the client is gone or the budget is spent.
async fn flood(session: &TcpPlazaSession<Op, Seat>, payload: usize, count: usize) {
  for _ in 0..count {
    if session.manager().connection_count() == 0 {
      return;
    }
    let _ = session
      .send_message(
        MessageTarget::All,
        SessionMessage::system(vec![Op("x".repeat(payload))]),
      )
      .await;
  }
}

#[tokio::test]
async fn turn_based_loses_no_ops_where_local_drops() {
  let cases = [
    ("turn_based", Workload::turn_based(), 0u64),
    ("local", Workload::local(), 1),
  ];

  for (name, workload, want_drops) in cases {
    let session = bind(&workload).await;
    let addr = session.local_addr();
    // Nothing subscribes, so the controller's queues fill and stay full.
    let stream = TcpStream::connect(addr).await.expect("connect");
    let mut client = Framed::new(stream, LengthDelimitedCodec::new());
    one_connection(&session).await;

    push_until_stalled(&mut client, ops_frame(256), 4_000).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let dropped = session.manager().stats().inbound_dropped();
    match want_drops {
      0 => assert_eq!(dropped, 0, "{name} lost ops a client believes arrived"),
      _ => assert!(dropped > 0, "{name} was expected to drop under a burst it cannot hold"),
    }
  }
}

#[tokio::test]
async fn turn_based_ends_a_client_it_cannot_reach_and_action_keeps_it() {
  let session = bind(&Workload::turn_based()).await;
  let addr = session.local_addr();
  let _silent = TcpStream::connect(addr).await.expect("connect");
  one_connection(&session).await;

  flood(&session, Workload::turn_based().max_payload, 2_000).await;
  tokio::time::sleep(Duration::from_millis(400)).await;
  assert_eq!(
    session.manager().connection_count(),
    0,
    "a client that cannot be delivered to holds a view the server never authored"
  );

  let session = bind(&Workload::action()).await;
  let addr = session.local_addr();
  let _silent = TcpStream::connect(addr).await.expect("connect");
  one_connection(&session).await;

  flood(&session, Workload::action().max_payload, 8_000).await;
  tokio::time::sleep(Duration::from_millis(400)).await;
  assert_eq!(
    session.manager().connection_count(),
    1,
    "a superseded frame is not a reason to end a connection"
  );
  assert!(
    session.manager().stats().outbound_dropped() > 0,
    "the frames went somewhere, and it was not the client"
  );
}
