//! Standing the floor up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! Two wires matter here beyond the usual stack: the session's pong clock is
//! the simulation clock, so a client's timeline aims its sub-tick claims at
//! sim time rather than wall time, and the session's measured RTT is what the
//! logic floors those claims against.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{Agent, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;
use plaza_session::host::{init_logging, Host};
use plaza_session::SessionOptions;
use plaza_wire::frame::ProtocolVersion;
use plaza_wire::MsgPackCodec;

use crate::logic::{DuelLogic, LatencySource};
use crate::protocol::{DrawOp, PlayerId, PROTOCOL, TICK_MS};
use crate::snapshot::DuelSnapshotter;
use crate::state::DuelState;

type DuelSession = ActixWsPlazaSession<DrawOp, PlayerId, MsgPackCodec>;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "quick_draw.wasm";

static NEXT_PLAYER: AtomicU32 = AtomicU32::new(1);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<DuelSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_PLAYER.fetch_add(1, Ordering::Relaxed));
  tracing::info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

/// Runs the floor until the process ends.
pub async fn serve(bind: &str, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  let sim_clock = Arc::new(AtomicU64::new(0));
  let session: Arc<DuelSession> = ActixWsPlazaSession::with_options(
    MsgPackCodec,
    SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
      let sim_clock = sim_clock.clone();
      move || sim_clock.load(Ordering::Relaxed)
    }),
  );

  // Half the measured round trip, in µs: the number the floor holds a claim
  // against. `None` until the probes have an answer, which the floor treats
  // as zero and therefore strictly.
  let latency: LatencySource = {
    let session = session.clone();
    Arc::new(move |player: &PlayerId| {
      session
        .agent_rtt(player)
        .map(|(rtt, _)| (rtt.as_micros() / 2) as u64)
    })
  };

  let logic = DuelLogic::new().with_latency(latency).with_clock(sim_clock);
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(logic),
    session.clone(),
    Arc::new(DuelSnapshotter),
    DuelState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      tracing::error!("StateController exited with error: {}", e);
    }
  });
  tokio::spawn(TickDriver::new(Duration::from_millis(TICK_MS)).run(commands));

  tracing::info!(bind, "quick draw listening (WebSocket at /ws)");
  let session = web::Data::new(session);
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
