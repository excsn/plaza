//! A card game behind a lobby: the shape a matchmade, turn-based game takes.
//!
//! Open http://127.0.0.1:8092 in three tabs and press quick match in each, or
//! press it in one and wait for the bots. See README.md.


use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_lobby::manager::InMemoryLobbyManager;
use plaza_lobby::{CachedTicketRegistry, TicketStore};
use plaza_session::codec::JsonCodec;
use plaza_session::host::{init_logging, Host};
use plaza_session::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use serde::Deserialize;
use tracing::{error, info, warn};

use plaza_example_parlour_game::factory::{TableFactory, TableRegistry};
use plaza_example_parlour_game::lobby::{LobbyLogic, LobbySession, LobbyState, NoLobbySnapshot};
use plaza_example_parlour_game::types::{PlayerId, PROTOCOL};
use plaza_example_parlour_game::wallets::WalletRegistry;

const BIND: &str = "127.0.0.1:8092";
const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

/// Keeps the socket registry from outliving the lobby's own room map.
const REAP_EVERY: Duration = Duration::from_secs(15);

/// How long a table may carry no traffic before the reaper drains it. Every
/// table here is spawned for one match, so all of them are eventually reaped.
const TABLE_IDLE_AFTER: Duration = Duration::from_secs(45);

/// Only the match queue needs the lobby to advance time, and it measures its
/// patience in seconds.
/// Longer than a placement takes to dial, shorter than the seat reservation it
/// pairs with, which currently has no window of its own.
const PLACEMENT_WINDOW: Duration = Duration::from_secs(30);

const LOBBY_TICK_HZ: u32 = 4;

/// The only place a `PlayerId` is created.
struct PlayerIds(AtomicU64);

struct Services {
  lobby_session: Arc<LobbySession>,
  tables: Arc<TableRegistry>,
  tickets: Arc<CachedTicketRegistry<PlayerId>>,
  ids: PlayerIds,
}

#[derive(Deserialize)]
struct TableQuery {
  /// The lobby's ticket. A table has no other way to learn who this is.
  t: Option<String>,
}

/// Identity is minted here and nowhere else.
async fn lobby_route(
  req: HttpRequest,
  stream: web::Payload,
  services: web::Data<Services>,
) -> Result<HttpResponse, actix_web::Error> {
  let player: PlayerId = services.ids.0.fetch_add(1, Ordering::Relaxed);
  info!(player, "Lobby connection opening.");
  services
    .lobby_session
    .handle_connection(&req, stream, Agent::new_human(player))
}

