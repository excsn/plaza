//! The village over real WebSockets, so the secrecy is something you see.
//!
//! Open http://127.0.0.1:8094 in **five** tabs: the deal happens when the fifth
//! seat fills. One tab knows it is the wolf and the others know only their own
//! role, because every snapshot is built per recipient. Get killed, and your
//! tab shows you everything: the dead see all, and can no longer be asked.
//!
//! Nights and days are long enough here to think in; the scripted run in
//! `main.rs` keeps the short defaults so its deadlines fire on purpose.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{agent::Agent, controller::StateControllerBuilder, tick_driver::TickDriver};
use plaza_session::host::Host;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use plaza_example_night_watch::logic::VillageLogic;
use plaza_example_night_watch::snapshot::VillageSnapshotter;
use plaza_example_night_watch::types::{PlayerId, VillageOp, VillageState};

const TICK: Duration = Duration::from_millis(20);
/// Twenty seconds of night, forty of day: enough to read the board and choose.
const NIGHT_TICKS: u64 = 1000;
const DAY_TICKS: u64 = 2000;

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

type VillageSession = ActixWsPlazaSession<VillageOp, PlayerId>;

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<VillageSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("plaza_example_night_watch=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Night Watch - Starting");

  let session: Arc<VillageSession> = ActixWsPlazaSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(VillageLogic),
    session.clone(),
    Arc::new(VillageSnapshotter),
    VillageState::new().with_deadlines(NIGHT_TICKS, DAY_TICKS),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(TICK).run(controller_tx.clone()));

  let server_addr = "127.0.0.1:8094";
  info!("Serving http://{} (WebSocket at /ws). Five tabs deals the village.", server_addr);

  let session = web::Data::new(session);
  Host::new(server_addr)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
