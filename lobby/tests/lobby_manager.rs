//! Coverage for `InMemoryLobbyManager`: room creation, join authorization,
//! password checks, filtering, reaping, and the two concurrency bugs this crate
//! has already produced.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::error::SnapshotError;
use plaza::session::InProcessSession;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_lobby::{
  InMemoryLobbyManager, InProcessRoomHandle, JoinRoomRequestPayload, LobbyError, RoomFactory, RoomFilters, RoomHandle,
  RoomId, RoomSettings,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type PlayerId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
enum GameOp {
  Noop,
  Snapshot(Box<GameState>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GameState {
  ticks: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GameSettings {
  difficulty: u8,
  /// What this room's schedule can carry, so the tests can build rooms that
  /// differ in the one thing latency routing sorts on.
  max_one_way_ms: Option<u32>,
}

#[derive(Debug, Default)]
struct GameLogic;

#[async_trait]
impl StateLogic<GameOp, PlayerId, GameState> for GameLogic {
  async fn process_input(
    &self,
    state: &mut GameState,
    input: LogicInput<GameOp, PlayerId>,
  ) -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError> {
    if let LogicInput::TimeStep { .. } = input {
      state.ticks += 1;
    }
    Ok(LogicOutput::none())
  }
}

#[derive(Debug, Default)]
struct GameSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, GameState, GameOp> for GameSnapshotter {
  async fn create_snapshot(
    &self,
    state: &GameState,
    _target: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<GameOp>, SnapshotError<PlayerId>> {
    Ok(Some(GameOp::Snapshot(Box::new(state.clone()))))
  }
}

/// Spawns real controllers, so handles behave the way production ones do.
#[derive(Debug, Default)]
struct TestRoomFactory {
  /// When set, `spawn_room` fails: exercising the error path.
  fail: bool,
  /// The concrete handles, kept beside the lobby's trait objects.
  ///
  /// This is the pattern an application uses when it needs something of its
  /// rooms that the seam does not carry: the factory built them, so it can keep
  /// them. Putting those methods on `RoomHandle` would name `GameOp` and
  /// `GameStateType` on a trait whose whole point is not to.
  spawned: Mutex<HashMap<RoomId, Arc<InProcessRoomHandle<GameOp, PlayerId, GameState, GameSettings>>>>,
}

impl TestRoomFactory {
  fn concrete(&self, room_id: &RoomId) -> Arc<InProcessRoomHandle<GameOp, PlayerId, GameState, GameSettings>> {
    Arc::clone(self.spawned.lock().get(room_id).expect("the factory spawned it"))
  }
}

#[async_trait]
impl RoomFactory for TestRoomFactory {
  type CustomGameSettings = GameSettings;
  type GameOp = GameOp;
  type GameID = PlayerId;
  type GameStateType = GameState;

  async fn spawn_room(
    &self,
    room_id: RoomId,
    settings: &RoomSettings<GameSettings>,
  ) -> Result<Arc<dyn RoomHandle<PlayerId, GameSettings>>, LobbyError> {
    if self.fail {
      return Err(LobbyError::RoomSpawnFailed("factory told to fail".into()));
    }

    let session = InProcessSession::<GameOp, PlayerId>::new();
    let (command_tx, controller) = StateControllerBuilder::new(
      Arc::new(GameLogic),
      session,
      Arc::new(GameSnapshotter),
      GameState::default(),
    )
    .build();
    let handle = tokio::spawn(controller.run());

    let metadata = plaza_lobby::RoomMetadata {
      room_id,
      name: settings.name.clone().unwrap_or_else(|| "room".into()),
      game_mode: settings.game_mode.clone(),
      current_players: 0,
      max_players: settings.max_players,
      has_password: settings.password_hash.is_some(),
      max_one_way_ms: settings.custom_game_settings.max_one_way_ms,
      custom_game_settings_summary: settings.custom_game_settings.clone(),
    };

    let room = Arc::new(InProcessRoomHandle::new(
      room_id,
      metadata,
      command_tx,
      handle,
      format!("ws://test/game/{room_id}"),
      settings.password_hash.clone(),
    ));
    self.spawned.lock().insert(room_id, Arc::clone(&room));
    Ok(room)
  }
}

/// A room that will only take connections inside `max_one_way_ms`.
fn settings_with_budget(max_players: u32, budget_ms: Option<u32>) -> RoomSettings<GameSettings> {
  let mut s = settings(max_players, None);
  s.custom_game_settings.max_one_way_ms = budget_ms;
  s
}

fn settings(max_players: u32, password: Option<&str>) -> RoomSettings<GameSettings> {
  RoomSettings {
    name: Some("test room".into()),
    game_mode: "deathmatch".into(),
    max_players,
    is_private: password.is_some(),
    password_hash: password.map(str::to_string),
    custom_game_settings: GameSettings::default(),
  }
}

fn manager() -> InMemoryLobbyManager<TestRoomFactory> {
  manager_with_factory().0
}

/// The lobby plus the factory that built its rooms, for a test that needs
/// something of a room the seam does not carry.
fn manager_with_factory() -> (InMemoryLobbyManager<TestRoomFactory>, Arc<TestRoomFactory>) {
  let factory = Arc::new(TestRoomFactory::default());
  (InMemoryLobbyManager::new(Arc::clone(&factory)), factory)
}

fn player() -> (PlayerId, Agent<PlayerId>) {
  let id = Uuid::new_v4();
  (id, Agent::new_human(id))
}

#[tokio::test]
async fn creating_a_room_returns_its_metadata_and_lists_it() {
  let lobby = manager();
  let (id, _) = player();

  let metadata = lobby
    .handle_create_room_request(&id, settings(4, None))
    .await
    .expect("spawn");

  assert_eq!(metadata.max_players, 4);
  assert!(!metadata.has_password);
  assert_eq!(lobby.list_rooms(None).len(), 1);
}

#[tokio::test]
async fn a_failing_factory_surfaces_its_error() {
  let lobby = InMemoryLobbyManager::new(Arc::new(TestRoomFactory { fail: true, ..Default::default() }));
  let (id, _) = player();

  let result = lobby.handle_create_room_request(&id, settings(4, None)).await;
  assert!(matches!(result, Err(LobbyError::RoomSpawnFailed(_))));
  assert!(lobby.list_rooms(None).is_empty(), "a failed spawn leaves no room");
}

/// Regression: `room.rs` once locked the same `parking_lot` mutex twice in one
/// expression, which deadlocked on the first join. A hang here means it is back.
#[tokio::test]
async fn joining_a_room_does_not_deadlock() {
  let lobby = manager();
  let (id, agent) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(4, None))
    .await
    .expect("spawn");

  let outcome = tokio::time::timeout(
    Duration::from_secs(5),
    lobby.handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: metadata.room_id,
        password_attempt: None,
        measured_one_way_ms: None,
      },
    ),
  )
  .await
  .expect("join must not hang")
  .expect("join should succeed");

  assert!(outcome.success);
  assert_eq!(outcome.room_session_endpoint.as_deref(), Some(&*format!("ws://test/game/{}", metadata.room_id)));
}

#[tokio::test]
async fn joining_an_unknown_room_is_rejected() {
  let lobby = manager();
  let (id, agent) = player();

  let result = lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: Uuid::new_v4(),
        password_attempt: None,
        measured_one_way_ms: None,
      },
    )
    .await;

  assert!(matches!(result, Err(LobbyError::RoomNotFound(_))));
}

