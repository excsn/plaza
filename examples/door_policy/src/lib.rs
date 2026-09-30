//! What it costs a server to be allowed to say no.
//!
//! The door and the arcade behind it are a library so the tests can knock on a
//! real socket, which is the only way to assert that a refusal arrived and a
//! close actually closed.

#[cfg(feature = "server")]
pub mod client;
#[cfg(feature = "server")]
pub mod door;
#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod snapshot;
pub mod types;
pub mod visit;

#[cfg(feature = "server")]
pub use server::*;

#[cfg(feature = "server")]
mod server {
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::controller::StateControllerBuilder;
use plaza::session::Session;
use plaza::tick_driver::TickDriver;
use plaza_session::codec::JsonCodec;
use plaza_session::tcp::TcpPlazaSession;
use plaza_session::{ActixWsPlazaSession, SessionOptions};

use crate::door::{Door, Doorman};
use crate::logic::{ArcadeLogic, ArcadeState};
use crate::snapshot::RoomSnapshotter;
use crate::types::{AgentKey, ArcadeOp, DuplicateLogin, CREDENTIAL_WAIT, PER_IP, TICK};

pub type Arcade = TcpPlazaSession<ArcadeOp, AgentKey>;
pub type WsArcade = ActixWsPlazaSession<ArcadeOp, AgentKey>;

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

  run_arcade(session.clone(), session.manager().clone(), door.clone());
  (session, door)
}

/// The same arcade over WebSockets, for a browser. The route admits each
/// socket through [`ws_route`] with the returned doorman.
pub fn ws_arcade(policy: DuplicateLogin, per_ip: usize) -> (Arc<WsArcade>, Arc<Door>, Arc<Doorman>) {
  let door = Door::with_per_ip(policy, per_ip);
  let doorman = Arc::new(Doorman::new(door.clone()));

  let mut options = SessionOptions::default();
  options.limits.credential_timeout = CREDENTIAL_WAIT;
  let session = WsArcade::with_options(JsonCodec, options);
  doorman.attach(session.manager().clone());

  run_arcade(session.clone(), session.manager().clone(), door.clone());
  (session, door, doorman)
}

/// Every socket waits unregistered until the doorman has read its credential.
pub async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<WsArcade>>,
  doorman: web::Data<Arc<Doorman>>,
) -> Result<HttpResponse, actix_web::Error> {
  session.admit_connection(&req, stream, doorman.get_ref().clone())
}

fn run_arcade<S>(session: Arc<S>, manager: Arc<plaza_session::ConnectionManager<AgentKey>>, door: Arc<Door>)
where
  S: Session<ArcadeOp, AgentKey> + 'static,
{
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(ArcadeLogic { door, manager }),
    session,
    Arc::new(RoomSnapshotter),
    ArcadeState::default(),
  )
  .build();
  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::new(TICK).run(tx));
}
}
