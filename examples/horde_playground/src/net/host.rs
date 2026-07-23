//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! One port matters more than it sounds. A joiner is given a single URL, the
//! page and the socket come from the same origin, and there is no CORS story and
//! no second thing to configure. It is also what makes `--role host` a thing you
//! can tell a friend over a chat message.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use parking_lot::Mutex;
use plaza::{Agent, StateControllerBuilder, TickDriver};
use plaza_session::actix_ws::ActixWsPlazaSession;

use crate::net::arena::{Arena, ArenaLogic, HostView, NoSnapshots, PlayerKey};
use crate::sim::protocol::Op;
use crate::sim::types::Controls;

type ArenaSession = ActixWsPlazaSession<Op, PlayerKey, ()>;

/// The tick rate the simulation is advanced at. Distinct from the *send* rate,
/// which is `Controls::sync_hz` and is usually far lower: simulating often and
/// sending rarely is the whole reason this example exists.
const TICK_HZ: u32 = 60;

struct Wiring {
  session: Arc<ArenaSession>,
  next_key: AtomicU64,
}

async fn ws_route(req: HttpRequest, stream: web::Payload, wiring: web::Data<Wiring>) -> Result<HttpResponse, actix_web::Error> {
  let key = wiring.next_key.fetch_add(1, Ordering::Relaxed);
  wiring.session.handle_connection(&req, stream, Agent::new_human(key, format!("player-{key}")))
}

/// Turns on console logging, once.
///
/// `plaza` and `plaza_session` are instrumented throughout and say useful things
/// about connections, presence and the controller loop, but `tracing` is silent
/// without a subscriber. A server that logs nothing is indistinguishable from a
/// server that is not running, which is exactly how this looked before.
pub fn init_logging() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
      .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,actix_server=warn,actix_web=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
  });
}

/// A local address somebody else could actually reach.
///
/// No dependency and no packets: connecting a UDP socket only picks a route, so
/// the kernel fills in the source address it would use.
fn lan_address() -> Option<String> {
  let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
  socket.connect("8.8.8.8:80").ok()?;
  Some(socket.local_addr().ok()?.ip().to_string())
}

/// Prints where to point a browser, unconditionally.
///
/// Not through `tracing`: a log line only appears if somebody installed a
/// subscriber and set a filter, and the first thing a person needs after
/// starting a server is a URL.
fn announce(bind: &str, static_dir: Option<&str>) {
  let port = bind.rsplit(':').next().unwrap_or("8080");
  println!("\n  arena listening on {bind}");
  if static_dir.is_some() {
    println!("  play here:  http://127.0.0.1:{port}");
    if let Some(ip) = lan_address() {
      println!("  others at:  http://{ip}:{port}");
    }
  } else {
    println!("  no --serve directory given, so there is no page to open.");
    println!("  clients can still join at ws://127.0.0.1:{port}/ws");
  }
  println!();
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

  // Check the directory before binding. A missing index.html otherwise shows up
  // much later as a 404 in a browser, which looks like a routing bug rather than
  // a wrong path on the command line.
  if let Some(dir) = &static_dir {
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
      return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("--serve {dir}: not a directory (relative to {:?})", std::env::current_dir().unwrap_or_default())));
    }
    if !path.join("index.html").is_file() {
      return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("--serve {dir}: no index.html in it, so there would be nothing to open")));
    }
  }

  let session: Arc<ArenaSession> = ActixWsPlazaSession::new();

  let initial = *controls.lock();
  let logic = ArenaLogic::new(controls, view);
  let (commands, controller) = StateControllerBuilder::new(Arc::new(logic), session.clone(), Arc::new(NoSnapshots), Arena::new(initial))
    // No snapshot on join. The world goes out as `Op::Frame` on the tick after
    // a player is seated, which is at most one send interval away.
    .snapshot_context_on_join(None)
    .command_buffer(256)
    .build();

  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(commands.clone()));

  let static_dir_for_banner = static_dir.clone();
  let wiring = web::Data::new(Wiring {
    session,
    next_key: AtomicU64::new(1),
  });

  let server = HttpServer::new(move || {
    let app = App::new().app_data(wiring.clone()).route("/ws", web::get().to(ws_route));
    match &static_dir {
      Some(dir) => app.service(actix_files::Files::new("/", dir).index_file("index.html")),
      None => app,
    }
  })
  // Leave the signals to the process. A windowed host runs this on a background
  // thread while macroquad owns the main one; if actix kept its own SIGINT
  // handler, Ctrl-C would start a graceful shutdown here, close the sockets, and
  // leave the window running and the controller spraying "connection closed" as
  // it kept ticking into dead links.
  .disable_signals()
  .bind(bind)
  .map_err(|e| std::io::Error::new(e.kind(), format!("could not bind {bind}: {e}. Is something already using that port?")))?;

  tracing::info!(bind, tick_hz = TICK_HZ, "arena listening");
  announce(bind, static_dir_for_banner.as_deref());
  server.run().await
}
