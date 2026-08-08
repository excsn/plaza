//! The bench, scripted: no window, no socket. Two editors work the shipped
//! vocabularies end to end: a lock granted and denied, paints under it, a
//! roster ordered and re-ordered, presence relayed, and the crossing: a
//! playtest whose bomb_grid bombs carve the wall that was just painted, after
//! which the bench still holds the authored map.

use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, ControllerCommand, StateControllerBuilder},
  session::InProcessSession,
  tick_driver::TickDriver,
};
use tracing::{error, info};

use map_forge::logic::ForgeLogic;
use map_forge::protocol::{tile_key, ForgeOp, PlayerId, BOARD_OBJECT, SPAWN_LIST, TILE_SOFT};
use map_forge::snapshot::ForgeSnapshotter;
use map_forge::state::ForgeState;

use plaza::app_common::locking::op_payloads::RequestLockPayload;
use plaza::app_common::object_property_ops::op_payloads::SetObjectPropertyPayload;
use plaza::app_common::ordered_collection_ops::op_payloads::{InsertListItemPayload, MoveListItemPayload};
use plaza::app_common::presence::op_payloads::UpdatePresencePayload;
use plaza::app_common::presence::payload_fragments::{ActivityStatusPayload, CursorPositionPayload};

type ForgeSession = InProcessSession<ForgeOp, PlayerId>;

const TICK: Duration = Duration::from_millis(map_forge::protocol::TICK_MS);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  plaza_session::host::init_logging();
  info!("Plaza Map Forge - scripted");

  let session = ForgeSession::new();
  let (commands, controller) = StateControllerBuilder::new(
    Arc::new(ForgeLogic),
    session.clone(),
    Arc::new(ForgeSnapshotter),
    ForgeState::new(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(async move {
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
  });
  let ticker = tokio::spawn(TickDriver::new(TICK).run(commands.clone()));

  let wren = Agent::new_human(1);
  let roy = Agent::new_human(2);
  let (_w, _wi) = session.connect(wren.clone()).await?;
  let (_r, _ri) = session.connect(roy.clone()).await?;

  let lock = |region: &str| {
    ForgeOp::RequestLock(RequestLockPayload {
      resource_id: region.to_string(),
    })
  };
  let paint = |x: u8, y: u8| {
    ForgeOp::SetTile(SetObjectPropertyPayload {
      object_id: BOARD_OBJECT.to_string(),
      property_key: tile_key(x, y),
      value: TILE_SOFT.to_string(),
    })
  };

  info!("--- Wren locks the north-west; Roy asks for the same region and is told no");
  send(&session, &wren, lock("north-west")).await;
  send(&session, &roy, lock("north-west")).await;

  info!("--- Roy paints into Wren's region anyway and the paint is refused; his own region takes it");
  send(&session, &roy, paint(2, 3)).await;
  send(&session, &roy, lock("north-east")).await;
  send(&session, &roy, paint(9, 3)).await;
  send(&session, &wren, paint(3, 2)).await;

  info!("--- the spawn roster fills in order, then re-orders");
  for (id, cell) in [(10u32, (2u8, 2u8)), (11, (12, 10))] {
    send(&session, &wren, ForgeOp::InsertSpawn(InsertListItemPayload {
      collection_key: SPAWN_LIST.to_string(),
      item_id: id,
      item_payload: cell,
      after_item_id: None,
      at_index: None,
    }))
    .await;
  }
  send(&session, &roy, ForgeOp::MoveSpawn(MoveListItemPayload {
    collection_key: SPAWN_LIST.to_string(),
    item_id_to_move: 11,
    new_after_item_id: None,
    new_index: Some(0),
  }))
  .await;

  info!("--- a cursor crosses the bench, relayed as presence");
  send(&session, &wren, ForgeOp::Presence(UpdatePresencePayload {
    details: map_forge::protocol::ForgePresence {
      cursor: CursorPositionPayload {
        x: 3.0,
        y: 2.0,
        context_id: None,
      },
      status: ActivityStatusPayload::Editing {
        resource_name: BOARD_OBJECT.to_string(),
      },
    },
  }))
  .await;

  info!("--- the crossing: the authored board goes live under bomb_grid's rules");
  send(&session, &wren, ForgeOp::StartPlaytest).await;
  send(&session, &roy, ForgeOp::Bomb).await;
  tokio::time::sleep(Duration::from_secs(5)).await;
  send(&session, &wren, ForgeOp::EndPlaytest).await;
  settle().await;

  let (board_kept, meters, spawn_order) = query_with(&commands, |state: &ForgeState| {
    (
      state.board.get(&tile_key(3, 2)).cloned(),
      state.meters,
      state.spawns.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    )
  })
  .await?;

  info!(
    "--- meters: {} paints applied, {} refused, {} lock denial(s), {} presence update(s), {} wall(s) carved",
    meters.paints_applied, meters.paints_refused, meters.lock_denials, meters.presence_updates, meters.walls_carved
  );
  info!("--- roster order after the move: {spawn_order:?}");
  assert_eq!(meters.lock_denials, 1);
  assert_eq!(meters.paints_refused, 1);
  assert_eq!(meters.paints_applied, 2);
  assert_eq!(spawn_order, vec![11, 10]);
  assert!(meters.walls_carved > 0, "the playtest carved an authored wall");
  assert_eq!(board_kept.as_deref(), Some(TILE_SOFT), "the artifact survives its playtest");

  info!("--- shutting down");
  commands.send(ControllerCommand::Shutdown).await?;
  ticker.abort();
  info!("Map Forge - Finished.");
  Ok(())
}

async fn send(session: &ForgeSession, who: &Agent<PlayerId>, op: ForgeOp) {
  session.client_send(who.clone(), vec![op]).await;
  settle().await;
}

async fn settle() {
  tokio::time::sleep(TICK * 3).await;
}
