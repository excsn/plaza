//! Spawning a table on demand: its own session, its own controller, and an
//! endpoint the lobby can hand out.
//!
//! Unlike `lobby_world`, nothing is pre-spawned. A table exists because a match
//! formed, which is the shape a card game wants and the shape a client that
//! dials a per-match endpoint needs.

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
use plaza_session::codec::MsgPackCodec;
use plaza_session::ActixWsPlazaSession;
use plaza_wire::frame::ProtocolVersion;
use tracing::info;

use crate::snapshot::TableSnapshotter;
use crate::table::TableLogic;
use crate::types::{PlayerId, TableOp, TableSettings, TableState, PROTOCOL};
use crate::wallets::WalletRegistry;

/// One per table: a session feeds exactly one controller.
///
/// **Named MessagePack, where the lobby is JSON.** Nothing about plaza ties a
/// deployment to one codec, because a codec belongs to a session and a session
/// belongs to a controller. The table is the wire a shipped client speaks and
/// the lobby is the one a browser tab reads, so they are encoded differently on
/// purpose, from one binary, over one port.
pub type TableSession = ActixWsPlazaSession<TableOp, PlayerId, MsgPackCodec>;

/// Turn timeouts are counted in ticks, and nothing here is latency-sensitive.
const TABLE_TICK_HZ: u32 = 20;

#[derive(Clone)]
pub struct TableEntry {
  pub session: Arc<TableSession>,
  /// The table's command channel, kept here rather than on its `RoomHandle`.
  ///
  /// The seam the lobby holds names neither `TableOp` nor `TableState`, which is
  /// what lets a room live somewhere else. An application that needs to speak
  /// to its rooms in their own vocabulary built them, so it keeps them.
  pub commands: CommandSender<TableOp, PlayerId, TableState>,
  pub room: Arc<InProcessRoomHandle<TableOp, PlayerId, TableState, TableSettings>>,
  /// An atomic rather than a controller query: the lobby reads this on every
  /// listing.
  pub seats: Arc<AtomicU32>,
}

/// Sockets by room id. Separate from the lobby's map, which holds seats and metadata.
#[derive(Default)]
pub struct TableRegistry {
  tables: Mutex<HashMap<RoomId, TableEntry>>,
}

impl TableRegistry {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn insert(&self, room_id: RoomId, entry: TableEntry) {
    self.tables.lock().insert(room_id, entry);
  }

  pub fn get(&self, room_id: &RoomId) -> Option<TableEntry> {
    self.tables.lock().get(room_id).cloned()
  }

  /// The command channel for one table, which the lobby's `RoomHandle`
  /// deliberately does not carry.
  pub fn commands(&self, room_id: &RoomId) -> Option<CommandSender<TableOp, PlayerId, TableState>> {
    self.tables.lock().get(room_id).map(|entry| entry.commands.clone())
  }

  pub fn room(&self, room_id: &RoomId) -> Option<Arc<InProcessRoomHandle<TableOp, PlayerId, TableState, TableSettings>>> {
    self.tables.lock().get(room_id).map(|entry| Arc::clone(&entry.room))
  }

  pub fn remove(&self, room_id: &RoomId) {
    self.tables.lock().remove(room_id);
  }

  pub fn len(&self) -> usize {
    self.tables.lock().len()
  }
}

pub struct TableFactory {
  wallets: Arc<WalletRegistry>,
  registry: Arc<TableRegistry>,
  /// `host:port`, so handed-out endpoints are dialable.
  authority: String,
}

impl TableFactory {
  pub fn new(wallets: Arc<WalletRegistry>, registry: Arc<TableRegistry>, authority: String) -> Self {
    Self {
      wallets,
      registry,
      authority,
    }
  }
}

#[async_trait]
impl RoomFactory for TableFactory {
  type CustomGameSettings = TableSettings;
  type GameOp = TableOp;
  type GameID = PlayerId;
  type GameStateType = TableState;

  async fn spawn_room(
    &self,
    room_id: RoomId,
    room_settings: &RoomSettings<Self::CustomGameSettings>,
  ) -> Result<Arc<dyn plaza_lobby::RoomHandle<Self::GameID, Self::CustomGameSettings>>, LobbyError> {
    let name = room_settings
      .name
      .clone()
      .unwrap_or_else(|| format!("table-{room_id}"));
    let seats = Arc::new(AtomicU32::new(0));
    let session: Arc<TableSession> = ActixWsPlazaSession::with_protocol(MsgPackCodec, ProtocolVersion(PROTOCOL));

    // Built from the room's settings, which is what `TableState::default` cannot
    // do and why that impl exists only to satisfy the trait bound.
    let state = TableState::new(
      name.clone(),
      room_settings.custom_game_settings,
      room_settings.max_players,
      self.wallets.clone(),
      seats.clone(),
    );

    let (commands, controller) =
      StateControllerBuilder::new(Arc::new(TableLogic), session.clone(), Arc::new(TableSnapshotter), state)
        .command_buffer(128)
        .build();

    let task = tokio::spawn(controller.run());
    tokio::spawn(TickDriver::from_hz(TABLE_TICK_HZ).run(commands.clone()));

    let endpoint = format!("ws://{}/ws/table/{}", self.authority, room_id);
    info!(room = %room_id, table = %name, endpoint = %endpoint, "Table opened.");

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
    self.registry.insert(room_id, TableEntry {
      session,
      seats,
      commands,
      room: Arc::clone(&room),
    });
    Ok(room)
  }
}
