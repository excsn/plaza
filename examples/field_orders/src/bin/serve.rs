//! The battlefield over real WebSockets: two tabs, two armies.
//!
//! Open http://127.0.0.1:8095 in **two** tabs. The first is Blue this
//! deployment, the second Red, and the sides swap every battle. Click one of
//! your units, then a cell to march, an adjacent enemy to strike, or the unit
//! again to hold. End Phase hands the field over; sit too long and the phase
//! hands itself over.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{agent::Agent, controller::StateControllerBuilder, tick_driver::TickDriver};
use plaza_session::host::Host;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use plaza_example_field_orders::logic::BattleLogic;
use plaza_example_field_orders::snapshot::BattleSnapshotter;
use plaza_example_field_orders::types::{BattleOp, BattleState, PlayerId};

const TICK: Duration = Duration::from_millis(20);
/// A minute per command phase: enough to think, short enough that an abandoned
/// tab does not hold the field.
const SIDE_TICKS: u64 = 3000;

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

type BattleSession = ActixWsPlazaSession<BattleOp, PlayerId>;

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<BattleSession>>,
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
        .add_directive("plaza_example_field_orders=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Field Orders - Starting");

  let session: Arc<BattleSession> = ActixWsPlazaSession::new();
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(BattleLogic),
    session.clone(),
    Arc::new(BattleSnapshotter),
    BattleState::new().with_side_ticks(SIDE_TICKS),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(TICK).run(controller_tx.clone()));

  let server_addr = "127.0.0.1:8095";
  info!("Serving http://{} (WebSocket at /ws). Two tabs deploys the armies.", server_addr);

  let session = web::Data::new(session);
  Host::new(server_addr)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
