//! Standing the bench up behind a WebSocket, and serving the browser client
//! from the same port.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{Agent, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;
use plaza_session::host::{init_logging, Host};
use plaza_session::SessionOptions;
use plaza_wire::frame::ProtocolVersion;
use plaza_wire::MsgPackCodec;

use crate::logic::ForgeLogic;
use crate::protocol::{ForgeOp, PlayerId, PROTOCOL, TICK_MS};
use crate::snapshot::ForgeSnapshotter;
use crate::state::ForgeState;

type ForgeSession = ActixWsPlazaSession<ForgeOp, PlayerId, MsgPackCodec>;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "map_forge.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ForgeSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  tracing::info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the bench until the process ends.
pub async fn serve(bind: &str, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  let session: Arc<ForgeSession> =
    ActixWsPlazaSession::with_options(MsgPackCodec, SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)));

  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(ForgeLogic),
    session.clone(),
    Arc::new(ForgeSnapshotter),
    ForgeState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(Duration::from_millis(TICK_MS)).run(commands));

  tracing::info!(bind, "map forge listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
