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
use plaza_session::tcp::TcpPlazaSession;
use plaza_session::SessionOptions;

use crate::door::{Door, Doorman};
use crate::logic::{ArcadeLogic, ArcadeState};
use crate::snapshot::RoomSnapshotter;
use crate::types::{AgentKey, ArcadeOp, DuplicateLogin, CREDENTIAL_WAIT, PER_IP};

pub type Arcade = TcpPlazaSession<ArcadeOp, AgentKey>;

/// The whole arcade: the shipped TCP transport with the door as its admitter.
/// No transport is written here, on purpose.
pub async fn arcade(policy: DuplicateLogin) -> (Arc<Arcade>, Arc<Door>) {
  arcade_with(policy, PER_IP).await
}

/// As [`arcade`], with the address cap chosen: every client on one machine
/// shares an address, so showing that rule needs a cap below the seat count.
pub async fn arcade_with(policy: DuplicateLogin, per_ip: usize) -> (Arc<Arcade>, Arc<Door>) {
  let door = Door::with_per_ip(policy, per_ip);
  let doorman = Arc::new(Doorman::new(door.clone()));

  let mut options = SessionOptions::default();
  options.limits.credential_timeout = CREDENTIAL_WAIT;
  let session = Arcade::bind_with_admitter("127.0.0.1:0", doorman.clone(), JsonCodec, options)
    .await
    .expect("bind");
  doorman.attach(session.manager().clone());

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
