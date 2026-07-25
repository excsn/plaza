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
use plaza_session::host::{init_logging, Host};

use plaza_lobby::{RoomId, RoomMetadata};

use crate::net::arena::{Arena, ArenaLogic, HostView, NoSnapshots, PlayerKey, Router};
use crate::net::rooms::{self, ROOMS, DEFAULT_ROOM};
use crate::sim::types::MAX_PLAYERS;
use crate::sim::protocol::Op;
use crate::sim::types::Controls;

type ArenaSession = ActixWsPlazaSession<Op, PlayerKey, ()>;

/// The tick rate the simulation is advanced at. Distinct from the *send* rate,
/// which is `Controls::sync_hz` and is usually far lower: simulating often and
/// sending rarely is the whole reason this example exists.
/// The arena's simulation rate. Public so a readout can say what the tick budget
/// *is* rather than hard-coding a second copy of it.
pub const TICK_HZ: u32 = 60;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "horde_playground.wasm";

struct Wiring {
  /// One session per arena, indexed by room id.
  sessions: Vec<Arc<ArenaSession>>,
  /// Keys are handed out across every arena from one counter, so a player that
  /// is placed elsewhere and reconnects is a new connection with a new key,
  /// which is what the seat table and the transport's measurements both expect.
  next_key: AtomicU64,
}

async fn ws_route(req: HttpRequest, stream: web::Payload, path: web::Path<u32>, wiring: web::Data<Wiring>) -> Result<HttpResponse, actix_web::Error> {
  let room = path.into_inner();
  let Some(session) = wiring.sessions.get(room as usize) else {
    return Ok(HttpResponse::NotFound().body("no such arena"));
  };
  let key = wiring.next_key.fetch_add(1, Ordering::Relaxed);
  session.handle_connection(&req, stream, Agent::new_human(key, format!("player-{key}")))
}

/// An address with no room in it lands in the default arena, which then measures
/// and places the connection like any other.
async fn default_route(req: HttpRequest, stream: web::Payload, wiring: web::Data<Wiring>) -> Result<HttpResponse, actix_web::Error> {
  ws_route(req, stream, web::Path::from(DEFAULT_ROOM as u32), wiring).await
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
pub async fn serve(
  bind: &str,
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  static_dir: Option<String>,
  stats: Option<Arc<plaza::stats::ControllerStats>>,
) -> std::io::Result<()> {
  init_logging();

  let base = *controls.lock();
  let mut sessions = Vec::new();

  // Every arena's advertised capacity, built once so the router can match a
  // measured link against all of them. A room states its own budget, derived
  // from its schedule rather than declared beside it.
  // The lobby's room id is a `Uuid` and this example's is a small index, so the
  // index rides in the settings summary, which is the field for exactly that.
  let catalogue: Vec<RoomMetadata<u32>> = ROOMS
    .iter()
    .map(|room| RoomMetadata {
      room_id: RoomId::new_v4(),
      name: room.name.to_owned(),
      game_mode: "horde".into(),
      current_players: 0,
      max_players: MAX_PLAYERS as u32,
      has_password: false,
      max_one_way_ms: Some(room.budget_ms(&base)),
      custom_game_settings_summary: room.id,
    })
    .collect();

  // The placement rule, from the lobby crate rather than open-coded here: the
  // tightest arena that can carry this link, or nothing when none can.
  let router: Router = {
    let catalogue = catalogue.clone();
    Arc::new(move |one_way_ms| {
      plaza_lobby::routing::best_for(one_way_ms, catalogue.clone()).and_then(|m| {
        let room = rooms::room(m.custom_game_settings_summary)?;
        Some((room.id, room.name.to_owned(), room.endpoint()))
      })
    })
  };

  for room in ROOMS.iter() {
    let session: Arc<ArenaSession> = ActixWsPlazaSession::new();
    // Only the arena the host plays in reads the shared panel; the others run on
    // their own settings, or a slider drag would rewrite every room's schedule
    // and undo the thing that makes them different.
    let room_controls = if room.id as usize == DEFAULT_ROOM {
      controls.clone()
    } else {
      Arc::new(Mutex::new(room.controls(base)))
    };
    let room_view = (room.id as usize == DEFAULT_ROOM).then(|| view.clone()).flatten();

    let measured = {
      let session = session.clone();
      Arc::new(move |key: &PlayerKey| session.agent_rtt(key)) as crate::net::arena::LatencySource
    };
    let logic = ArenaLogic::new(room_controls, room_view)
      .with_latency(measured)
      .with_router(room.id, router.clone());
    let mut builder = StateControllerBuilder::new(
      Arc::new(logic),
      session.clone(),
      Arc::new(NoSnapshots),
      Arena::new(room.controls(base)),
    )
    // No snapshot on join. The world goes out as `Op::Frame` on the tick after
    // a player is seated, which is at most one send interval away.
    .snapshot_context_on_join(None)
    .command_buffer(256);
    // Handed in rather than taken out, because the window is already running by
    // the time this task starts and needs the handle before then. Only the room
    // the host plays in reports to its panel.
    if room.id as usize == DEFAULT_ROOM
      && let Some(stats) = stats.clone()
    {
      builder = builder.with_stats(stats);
    }
    let (commands, controller) = builder.build();

    tokio::spawn(controller.run());
    tokio::spawn(TickDriver::from_hz(TICK_HZ).run(commands.clone()));
    tracing::info!(room = room.id, name = room.name, playout_ms = room.playout_delay_ms, budget_ms = room.budget_ms(&base), "arena up");
    sessions.push(session);
  }

  let wiring = web::Data::new(Wiring {
    sessions,
    next_key: AtomicU64::new(1),
  });

  tracing::info!(bind, tick_hz = TICK_HZ, "arena listening");
  Host::new(bind)
    .serve_dir(static_dir)
    // The wasm is a build product: it does not rebuild when the host does, so a
    // browser holding an older copy is the normal case rather than an exotic one.
    .cache_bust(WASM_FILE)
    .run(move |cfg| {
      cfg
        .app_data(wiring.clone())
        // The room the host plays in, so an unqualified address still works and a
        // browser served from this port needs to know nothing about rooms.
        .route("/ws", web::get().to(default_route))
        .route("/ws/{room}", web::get().to(ws_route));
    })
    .await
}
