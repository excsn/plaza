//! Spawning an arena on demand: its own session, its own controller, and an
//! endpoint the lobby can hand out.

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::controller::{CommandSender, StateControllerBuilder};
use plaza::tick_driver::TickDriver;
use plaza_lobby::factory::RoomFactory;
use plaza_lobby::op_payloads::RoomSettings;
use plaza_lobby::room::InProcessRoomHandle;
use plaza_lobby::{LobbyError, RoomId};
use plaza_session::codec::JsonCodec;
use plaza_session::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use tracing::info;

use crate::room::{ArenaLogic, ArenaSnapshotter, ArenaState};
use crate::types::{ArenaSettings, PlayerId, RoomOp, PROTOCOL};
use crate::wallets::WalletRegistry;

/// One per arena: a session feeds exactly one controller.
pub type ArenaSession = ActixWsPlazaSession<RoomOp, PlayerId>;

/// Enough to land the deci-second pot schedule; nothing here is latency-sensitive.
const ARENA_TICK_HZ: u32 = 20;

#[derive(Clone)]
pub struct ArenaEntry {
  pub session: Arc<ArenaSession>,
  /// The room's command channel, kept here rather than on its `RoomHandle`.
  ///
  /// The seam the lobby holds names neither `RoomOp` nor `ArenaState`, which is
  /// what lets a room live somewhere else. An application that needs to speak
  /// to its rooms in their own vocabulary built them, so it keeps them.
  pub commands: CommandSender<RoomOp, PlayerId, ArenaState>,
  /// The concrete handle, for the same reason as `commands`: the lobby's copy
  /// of a room's metadata is a cache, and refreshing it is the application's
  /// job because only the application knows the live count.
  pub room: Arc<InProcessRoomHandle<RoomOp, PlayerId, ArenaState, ArenaSettings>>,
  /// An atomic rather than a controller query: the lobby reads this on every
  /// room listing.
  pub seats: Arc<AtomicU32>,
}

/// Sockets by room id. Separate from the lobby's map, which holds seats and metadata.
#[derive(Default)]
pub struct RoomRegistry {
  arenas: Mutex<HashMap<RoomId, ArenaEntry>>,
}

impl RoomRegistry {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn insert(&self, room_id: RoomId, entry: ArenaEntry) {
    self.arenas.lock().insert(room_id, entry);
  }

  pub fn get(&self, room_id: &RoomId) -> Option<ArenaEntry> {
    self.arenas.lock().get(room_id).cloned()
  }

  /// The command channel for one arena, which the lobby's `RoomHandle`
  /// deliberately does not carry.
  pub fn commands(&self, room_id: &RoomId) -> Option<CommandSender<RoomOp, PlayerId, ArenaState>> {
    self.arenas.lock().get(room_id).map(|entry| entry.commands.clone())
  }

  /// The concrete handle for one arena.
  pub fn room(&self, room_id: &RoomId) -> Option<Arc<InProcessRoomHandle<RoomOp, PlayerId, ArenaState, ArenaSettings>>> {
    self.arenas.lock().get(room_id).map(|entry| Arc::clone(&entry.room))
  }

  pub fn remove(&self, room_id: &RoomId) {
    self.arenas.lock().remove(room_id);
  }
}

pub struct ArenaFactory {
  wallets: Arc<WalletRegistry>,
  registry: Arc<RoomRegistry>,
  /// `host:port`, so handed-out endpoints are dialable.
  authority: String,
}

impl ArenaFactory {
  pub fn new(wallets: Arc<WalletRegistry>, registry: Arc<RoomRegistry>, authority: String) -> Self {
    Self {
      wallets,
      registry,
      authority,
    }
  }
}

#[async_trait]
impl RoomFactory for ArenaFactory {
  type CustomGameSettings = ArenaSettings;
  type GameOp = RoomOp;
  type GameID = PlayerId;
  type GameStateType = ArenaState;

  async fn spawn_room(
    &self,
    room_id: RoomId,
    room_settings: &RoomSettings<Self::CustomGameSettings>,
  ) -> Result<Arc<dyn plaza_lobby::RoomHandle<Self::GameID, Self::CustomGameSettings>>, LobbyError> {
    let name = room_settings
      .name
      .clone()
      .unwrap_or_else(|| format!("arena-{room_id}"));
    let seats = Arc::new(AtomicU32::new(0));
    let session: Arc<ArenaSession> = ActixWsPlazaSession::with_protocol(JsonCodec, ProtocolVersion(PROTOCOL));

    let state = ArenaState::new(
      name.clone(),
      room_settings.custom_game_settings,
      room_settings.max_players,
      self.wallets.clone(),
      seats.clone(),
    );

    let (commands, controller) = StateControllerBuilder::new(
      Arc::new(ArenaLogic),
      session.clone(),
      Arc::new(ArenaSnapshotter),
      state,
    )
    .command_buffer(128)
    .build();

    let task = tokio::spawn(controller.run());
    tokio::spawn(TickDriver::from_hz(ARENA_TICK_HZ).run(commands.clone()));



    let endpoint = format!("ws://{}/ws/room/{}", self.authority, room_id);
    info!(room = %room_id, arena = %name, endpoint = %endpoint, "Arena spawned.");

    let metadata = plaza_lobby::op_payloads::RoomMetadata {
      room_id,
      name,
      game_mode: room_settings.game_mode.clone(),
      current_players: 0,
      max_players: room_settings.max_players,
      has_password: room_settings.password_hash.is_some(),
      max_one_way_ms: room_settings.custom_game_settings.budget_ms,
      custom_game_settings_summary: room_settings.custom_game_settings,
    };

    let room = Arc::new(InProcessRoomHandle::new(
      room_id,
      metadata,
      commands.clone(),
      task,
      endpoint,
      room_settings.password_hash.clone(),
    ));
    self.registry.insert(room_id, ArenaEntry {
      session,
      seats,
      commands,
      room: Arc::clone(&room),
    });
    Ok(room)
  }
}
