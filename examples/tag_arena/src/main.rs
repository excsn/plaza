//! Tag arena: the state-sync netcode model, over real WebSockets.
//!
//! Clients send nothing but a steer direction. The server integrates movement
//! and, every tick, sends everyone the same whole-world snapshot as one uniform
//! pass: the provider runs once, the payload encodes once, and each recipient
//! gets a refcounted copy (`SnapshotRequest::uniform`). Clients keep no op
//! history; the latest snapshot IS the game, so a stale one is discarded and a
//! mid-game joiner is caught up by the very next frame.
//!
//! Open http://127.0.0.1:8080 to play. Bots are already running, so one browser
//! tab is a game; open a second to be chased by a person instead.

mod bots;
mod logic;
mod snapshot;
mod types;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::{
  agent::Agent,
  controller::{query_with, StateControllerBuilder},
  tick_driver::TickDriver,
};
use plaza_session::host::{init_logging, Host};
use plaza_session::ActixWsPlazaSession;
use tracing::{error, info};

use plaza_session::codec::JsonCodec;
use plaza_wire::frame::ProtocolVersion;

use crate::{
  bots::ArenaCommands,
  logic::TagLogic,
  snapshot::WorldSnapshotProvider,
  types::{ArenaOp, ArenaState, PlayerId, PROTOCOL},
};

/// Simulation rate. Tag is a chase, so it runs at 60Hz like `pong`.
const TICK_HZ: u32 = 60;
/// Bots take ids 1..=BOTS; browsers start at 100, so a log line says which is
/// which without a lookup.
const BOTS: PlayerId = 3;
const FIRST_HUMAN: PlayerId = 100;
/// How often the standings are logged, so running this with no browser open
/// still shows a game happening.
const STANDINGS_EVERY: Duration = Duration::from_secs(10);

type ArenaSession = ActixWsPlazaSession<ArenaOp, PlayerId>;

static NEXT_HUMAN: AtomicU32 = AtomicU32::new(FIRST_HUMAN);

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session: web::Data<Arc<ArenaSession>>,
) -> Result<HttpResponse, actix_web::Error> {
  let agent = Agent::new_human(NEXT_HUMAN.fetch_add(1, Ordering::Relaxed));
  info!(player = %agent, "WebSocket connection opening.");
  session.handle_connection(&req, stream, agent)
}

async fn log_standings(tx: ArenaCommands) {
  let mut ticker = tokio::time::interval(STANDINGS_EVERY);
  ticker.tick().await;
  loop {
    ticker.tick().await;
    let Ok(standings) = query_with(&tx, |state: &ArenaState| {
      let mut rows: Vec<_> = state
        .runners
        .iter()
        .map(|(id, r)| (*id, r.bot, r.tags, r.ticks_as_it))
        .collect();
      rows.sort_by_key(|(id, ..)| *id);
      (rows, state.it, state.tick)
    })
    .await
    else {
      return;
    };
    let (rows, it, tick) = standings;
    if rows.is_empty() {
      continue;
    }
    let line = rows
      .iter()
      .map(|(id, bot, tags, as_it)| {
        let kind = if *bot { "bot" } else { "human" };
        let marker = if Some(*id) == it { "*" } else { "" };
        format!("{kind}-{id}{marker} tags:{tags} it:{as_it}")
      })
      .collect::<Vec<_>>()
      .join("  ");
    info!(tick, "{}", line);
  }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  init_logging();

  info!("Plaza Tag Arena - Starting");

  let session: Arc<ArenaSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let (controller_tx, controller) = StateControllerBuilder::new(
    Arc::new(TagLogic),
    session.clone(),
    Arc::new(WorldSnapshotProvider),
    ArenaState::default(),
  )
  .command_buffer(128)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });

  // Without this nobody moves: the controller only advances simulation time
  // when something sends it a time step.
  tokio::spawn(TickDriver::from_hz(TICK_HZ).run(controller_tx.clone()));
  tokio::spawn(bots::spawn_bots(controller_tx.clone(), (1..=BOTS).collect()));
  tokio::spawn(log_standings(controller_tx.clone()));

  let session = web::Data::new(session);

  info!(tick_hz = TICK_HZ, bots = BOTS, "tag arena listening");
  Host::new("127.0.0.1:8080")
    .serve_dir(Some(concat!(env!("CARGO_MANIFEST_DIR"), "/static").to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg.app_data(session.clone()).route("/ws", web::get().to(ws_route));
    })
    .await
}