#[tokio::test]
async fn a_private_room_requires_the_right_password() {
  let lobby = manager();
  let (id, agent) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(4, Some("hunter2")))
    .await
    .expect("spawn");
  assert!(metadata.has_password);

  let attempt = |password: Option<&str>| JoinRoomRequestPayload {
    room_id: metadata.room_id,
    password_attempt: password.map(str::to_string),
    measured_one_way_ms: None,
  };

  assert!(
    lobby
      .handle_join_room_request(&id, agent.clone(), &attempt(None))
      .await
      .is_err(),
    "missing password"
  );
  assert!(
    lobby
      .handle_join_room_request(&id, agent.clone(), &attempt(Some("wrong")))
      .await
      .is_err(),
    "wrong password"
  );
  assert!(
    lobby
      .handle_join_room_request(&id, agent, &attempt(Some("hunter2")))
      .await
      .is_ok(),
    "correct password"
  );
}

#[tokio::test]
async fn a_custom_verifier_replaces_the_default_comparison() {
  // Stands in for argon2: the stored "hash" is the attempt reversed.
  let lobby = InMemoryLobbyManager::new(Arc::new(TestRoomFactory::default()))
    .with_password_verifier(Arc::new(|attempt, stored| attempt.chars().rev().collect::<String>() == stored));

  let (id, agent) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(4, Some("2retnuh")))
    .await
    .expect("spawn");

  let outcome = lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: metadata.room_id,
        password_attempt: Some("hunter2".into()),
        measured_one_way_ms: None,
      },
    )
    .await;

  assert!(outcome.is_ok(), "the custom verifier decided, not string equality");
}

