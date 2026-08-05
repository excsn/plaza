//! What it costs a server to be allowed to say no.
//!
//! The door and the arcade behind it are a library so the tests can knock on a
//! real socket, which is the only way to assert that a refusal arrived and a
//! close actually closed.

pub mod client;
pub mod door;
pub mod logic;
pub mod snapshot;
pub mod types;

use std::sync::Arc;

use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_session::codec::JsonCodec;
use plaza_session::tcp::{AgentFactory, TcpPlazaSession};
use plaza_session::SessionOptions;

use crate::door::Door;
use crate::logic::{ArcadeLogic, ArcadeState};
use crate::snapshot::RoomSnapshotter;
use crate::types::{op_frame, AgentKey, ArcadeOp, DuplicateLogin};

pub type Arcade = TcpPlazaSession<ArcadeOp, AgentKey>;

/// The whole arcade: the shipped TCP transport with the door's rules plugged
/// into its seams. No transport is written here, which is the point.
pub async fn arcade(policy: DuplicateLogin) -> (Arc<Arcade>, Arc<Door>) {
  let door = Door::new(policy);

  // The socket rule lives in the factory: judged before anything is
  // registered, with the reason riding the refusal.
  let factory: AgentFactory<AgentKey> = {
    let door = door.clone();
    Arc::new(move |peer| {
      door
        .knock(peer)
        .map(plaza::agent::Agent::new_human)
        .map_err(|reason| plaza_session::tcp::Refusal::saying(op_frame(ArcadeOp::Refused { reason })))
    })
  };

  let session = Arcade::bind_with_options("127.0.0.1:0", factory, JsonCodec, SessionOptions::default())
    .await
    .expect("bind");

  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(ArcadeLogic {
      door: door.clone(),
      manager: session.manager().clone(),
    }),
    session.clone(),
    Arc::new(RoomSnapshotter),
    ArcadeState::default(),
  )
  .build();
  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::new(std::time::Duration::from_millis(50)).run(tx));

  (session, door)
}
