//! Standing the volume up behind a WebSocket, and serving the browser client
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

use crate::controls::Controls;
use crate::logic::SpaceLogic;
use crate::protocol::{PlayerId, SpaceOp, PROTOCOL, TICK_HZ};
use crate::state::SpaceState;

type SpaceSession = ActixWsPlazaSession<SpaceOp, PlayerId, MsgPackCodec>;

const WASM_FILE: &str = "spacemo.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<SpaceSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  tracing::info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the volume until the process ends.
pub async fn serve(
  bind: &str,
  static_dir: Option<String>,
  controls: Arc<parking_lot::Mutex<Controls>>,
) -> std::io::Result<()> {
  init_logging();
  let held = *controls.lock();

  let sim_clock = Arc::new(AtomicU64::new(0));
  let session: Arc<SpaceSession> = ActixWsPlazaSession::with_options(
    MsgPackCodec,
    SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
      let sim_clock = sim_clock.clone();
      move || sim_clock.load(Ordering::Relaxed)
    }),
  );

  let logic = SpaceLogic::new().with_clock(sim_clock).with_controls(controls);
  // No snapshot provider: a joiner is told what it can see on the next tick,
  // which in a volume is the whole of what it could be told anyway.
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(logic),
    session.clone(),
    Arc::new(NoSnapshots),
    SpaceState::with(held.strategy),
  )
  .snapshot_context_on_join(None)
  .command_buffer(256)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::from_hz(TICK_HZ as u32).run(commands));

  tracing::info!(bind, strategy = ?held.strategy, "spacemo listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
