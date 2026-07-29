//! Fairness distilled: items drop, everyone grabs, the server awards each claim.
//!
//! The subject is the reply. Several items sit on the floor at once, so a client
//! has several claims outstanding and "your last one was refused" answers
//! nothing: every reply carries the `req` the client sent. See README.md.

mod logic;
mod types;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use crate::logic::{AuctionLogic, Floor, FloorSession, FloorSnapshotter};
use crate::types::{PlayerId, TICK_HZ};

const BIND: &str = "127.0.0.1:8091";

struct Services {
  session: Arc<FloorSession>,
  ids: AtomicU64,
}

async fn index() -> HttpResponse {
  HttpResponse::Ok()
    .content_type("text/html; charset=utf-8")
    .body(include_str!("../static/index.html"))
}

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  services: web::Data<Services>,
) -> Result<HttpResponse, actix_web::Error> {
  let player: PlayerId = services.ids.fetch_add(1, Ordering::Relaxed);
  info!(player, "Bidder connected.");
  services.session.handle_connection(&req, stream, Agent::new_human(player))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("plaza_example_auction_floor=debug".parse().unwrap()),
    )
    .init();

  let session: Arc<FloorSession> = ActixWsPlazaSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(AuctionLogic::new(session.clone())),
    session.clone(),
    Arc::new(FloorSnapshotter::new(session.clone())),
    Floor::new(),
  )
  .command_buffer(256)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("Controller exited with error: {e}");
    }
  });
  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(commands));

  let services = web::Data::new(Services {
    session,
    ids: AtomicU64::new(1),
  });

  info!("Auction floor on http://{BIND} at {TICK_HZ}Hz");

  HttpServer::new(move || {
    App::new()
      .app_data(services.clone())
      .route("/", web::get().to(index))
      .route("/ws", web::get().to(ws_route))
  })
  .bind(BIND)?
  .run()
  .await
}
