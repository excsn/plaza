//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! The HTTP side of that (the port, the served directory, the version stamping
//! that keeps a browser from running yesterday's bundle against today's server,
//! and leaving signals to the process) is [`plaza_session::host::Host`]. It is
//! the same in every listen server, and was the same in this repository twice
//! over. What is left here is the part that is actually this arena's: which
//! state, which logic, and at what tick rate.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use parking_lot::Mutex;
use plaza::{Agent, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use plaza_session::host::{init_logging, Host};

use plaza::NoSnapshots;

use crate::net::arena::{Arena, ArenaLogic, HostView, PlayerKey};
use crate::sim::protocol::{Op, PROTOCOL};
use crate::sim::types::Controls;

type ArenaSession = ActixWsPlazaSession<Op, PlayerKey, plaza_wire::MsgPackCodec>;

/// The tick rate the simulation is advanced at. Distinct from the *send* rate,
/// which is `Controls::sync_hz` and is usually far lower: simulating often and
/// sending rarely is the whole reason this example exists.
const TICK_HZ: u32 = 60;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "blackhole_playground.wasm";

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
///
/// `controls` is shared with the host's UI, which is how the panel's sliders
/// reach a running arena; a headless server passes the fixed set it launched
/// with and nothing ever writes it. `view` is where the arena publishes its
/// omniscient state for a windowed host to read, and `None` for a headless one
/// that has no screen to draw it on.
pub async fn serve(bind: &str, controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>, static_dir: Option<String>) -> std::io::Result<()> {
  init_logging();

  // The clock a `Pong` carries is the arena's simulation clock, which is what
  // clients synchronise against; the arena stores its tick here and the session
  // reads it from whichever connection task is answering.
  let sim_clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
  // The codec is named, not defaulted: the client is built with `MsgPackCodec`,
  // and a defaulted session speaks JSON at it. The two ends of one example
  // must name the same format.
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
  let (commands, controller) = StateControllerBuilder::new(Arc::new(logic), session.clone(), Arc::new(NoSnapshots), Arena::new(initial))
    // No snapshot on join. The world goes out as `Op::Frame` on the tick after
    // a player is seated, which is at most one send interval away.
    .snapshot_context_on_join(None)
    .command_buffer(256)
    .build();

  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(commands.clone()));

  let wiring = web::Data::new(Wiring {
    session,
    next_key: AtomicU64::new(1),
  });

  tracing::info!(bind, tick_hz = TICK_HZ, "arena listening");
  Host::new(bind)
    .serve_dir(static_dir)
    // The wasm is a build product: it does not rebuild when the host does, so a
    // browser holding an older copy is the normal case rather than an exotic one.
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg.app_data(wiring.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
