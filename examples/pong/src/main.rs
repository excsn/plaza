//! Two-player Pong over real WebSockets.
//!
//! This is the "wire it to a real transport" example: the game loop is the same
//! Plaza controller as everywhere else, but clients connect over actix-web
//! WebSockets via `plaza_session`. Open http://127.0.0.1:8080 in two browser
//! tabs to play.

mod logic;
mod snapshot;
mod types;

use std::sync::Arc;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use plaza::{
  agent::Agent,
  controller::StateControllerBuilder,
  tick_driver::TickDriver,
};
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::{
  logic::PongLogic,
  snapshot::PongSnapshotter,
  types::{PlayerId, PongGameState, PongOp},
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

async fn index() -> HttpResponse {
  HttpResponse::Ok()
    .content_type("text/html; charset=utf-8")
    .body(include_str!("../static/index.html"))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("plaza_example_pong=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Pong Example - Starting");

  let session: Arc<PongSession> = ActixWsPlazaSession::new();
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

  let server_addr = "127.0.0.1:8080";
  info!("Serving http://{} (WebSocket at /ws), {}Hz tick", server_addr, TICK_HZ);

  HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(session.clone()))
      .route("/", web::get().to(index))
      .route("/ws", web::get().to(ws_route))
  })
  .bind(server_addr)?
  .run()
  .await
}