#[tokio::test]
async fn a_full_room_refuses_new_players() {
  let (lobby, factory) = manager_with_factory();
  let (id, agent) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(1, None))
    .await
    .expect("spawn");

  // The room's own session owns the player count; simulate it filling up
  // through the factory's own handle, since the lobby holds a seam that
  // deliberately cannot reach in and do this.
  factory.concrete(&metadata.room_id).update_player_count_in_metadata(1);

  let result = lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: metadata.room_id,
        password_attempt: None,
        measured_one_way_ms: None,
      },
    )
    .await;

  assert!(matches!(result, Err(LobbyError::JoinRoomFailed(_))));
}

#[tokio::test]
async fn filters_narrow_the_room_list() {
  let lobby = manager();
  let (id, _) = player();

  let mut deathmatch = settings(4, None);
  deathmatch.game_mode = "deathmatch".into();
  lobby.handle_create_room_request(&id, deathmatch).await.expect("spawn");

  let mut ctf = settings(4, None);
  ctf.game_mode = "capture-the-flag".into();
  lobby.handle_create_room_request(&id, ctf).await.expect("spawn");

  let private = settings(4, Some("secret"));
  lobby.handle_create_room_request(&id, private).await.expect("spawn");

  assert_eq!(lobby.list_rooms(None).len(), 3, "no filter lists everything");

  let by_mode = lobby.list_rooms(Some(&RoomFilters {
    game_mode: Some("capture-the-flag".into()),
    ..Default::default()
  }));
  assert_eq!(by_mode.len(), 1);

  let public_only = lobby.list_rooms(Some(&RoomFilters {
    exclude_private_if_no_password_known: Some(true),
    ..Default::default()
  }));
  assert_eq!(public_only.len(), 2, "the private room is hidden");
}

/// Regression: `is_finished` once used `try_lock` and treated contention as
/// "finished", which reaped rooms whose players were still in them.
#[tokio::test]
async fn a_live_room_is_not_reaped() {
  let lobby = manager();
  let (id, _) = player();
  lobby
    .handle_create_room_request(&id, settings(4, None))
    .await
    .expect("spawn");

  lobby.reap_finished_rooms().await;
  assert_eq!(lobby.list_rooms(None).len(), 1, "a running room must survive reaping");
}

#[tokio::test]
async fn a_finished_room_is_reaped() {
  let lobby = manager();
  let (id, _) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(4, None))
    .await
    .expect("spawn");

  lobby.room(&metadata.room_id).expect("room exists").request_shutdown().await;
  // Give the controller task a moment to actually finish.
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      lobby.reap_finished_rooms().await;
      if lobby.list_rooms(None).is_empty() {
        return;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("a stopped room should be reaped");
}

