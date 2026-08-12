//! Standing the world up behind a WebSocket, and serving the browser client
//! from the same port.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{Agent, NoSnapshots, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;
use plaza_session::host::{init_logging, Host};
use plaza_session::SessionOptions;
use plaza_wire::frame::ProtocolVersion;
use plaza_wire::MsgPackCodec;

use crate::controls::Dial;
use crate::logic::SkapeLogic;
use crate::protocol::{PlayerId, SkapeOp, DRIVER_MS, PROTOCOL};
use crate::state::SkapeState;

type SkapeSession = ActixWsPlazaSession<SkapeOp, PlayerId, MsgPackCodec>;

const WASM_FILE: &str = "chapskape.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<SkapeSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  tracing::info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the world until the process ends.
pub async fn serve(
  bind: &str,
  static_dir: Option<String>,
  dial: Dial,
  bots: usize,
) -> std::io::Result<()> {
  init_logging();

  let sim_clock = Arc::new(AtomicU64::new(0));
  let session: Arc<SkapeSession> = ActixWsPlazaSession::with_options(
    MsgPackCodec,
    SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
      let sim_clock = sim_clock.clone();
      move || sim_clock.load(Ordering::Relaxed)
    }),
  );

  let logic = SkapeLogic::new()
    .with_bots(bots)
    .with_clock(sim_clock)
    .with_dial(dial);
  // No snapshot provider. A joiner's first ordinary frame is the whole audience
  // and the whole of the still world it can see, because nothing here is a
  // delta against a baseline the joiner does not have.
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(logic),
    session.clone(),
    Arc::new(NoSnapshots),
    SkapeState::new(),
  )
  .snapshot_context_on_join(None)
  .command_buffer(256)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  // The host wakes far more often than the world moves, so the game tick can be
  // a dial the panel turns rather than the rate this driver was built with.
  tokio::spawn(TickDriver::new(std::time::Duration::from_millis(DRIVER_MS)).run(commands));

  tracing::info!(bind, "ChapsKape listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
