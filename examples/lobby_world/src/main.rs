//! Rooms, placement, and travel: the `plaza_lobby` example. See README.md.
//!
//! Open http://127.0.0.1:8090 in several tabs; each is assigned a different
//! link, so the room lists differ.

mod factory;
mod lobby;
mod room;
mod types;
mod wallets;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_lobby::manager::InMemoryLobbyManager;
use plaza_lobby::op_payloads::RoomSettings;
use plaza_lobby::TicketRegistry;
use plaza_session::codec::JsonCodec;
use plaza_session::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use serde::Deserialize;
use tracing::{error, info, warn, Level};
use tracing_subscriber::EnvFilter;

use crate::factory::{ArenaFactory, RoomRegistry};
use crate::lobby::{LobbyLogic, LobbySession, LobbyState, NoLobbySnapshot};
use crate::types::{PlayerId, ARENAS, PROTOCOL};
use crate::wallets::WalletRegistry;

const BIND: &str = "127.0.0.1:8090";

/// Keeps the socket registry from outliving the lobby's own room map.
const REAP_EVERY: Duration = Duration::from_secs(15);

/// How long a dynamic room may carry no traffic before the reaper drains it.
/// The pre-spawned arenas are the fixed offering and are never reaped.
const ROOM_IDLE_AFTER: Duration = Duration::from_secs(45);

/// Only the match queue needs the lobby to advance time, and it measures its
/// patience in seconds.
const LOBBY_TICK_HZ: u32 = 4;

/// The only place a `PlayerId` is created.
struct PlayerIds(AtomicU64);

struct Services {
  lobby_session: Arc<LobbySession>,
  rooms: Arc<RoomRegistry>,
  tickets: Arc<TicketRegistry<PlayerId>>,
  ids: PlayerIds,
}

#[derive(Deserialize)]
struct ArenaQuery {
  /// The lobby's ticket. An arena has no other way to learn who this is.
  t: Option<String>,
}

async fn index() -> HttpResponse {
  HttpResponse::Ok()
    .content_type("text/html; charset=utf-8")
    .body(include_str!("../static/index.html"))
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
/// anyone connect as anyone and take their wallet.
async fn arena_route(
  req: HttpRequest,
  stream: web::Payload,
  path: web::Path<String>,
  query: web::Query<ArenaQuery>,
  services: web::Data<Services>,
) -> Result<HttpResponse, actix_web::Error> {
  let Ok(room_id) = path.into_inner().parse::<uuid::Uuid>() else {
    return Ok(HttpResponse::BadRequest().body("malformed room id"));
  };
  let Some(entry) = services.rooms.get(&room_id) else {
    return Ok(HttpResponse::NotFound().body("no such arena"));
  };
  let Some(token) = query.into_inner().t else {
    return Ok(HttpResponse::Unauthorized().body("no ticket: ask the lobby for a placement"));
  };
  let Some(ticket) = services.tickets.redeem(&token) else {
    return Ok(HttpResponse::Unauthorized().body("ticket unknown or already spent"));
  };
  if ticket.room != room_id {
    return Ok(HttpResponse::Forbidden().body("ticket is for a different arena"));
  }

  info!(player = ticket.player, room = %room_id, "Arena connection opening.");
  entry
    .session
    .handle_connection(&req, stream, Agent::new_human(ticket.player))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("plaza_example_lobby_world=debug".parse().unwrap()),
    )
    .init();

  let wallets = Arc::new(WalletRegistry::new());
  let rooms = Arc::new(RoomRegistry::new());
  let tickets = Arc::new(TicketRegistry::new());

  let factory = Arc::new(ArenaFactory::new(wallets.clone(), rooms.clone(), BIND.to_string()));
  let manager = Arc::new(InMemoryLobbyManager::new(factory));

  for arena in ARENAS.iter() {
    let settings = RoomSettings {
      name: Some(arena.name.to_string()),
      game_mode: "coin-pot".to_string(),
      max_players: arena.max_players,
      is_private: false,
      password_hash: None,
      custom_game_settings: arena.settings,
    };
    match manager.handle_create_room_request(&0, settings).await {
      Ok(metadata) => info!(
        arena = %metadata.name,
        room = %metadata.room_id,
        budget_ms = ?metadata.max_one_way_ms,
        seats = metadata.max_players,
        "Arena open."
      ),
      Err(e) => {
        error!("Could not open arena {}: {e}", arena.name);
        return Err(std::io::Error::other(e.to_string()));
      }
    }
  }

  let lobby_session: Arc<LobbySession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));
  let logic = Arc::new(LobbyLogic::new(
    manager.clone(),
    rooms.clone(),
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
    let rooms = rooms.clone();
    let tickets_for_sweep = tickets.clone();
    let wallets_for_sweep = wallets.clone();
    let fixed_arenas: std::collections::HashSet<_> = manager.rooms().iter().map(|h| h.id()).collect();
    tokio::spawn(async move {
      let mut ticker = tokio::time::interval(REAP_EVERY);
      let mut quiet: std::collections::HashMap<_, (u64, tokio::time::Instant)> = std::collections::HashMap::new();
      loop {
        ticker.tick().await;

        // The reaper's other half: a dynamic room that has carried no traffic
        // for a while is drained through the same flush-then-farewell close as
        // a kick, then told to shut down; the reap below collects the finished
        // handle on a later pass. Occupants hear `Closed` before the socket
        // goes, never a silent EOF.
        let now = tokio::time::Instant::now();
        for handle in manager.rooms() {
          let id = handle.id();
          if fixed_arenas.contains(&id) {
            continue;
          }
          let Some(entry) = rooms.get(&id) else { continue };
          let inbound = entry.session.stats().inbound();
          let (last_inbound, since) = quiet.entry(id).or_insert((inbound, now));
          if inbound != *last_inbound {
            (*last_inbound, *since) = (inbound, now);
            continue;
          }
          if now.duration_since(*since) < ROOM_IDLE_AFTER {
            continue;
          }
          let farewell = entry
            .session
            .encode_message(plaza::session::SessionMessage::system(vec![types::RoomOp::Closed {
              reason: "closed for inactivity".into(),
            }]))
            .ok();
          let told = entry.session.manager().disconnect_all(farewell);
          warn!(room = %id, told, "Idle arena drained; shutting it down.");
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
            warn!(room = %id, "Arena ended; dropping its socket registration.");
            rooms.remove(&id);
          }
        }
        // Outstanding tickets climbing means placements are handed out and
        // never dialled.
        info!(
          arenas = manager.rooms().len(),
          wallets = wallets_for_sweep.tracked(),
          tickets_outstanding = tickets_for_sweep.outstanding(),
          "Lobby sweep."
        );
      }
    });
  }

  let services = web::Data::new(Services {
    lobby_session,
    rooms,
    tickets,
    ids: PlayerIds(AtomicU64::new(1)),
  });

  info!("Lobby world on http://{BIND} ({} arenas)", ARENAS.len());

  HttpServer::new(move || {
    App::new()
      .app_data(services.clone())
      .route("/", web::get().to(index))
      .route("/ws/lobby", web::get().to(lobby_route))
      .route("/ws/room/{room_id}", web::get().to(arena_route))
  })
  .bind(BIND)?
  .run()
  .await
}
