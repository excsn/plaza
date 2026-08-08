//! The card table over real WebSockets, so the hidden information is visible
//! as an absence rather than asserted in a log.
//!
//! Open http://127.0.0.1:8081 in three tabs: the table deals once three seats
//! are filled. Each tab shows its own three cards by rank and everyone else's
//! as a count, because [`TableSnapshotter`] builds a different payload per
//! recipient. That is the whole point of this example, and a page is the only
//! place it can be seen rather than taken on trust.
//!
//! Stall on your turn and the table plays for you: the turn timeout is the same
//! `Epoch`-guarded scheduler the scripted run in `main.rs` exercises.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{agent::Agent, controller::StateControllerBuilder, tick_driver::TickDriver};
use plaza_session::host::Host;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use plaza_example_card_table::logic::TableLogic;
use plaza_example_card_table::snapshot::TableSnapshotter;
use plaza_example_card_table::types::{CardOp, PlayerId, TableState, PROTOCOL};
use plaza_session::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;

/// Drives the turn timeouts. Without it nothing ever times out.
const TICK: Duration = Duration::from_millis(20);
/// Ten seconds, which is what a person needs to look at a hand and click. The
/// scripted run keeps the far shorter default so it reaches a timeout on
/// purpose within a few seconds.
const TURN_TIMEOUT: u64 = 500;

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

type TableSession = ActixWsPlazaSession<CardOp, PlayerId>;

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<TableSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(PlayerId(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed)));
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
        .add_directive("plaza_example_card_table=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Card Table - Starting");

  let session: Arc<TableSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(TableLogic),
    session.clone(),
    Arc::new(TableSnapshotter),
    TableState::new().with_turn_timeout(TURN_TIMEOUT),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  tokio::spawn(TickDriver::new(TICK).run(controller_tx.clone()));
  // Seats fill with bots only after someone has waited: three tabs should get
  // each other rather than a table of bots.
  tokio::spawn(plaza_example_card_table::bots::fill_the_table(
    controller_tx.clone(),
    vec![PlayerId(901), PlayerId(902)],
  ));

  let server_addr = "127.0.0.1:8081";
  info!("Serving http://{} (WebSocket at /ws). Three tabs seats the table.", server_addr);

  let session = web::Data::new(session);
  Host::new(server_addr)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
