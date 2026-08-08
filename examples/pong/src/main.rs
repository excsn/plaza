//! Two-player Pong over real WebSockets.
//!
//! This is the "wire it to a real transport" example: the game loop is the same
//! Plaza controller as everywhere else, but clients connect over actix-web
//! WebSockets via `plaza_session`. Open http://127.0.0.1:8080 in two browser
//! tabs to play.

mod bots;
mod logic;
mod snapshot;
mod types;

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{
  agent::Agent,
  controller::StateControllerBuilder,
  tick_driver::TickDriver,
};
use plaza_session::host::{init_logging, Host};
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info};
use uuid::Uuid;

use plaza_session::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;

use crate::{
  logic::PongLogic,
  snapshot::PongSnapshotter,
  types::{PlayerId, PongGameState, PongOp, PROTOCOL},
};

/// Simulation rate. Pong is latency-sensitive, so it runs at 60Hz.
const TICK_HZ: u32 = 60;

type PongSession = ActixWsPlazaSession<PongOp, PlayerId>;

/// Upgrades a request to a WebSocket and hands the connection to Plaza.
///
/// Everything after this: registration, framing, broadcast fan-out, cleanup on
/// disconnect: is handled by `plaza_session`.
async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<PongSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let player_id = Uuid::new_v4();
  let agent = Agent::new_human(player_id);
  info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  init_logging();

  info!("Plaza Pong Example - Starting");

  let session: Arc<PongSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(PongLogic::default()),
    session.clone(),
    Arc::new(PongSnapshotter::default()),
    PongGameState::default(),
  )
  .command_buffer(128)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  // Without this the ball never moves: the controller only advances simulation
  // time when something sends it a time step.
  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(controller_tx.clone()));
  tokio::spawn(bots::keep_a_seat_warm(controller_tx.clone()));
  tokio::spawn(bots::play(controller_tx.clone()));

  let session = web::Data::new(session);

  info!(tick_hz = TICK_HZ, "pong listening");
  Host::new("127.0.0.1:8080")
    .serve_dir(Some(concat!(env!("CARGO_MANIFEST_DIR"), "/static").to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
