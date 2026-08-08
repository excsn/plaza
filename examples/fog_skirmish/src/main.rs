//! Fog of war over real WebSockets.
//!
//! Open http://127.0.0.1:8082. Click to send your scouts; you see only what
//! they see. Bots hold the other corners.
//!
//! The panel is the example. It counts what the *outbound op stream* told you,
//! not what the frames cost, because a per-recipient frame can be perfectly
//! filtered while the events beside it name places nobody scouted. The toggle
//! turns the deferral off, and the leak counter starts climbing immediately.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{agent::Agent, controller::StateControllerBuilder, tick_driver::TickDriver};
use plaza_session::host::Host;
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info, Level};
use tracing_subscriber::EnvFilter;

use plaza_example_fog_skirmish::bots;
use plaza_example_fog_skirmish::logic::FogLogic;
use plaza_example_fog_skirmish::snapshot::FogSnapshotter;
use plaza_example_fog_skirmish::types::{FogOp, FogState, PlayerId, PROTOCOL};
use plaza_session::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;

const TICK_HZ: u32 = 60;
/// Bots take 1..=BOTS, browsers start at 100.
const BOTS: PlayerId = 2;
const FIRST_HUMAN: PlayerId = 100;

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

type FogSession = ActixWsPlazaSession<FogOp, PlayerId>;

static NEXT_HUMAN: AtomicU32 = AtomicU32::new(FIRST_HUMAN);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<FogSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_HUMAN.fetch_add(1, Ordering::Relaxed));
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
        .add_directive("plaza_example_fog_skirmish=debug".parse().unwrap()),
    )
    .init();

  info!("Plaza Fog Skirmish - Starting");

  let session: Arc<FogSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(FogLogic),
    session.clone(),
    Arc::new(FogSnapshotter),
    FogState::new(),
  )
  .command_buffer(128)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(controller_tx.clone()));
  tokio::spawn(bots::spawn_bots(controller_tx.clone(), (1..=BOTS).collect()));

  let server_addr = "127.0.0.1:8082";
  info!(
    "Serving http://{} (WebSocket at /ws), {}Hz tick, {} bot scouts",
    server_addr, TICK_HZ, BOTS
  );

  let session = web::Data::new(session);
  Host::new(server_addr)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
