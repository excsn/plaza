//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! The HTTP side of that (the port, the served directory, the version stamping
//! that keeps a browser from running yesterday's bundle against today's server,
//! and leaving signals to the process) is [`plaza_session::host::Host`]. What is
//! left here is the part that is actually this arena's: which state, which
//! logic, and at what tick rate.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use parking_lot::Mutex;
use plaza::{Agent, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use plaza_session::host::{init_logging, Host};

use crate::net::arena::{Arena, ArenaLogic, HostView, NoSnapshots, PlayerKey};
use crate::sim::protocol::{Op, PROTOCOL};
use crate::sim::types::{Controls, B0MB_SEED, SIM_STEP_MS};

type ArenaSession = ActixWsPlazaSession<Op, PlayerKey, plaza_wire::MsgPackCodec>;

/// How often the driver wakes.
///
/// Higher than the step rate on purpose: waking more often than you step keeps
/// the phase error small, because a step is spent nearer the moment it was
/// earned. The *step* is [`SIM_STEP_MS`] whatever this is set to.
const WAKE_HZ: u32 = 120;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "bomb_grid.wasm";

struct Wiring {
  session: Arc<ArenaSession>,
  next_key: AtomicU64,
}

async fn ws_route(req: HttpRequest, stream: web::Payload, wiring: web::Data<Wiring>) -> Result<HttpResponse, actix_web::Error> {
  let key = wiring.next_key.fetch_add(1, Ordering::Relaxed);
  wiring.session.handle_connection(&req, stream, Agent::new_human(key))
}

/// Runs the arena until the process ends.
///
/// Blocks, so a windowed host calls it on a background thread with its own
/// runtime while the frame loop keeps the main thread.
pub async fn serve(bind: &str, controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  // Built with the protocol this binary speaks, so a stale browser bundle is
  // told to reload by the handshake rather than half-working.
  // The clock a `Pong` carries is the arena's simulation clock, which is what
  // clients synchronise against; the arena stores its tick here and the session
  // reads it from whichever connection task is answering.
  let sim_clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
  let session: Arc<ArenaSession> = ActixWsPlazaSession::with_options(
    plaza_wire::MsgPackCodec,
    plaza_session::SessionOptions::with_protocol(ProtocolVersion(PROTOCOL)).clock({
      let sim_clock = sim_clock.clone();
      move || sim_clock.load(std::sync::atomic::Ordering::Relaxed)
    }),
  );

  let initial = *controls.lock();
  let link = {
    let session = session.clone();
    Arc::new(move |profile| session.set_all_link_profiles(profile)) as crate::net::arena::LinkSink
  };
  let logic = ArenaLogic::new(controls, view).with_link(link).with_clock(sim_clock);
  let (commands, controller) = StateControllerBuilder::new(Arc::new(logic), session.clone(), Arc::new(NoSnapshots), Arena::new(initial, B0MB_SEED))
    // No snapshot on join: `Op::Welcome` carries the board and the players
    // together on the tick a joiner is seated.
    .snapshot_context_on_join(None)
    .command_buffer(256)
    .build();

  tokio::spawn(controller.run());
  // `run_fixed`, never `run`. `run` delivers the measured elapsed time, so the
  // simulation's rate would be a property of the host's scheduler and no client
  // could reproduce it: on a lattice that lands as a snap at every cell
  // boundary. The simulation accumulates whole ticks of its own as well, so a
  // different driver cannot silently break the guarantee.
  tokio::spawn(TickDriver::from_hz(WAKE_HZ).run_fixed(commands.clone(), Duration::from_millis(SIM_STEP_MS)));

  let wiring = web::Data::new(Wiring {
    session,
    next_key: AtomicU64::new(1),
  });

  tracing::info!(bind, wake_hz = WAKE_HZ, step_ms = SIM_STEP_MS, "arena listening");
  Host::new(bind)
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(wiring.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
