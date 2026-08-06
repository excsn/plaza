//! Fairness distilled: items drop, everyone grabs, the server awards each claim.
//!
//! The subject is the reply. Several items sit on the floor at once, so a client
//! has several claims outstanding and "your last one was refused" answers
//! nothing: every reply carries the `req` the client sent. See README.md.

mod logic;
mod types;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_session::host::{init_logging, Host};
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info};

use crate::logic::{AuctionLogic, Floor, FloorSession, FloorSnapshotter};
use crate::types::{PlayerId, TICK_HZ};

const BIND: &str = "127.0.0.1:8091";
const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

struct Services {
  session: Arc<FloorSession>,
  ids: AtomicU64,
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
  init_logging();

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

  info!("Auction floor at {TICK_HZ}Hz");

  Host::new(BIND)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .run(move |cfg| {
      cfg.app_data(services.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