/// Identity comes from the ticket, not the URL: a URL-supplied id would let
/// anyone connect as anyone and play their hand.
async fn table_route(
  req: HttpRequest,
  stream: web::Payload,
  path: web::Path<String>,
  query: web::Query<TableQuery>,
  services: web::Data<Services>,
) -> Result<HttpResponse, actix_web::Error> {
  let Ok(room_id) = path.into_inner().parse::<uuid::Uuid>() else {
    return Ok(HttpResponse::BadRequest().body("malformed table id"));
  };
  let Some(entry) = services.tables.get(&room_id) else {
    return Ok(HttpResponse::NotFound().body("no such table"));
  };
  let Some(token) = query.into_inner().t else {
    return Ok(HttpResponse::Unauthorized().body("no ticket: ask the lobby for a placement"));
  };
  let Some(ticket) = services.tickets.redeem(&token, &room_id) else {
    return Ok(HttpResponse::Unauthorized().body("ticket unknown, already spent, expired, or for another table"));
  };

  info!(player = ticket.player, room = %room_id, "Table connection opening.");
  entry
    .session
    .handle_connection(&req, stream, Agent::new_human(ticket.player))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  init_logging();

  let wallets = Arc::new(WalletRegistry::new());
  let tables = Arc::new(TableRegistry::new());
  let tickets = Arc::new(CachedTicketRegistry::with_expiry(PLACEMENT_WINDOW));

  let factory = Arc::new(TableFactory::new(wallets.clone(), tables.clone(), BIND.to_string()));
  let manager = Arc::new(InMemoryLobbyManager::new(factory));

  // Nothing is pre-spawned: a table exists because a match formed.
  let lobby_session: Arc<LobbySession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let logic = Arc::new(LobbyLogic::new(
    manager.clone(),
    tables.clone(),
    wallets.clone(),
    tickets.clone(),
    lobby_session.clone(),
  ));

  let (lobby_tx, lobby_controller) = StateControllerBuilder::new(
    logic,
    lobby_session.clone(),
    Arc::new(NoLobbySnapshot),
    LobbyState::default(),
  )
  .command_buffer(128)
  .snapshot_context_on_join(None)
  .build();

  tokio::spawn(async move {
    if let Err(e) = lobby_controller.run().await {
      error!("Lobby controller exited with error: {e}");
    }
  });
  tokio::spawn(TickDriver::from_hz(LOBBY_TICK_HZ).run(lobby_tx));

  {
    let manager = manager.clone();
    let tables = tables.clone();
    let tickets_for_sweep = tickets.clone();
    let wallets_for_sweep = wallets.clone();
    tokio::spawn(async move {
      let mut ticker = tokio::time::interval(REAP_EVERY);
      let mut quiet: std::collections::HashMap<_, (u64, tokio::time::Instant)> = std::collections::HashMap::new();
      loop {
        ticker.tick().await;

        // A table that has carried no traffic for a while is drained through the
        // same flush-then-farewell close as a kick, then told to shut down; the
        // reap below collects the finished handle on a later pass. Occupants
        // hear `Closed` before the socket goes, never a silent EOF.
        let now = tokio::time::Instant::now();
        for handle in manager.rooms() {
          let id = handle.id();
          let Some(entry) = tables.get(&id) else { continue };
          let inbound = entry.session.stats().inbound();
          let (last_inbound, since) = quiet.entry(id).or_insert((inbound, now));
          if inbound != *last_inbound {
            (*last_inbound, *since) = (inbound, now);
            continue;
          }
          if now.duration_since(*since) < TABLE_IDLE_AFTER {
            continue;
          }
          let farewell = entry
            .session
            .encode_message(plaza::session::SessionMessage::system(vec![plaza_example_parlour_game::types::TableOp::Closed {
              reason: "table closed for inactivity".into(),
            }]))
            .ok();
          let told = entry.session.manager().disconnect_all(farewell);
          warn!(room = %id, told, "Idle table drained; shutting it down.");
          let _ = entry
            .commands
            .send(plaza::controller::ControllerCommand::Shutdown)
            .await;
          quiet.remove(&id);
        }

        let before: Vec<_> = manager.rooms().iter().map(|h| h.id()).collect();
        manager.reap_finished_rooms().await;
        quiet.retain(|id, _| manager.room(id).is_some());
        for id in before {
          if manager.room(&id).is_none() {
            warn!(room = %id, "Table ended; dropping its socket registration.");
            tables.remove(&id);
          }
        }
        // Outstanding tickets climbing means placements are handed out and
        // never dialled.
        info!(
          tables = manager.rooms().len(),
          wallets = wallets_for_sweep.tracked(),
          tickets_outstanding = tickets_for_sweep.outstanding(),
          "Lobby sweep."
        );
      }
    });
  }

  let services = web::Data::new(Services {
    lobby_session,
    tables,
    tickets,
    ids: PlayerIds(AtomicU64::new(1)),
  });

  info!("Parlour game on http://{BIND}");

  Host::new(BIND)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .protocol(ProtocolVersion(PROTOCOL))
    .run(move |cfg| {
      cfg
        .app_data(services.clone())
        .route("/ws/lobby", web::get().to(lobby_route))
        .route("/ws/table/{room_id}", web::get().to(table_route));
    })
    .await
}
