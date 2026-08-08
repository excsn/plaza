//! Standing the battlefield up behind a WebSocket, and serving the browser
//! client from the same port.
//!
//! The HTTP side (the port, the served directory, the cache busting that keeps
//! a browser from running yesterday's bundle against today's server) is
//! [`plaza_session::host::Host`]. What is left here is this battle's own:
//! which state, which logic, which codec, and the bot that takes the second
//! seat when nobody else does.

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

use crate::bots;
use crate::logic::BattleLogic;
use crate::protocol::{BattleOp, PlayerId, PROTOCOL};
use crate::snapshot::BattleSnapshotter;
use crate::state::BattleState;

type BattleSession = ActixWsPlazaSession<BattleOp, PlayerId, MsgPackCodec>;

const TICK: Duration = Duration::from_millis(20);

/// A minute per command phase on the small field: enough to think, short
/// enough that an abandoned window does not hold the field. The bigger maps
/// scale it up (`MapSize::side_ticks`).
const SIDE_TICKS: u64 = 3000;

/// Fifteen seconds of muster once the first commander stands ready.
const MUSTER_TICKS: u64 = 750;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "field_orders.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<BattleSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  tracing::info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the battlefield until the process ends.
///
/// Blocks, so a windowed host calls it on a background thread with its own
/// runtime while the frame loop keeps the main thread.
pub async fn serve(bind: &str, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  // MessagePack with a declared protocol version: the wasm client is a build
  // product that goes stale, and the handshake is what tells it so.
  let session: Arc<BattleSession> =
    ActixWsPlazaSession::with_options(MsgPackCodec, SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)));

  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(BattleLogic),
    session.clone(),
    Arc::new(BattleSnapshotter),
    BattleState::new().with_side_ticks(SIDE_TICKS).with_muster_ticks(MUSTER_TICKS),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(TICK).run(commands.clone()));
  tokio::spawn(bots::play_the_bots(commands, bots::THINK));

  tracing::info!(bind, "field orders listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
