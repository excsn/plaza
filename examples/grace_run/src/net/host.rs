//! Standing the run up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! The one route detail that matters here: a returning client presents the
//! **same** agent id (`/ws?p=<id>`), because a held seat is keyed by identity
//! and a fresh id would be a stranger. In a deployment that id comes from an
//! auth token (`door_policy`'s subject); a query parameter is the development
//! stand-in.

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

use crate::logic::RunLogic;
use crate::protocol::{PlayerId, RunOp, PROTOCOL, TICK_MS};
use crate::snapshot::RunSnapshotter;
use crate::state::RunState;

type RunSession = ActixWsPlazaSession<RunOp, PlayerId, MsgPackCodec>;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "grace_run.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<RunSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let resumed: Option<PlayerId> = req
    .query_string()
    .split('&')
    .find_map(|pair| pair.strip_prefix("p="))
    .and_then(|v| v.parse().ok());
  let id = resumed.unwrap_or_else(|| NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  let agent = Agent::new_human(id);
  tracing::info!(player = %agent, resumed = resumed.is_some(), "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the delve until the process ends.
pub async fn serve(bind: &str, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  let session: Arc<RunSession> =
    ActixWsPlazaSession::with_options(MsgPackCodec, SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)));

  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(RunLogic),
    session.clone(),
    Arc::new(RunSnapshotter),
    RunState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(Duration::from_millis(TICK_MS)).run(commands));

  tracing::info!(bind, "grace run listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
