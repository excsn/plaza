//! The draft over real WebSockets, so the reversal is something you sit through
//! rather than read in a log.
//!
//! Open http://127.0.0.1:8093 in three tabs: the board opens once three seats
//! are filled. Watch the order run down the board, then come back up it, which
//! is the one thing a round-robin manager cannot do and the reason this example
//! exists.
//!
//! Sit on the clock and the board takes the best remaining prospect for you,
//! through the same `Epoch`-guarded scheduler the scripted run in `main.rs`
//! exercises.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{agent::Agent, controller::StateControllerBuilder, tick_driver::TickDriver};
use plaza_session::host::Host;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use plaza_example_draft_board::logic::DraftLogic;
use plaza_example_draft_board::snapshot::BoardSnapshotter;
use plaza_example_draft_board::types::{DraftOp, DraftState, PlayerId, PROTOCOL};
use plaza_session::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;

/// Drives the pick clock. Without it nothing ever times out.
const TICK: Duration = Duration::from_millis(20);
/// Fifteen seconds, which is what a person needs to read a board and choose.
/// The scripted run keeps the far shorter default so it reaches a timeout on
/// purpose within a few seconds.
const PICK_TIMEOUT: u64 = 750;

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

type BoardSession = ActixWsPlazaSession<DraftOp, PlayerId>;

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<BoardSession>>,
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
        .add_directive("plaza_example_draft_board=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Draft Board - Starting");

  let session: Arc<BoardSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(DraftLogic),
    session.clone(),
    Arc::new(BoardSnapshotter),
    DraftState::new().with_pick_timeout(PICK_TIMEOUT),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  tokio::spawn(TickDriver::new(TICK).run(controller_tx.clone()));

  let server_addr = "127.0.0.1:8093";
  info!("Serving http://{} (WebSocket at /ws). Three tabs opens the board.", server_addr);

  let session = web::Data::new(session);
  Host::new(server_addr)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