#[tokio::test]
async fn a_player_leaving_the_lobby_is_forwarded_to_their_room() {
  let lobby = manager();
  let (id, agent) = player();
  let metadata = lobby
    .handle_create_room_request(&id, settings(4, None))
    .await
    .expect("spawn");

  lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: metadata.room_id,
        password_attempt: None,
        measured_one_way_ms: None,
      },
    )
    .await
    .expect("join");

  // Must not hang: this path once held a lock across an await.
  tokio::time::timeout(Duration::from_secs(5), lobby.handle_player_leaving_lobby(&id))
    .await
    .expect("leaving must not hang");
}

#[tokio::test]
async fn a_player_who_never_joined_a_room_leaves_cleanly() {
  let lobby = manager();
  let (id, _) = player();
  tokio::time::timeout(Duration::from_secs(5), lobby.handle_player_leaving_lobby(&id))
    .await
    .expect("leaving must not hang");
}

#[tokio::test]
async fn a_connection_too_slow_for_a_room_is_refused_with_both_numbers() {
  // The refusal a client can act on. A room that schedules inputs ahead can only
  // carry a connection whose delay fits the schedule; past that the player is
  // seated and then silently loses every input, which reads as a broken game.
  // Refused here instead, and with the measurement attached rather than a string.
  let lobby = manager();
  let (owner, _) = player();
  let created = lobby
    .handle_create_room_request(&owner, settings_with_budget(4, Some(80)))
    .await
    .expect("room created");

  let (id, agent) = player();
  let outcome = lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: created.room_id,
        password_attempt: None,
        measured_one_way_ms: Some(250),
      },
    )
    .await;

  match outcome {
    Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => {
      assert_eq!((measured_ms, allowed_ms), (250, 80), "the refusal carries what was measured and what is allowed");
    }
    other => panic!("expected a latency refusal, got {other:?}"),
  }
}

#[tokio::test]
async fn a_room_with_no_budget_takes_anybody() {
  // The default has to stay open. A game that applies input on arrival has no
  // schedule to miss, so it states no limit and a latency check must not invent
  // one for it.
  let lobby = manager();
  let (owner, _owner_agent) = player();
  let created = lobby
    .handle_create_room_request(&owner, settings_with_budget(4, None))
    .await
    .expect("room created");

  let (id, agent) = player();
  let outcome = lobby
    .handle_join_room_request(
      &id,
      agent,
      &JoinRoomRequestPayload {
        room_id: created.room_id,
        password_attempt: None,
        measured_one_way_ms: Some(4000),
      },
    )
    .await
    .expect("a room with no limit accepts any connection");
  assert!(outcome.success);
}

#[tokio::test]
async fn a_slow_connection_is_routed_rather_than_turned_away() {
  // Why this belongs to a lobby rather than to a room. A room can only say yes
  // or no; a lobby can say *where*. Given rooms with different schedules, a slow
  // link gets the one built for it instead of a door slam, and refusal is what
  // is left when nothing fits.
  let lobby = manager();
  for budget in [Some(50u32), Some(300), None] {
    let (owner, _owner_agent) = player();
    lobby
      .handle_create_room_request(&owner, settings_with_budget(4, budget))
      .await
      .expect("room created");
  }

  let fast = lobby.rooms_playable_at(20);
  assert_eq!(fast.len(), 3, "a fast link can play anywhere");
  assert_eq!(fast[0].max_one_way_ms, Some(50), "and is offered the tightest room first, not the one built for slow links");

  let slow = lobby.rooms_playable_at(200);
  assert_eq!(slow.len(), 2, "a slow link loses the tightest room and keeps the rest");
  assert_eq!(slow[0].max_one_way_ms, Some(300));
  assert_eq!(slow.last().unwrap().max_one_way_ms, None, "the unlimited room sorts last: it takes anybody, so it is the fallback");

  assert!(lobby.rooms_playable_at(5000).len() == 1, "past every stated budget only the unlimited room is left");
}
