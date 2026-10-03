//! The reservation seam over a real socket: a player placed through
//! `reserve_seat` and connecting over TCP the moment it returns is seated.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::{query_with, StateControllerBuilder};
use plaza::snapshot::NoSnapshots;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_lobby::{InProcessRoomHandle, RoomHandle, RoomId, RoomMetadata};
use plaza_session::tcp::AgentFactory;
use plaza_session::TcpPlazaSession;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use uuid::Uuid;

type PlayerId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
enum TableOp {
  Reserve(PlayerId),
  Withdraw(PlayerId),
}

#[derive(Debug, Clone, Default)]
struct Table {
  reserved: Vec<PlayerId>,
  seated: Vec<PlayerId>,
  spectators: Vec<PlayerId>,
}

#[derive(Debug, Default)]
struct TableLogic;

#[async_trait]
impl StateLogic<TableOp, PlayerId, Table> for TableLogic {
  async fn process_input(
    &self,
    state: &mut Table,
    input: LogicInput<TableOp, PlayerId>,
  ) -> Result<LogicOutput<TableOp, PlayerId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        if let Some(player) = agent.id().copied() {
          if state.reserved.contains(&player) {
            state.reserved.retain(|held| *held != player);
            state.seated.push(player);
          } else {
            state.spectators.push(player);
          }
        }
      }
      LogicInput::AgentOps { source, ops } if source.is_system() => {
        for op in ops {
          match op {
            TableOp::Reserve(player) => state.reserved.push(player),
            TableOp::Withdraw(player) => state.reserved.retain(|held| *held != player),
          }
        }
      }
      _ => {}
    }
    Ok(LogicOutput::none())
  }
}

/// A room whose session is a TCP listener on an ephemeral port, admitting
/// every connection as `player`.
async fn room_for(player: PlayerId) -> (Arc<InProcessRoomHandle<TableOp, PlayerId, Table, ()>>, std::net::SocketAddr) {
  let agents: AgentFactory<PlayerId> = Arc::new(move |_peer| Ok(Agent::new_human(player)));
  let session: Arc<TcpPlazaSession<TableOp, PlayerId>> = TcpPlazaSession::bind("127.0.0.1:0", agents)
    .await
    .expect("bind on an ephemeral port");
  let addr = session.local_addr();

  let (commands, controller) =
    StateControllerBuilder::new(Arc::new(TableLogic), session, Arc::new(NoSnapshots), Table::default()).build();
  let handle = tokio::spawn(controller.run());

  let room_id = RoomId::new_v4();
  let metadata = RoomMetadata {
    room_id,
    name: "table".into(),
    game_mode: "cards".into(),
    current_players: 0,
    max_players: 4,
    has_password: false,
    max_one_way_ms: None,
    custom_game_settings_summary: (),
  };
  let room = InProcessRoomHandle::new(room_id, metadata, commands, handle, format!("tcp://{addr}"), None)
    .with_reservations(TableOp::Reserve, TableOp::Withdraw);
  (Arc::new(room), addr)
}

/// Polls the table until the player has been placed somewhere, since the join
/// travels through accept and registration rather than arriving in step.
async fn placed(room: &InProcessRoomHandle<TableOp, PlayerId, Table, ()>, player: PlayerId) -> Table {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let table = query_with(&room.command_tx, |table: &Table| table.clone())
        .await
        .expect("the room is running");
      if table.seated.contains(&player) || table.spectators.contains(&player) {
        return table;
      }
      tokio::time::sleep(Duration::from_millis(2)).await;
    }
  })
  .await
  .expect("the join never reached the table")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_who_connects_over_tcp_the_moment_reserve_seat_returns_is_seated() {
  for trial in 0..25 {
    let player = Uuid::new_v4();
    let (room, addr) = room_for(player).await;

    room.reserve_seat(&player).await.expect("the room takes reservations");
    let _socket = TcpStream::connect(addr).await.expect("connect");

    let table = placed(&room, player).await;
    assert_eq!(table.seated, vec![player], "trial {trial}: the join found no hold");
    assert!(table.spectators.is_empty(), "trial {trial}");
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_who_connects_over_tcp_the_moment_withdraw_seat_returns_is_a_spectator() {
  for trial in 0..25 {
    let player = Uuid::new_v4();
    let (room, addr) = room_for(player).await;

    room.reserve_seat(&player).await.expect("the room takes reservations");
    let held = query_with(&room.command_tx, |table: &Table| table.reserved.clone()).await.expect("running");
    assert_eq!(held, vec![player]);
    room.withdraw_seat(&player).await.expect("and withdrawals");
    let _socket = TcpStream::connect(addr).await.expect("connect");

    let table = placed(&room, player).await;
    assert_eq!(table.spectators, vec![player], "trial {trial}: the join still found the hold");
    assert!(table.seated.is_empty(), "trial {trial}");
    assert!(table.reserved.is_empty(), "trial {trial}");
  }
}
