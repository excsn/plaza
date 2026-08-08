//! The bench's rules. The only place `ForgeState` changes.
//!
//! The collaborative surface is `app_common` consumed as shipped: the
//! [`LockManager`] answers every paint, the board mutates only through the
//! property payloads, the roster only through the collection payloads, and
//! presence is relayed rather than stored. What the application still writes
//! is exactly what those docs promise it must: the rules (a paint needs its
//! region's lock), the consequences (a denial is a counted refusal some
//! optimistic client now reverses), and the crossing into the game.

use async_trait::async_trait;
use bomb_grid::sim::protocol::Intent;
use bomb_grid::sim::server::Server;
use bomb_grid::sim::types::{Controls, Tile};
use plaza::agent::Agent;
use plaza::app_common::locking::op_payloads::{
  LockAcquiredNoticePayload, LockDeniedNoticePayload, LockReleasedNoticePayload,
};
use plaza::app_common::presence::op_payloads::PresenceChangedNoticePayload;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::protocol::{
  parse_tile_key, region_of, ForgeOp, ForgePhase, PlayerId, Refusal, TestFrame, BOARD_H, BOARD_OBJECT, BOARD_W,
  SEATS, SPAWN_LIST, TICK_MS, TILE_EMPTY, TILE_HARD, TILE_SOFT,
};
use crate::state::{ForgeState, Playtest};

type Ctx = OpsQueue<ForgeOp, PlayerId>;

#[derive(Debug, Default)]
pub struct ForgeLogic;

#[async_trait]
impl StateLogic<ForgeOp, PlayerId, ForgeState> for ForgeLogic {
  async fn process_input(
    &self,
    state: &mut ForgeState,
    input: LogicInput<ForgeOp, PlayerId>,
  ) -> Result<LogicOutput<ForgeOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = arrive(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        resnapshot = depart(state, agent_id, &mut ctx);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          resnapshot |= handle(state, player, op, &mut ctx);
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = step_playtest(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

fn refuse(ctx: &mut Ctx, player: PlayerId, why: Refusal) -> bool {
  warn!(player, ?why, "refused");
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![ForgeOp::Refused(why)]));
  false
}

fn arrive(state: &mut ForgeState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());
  if state.editors.len() < SEATS {
    state.editors.push(player);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![ForgeOp::YouAre(player)]));
    info!(player, "editor at the bench");
  } else {
    info!(player, "the bench is full; watching");
  }
  true
}

/// A leaver's locks are force-released with no releasing agent named, which
/// is exactly the shape the payload's `Option` exists for.
fn depart(state: &mut ForgeState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  if !state.is_editor(player) {
    return false;
  }
  state.editors.retain(|p| *p != player);
  for region in crate::protocol::REGIONS {
    let region = region.to_string();
    if state.locks.get_lock_owner(&region) == Some(&player) {
      state.locks.force_release_lock(&region);
      ctx
        .ops_q()
        .push(TargetedOp::new_system_all(vec![ForgeOp::LockReleased(
          LockReleasedNoticePayload {
            resource_id: region,
            by_agent_id: None,
          },
        )]));
    }
  }
  if let Some(test) = &mut state.playtest
    && let Some(seat) = test.seat_of.iter().position(|p| *p == player)
  {
    test.server.release_seat(seat);
  }
  info!(player, "left the bench; their locks fell open");
  true
}

fn handle(state: &mut ForgeState, player: PlayerId, op: ForgeOp, ctx: &mut Ctx) -> bool {
  if !state.is_editor(player) {
    return match op {
      ForgeOp::Presence(_) => false,
      _ => refuse(ctx, player, Refusal::Spectating),
    };
  }

  match op {
    ForgeOp::RequestLock(request) => {
      match state.locks.try_acquire_lock(&request.resource_id, player) {
        None => {
          ctx
            .ops_q()
            .push(TargetedOp::new_system_all(vec![ForgeOp::LockAcquired(
              LockAcquiredNoticePayload {
                resource_id: request.resource_id,
                by_agent_id: player,
              },
            )]));
          true
        }
        Some(owner) => {
          state.meters.lock_denials += 1;
          let denied = LockDeniedNoticePayload {
            resource_id: request.resource_id,
            reason: format!("held by P{owner}"),
          };
          ctx
            .ops_q()
            .push(TargetedOp::new_system_to(player, vec![ForgeOp::LockDenied(denied)]));
          true
        }
      }
    }

    ForgeOp::ReleaseLock(release) => {
      if state.locks.release_lock(&release.resource_id, &player) {
        ctx
          .ops_q()
          .push(TargetedOp::new_system_all(vec![ForgeOp::LockReleased(
            LockReleasedNoticePayload {
              resource_id: release.resource_id,
              by_agent_id: Some(player),
            },
          )]));
        true
      } else {
        false
      }
    }

    ForgeOp::SetTile(set) => {
      if state.phase != ForgePhase::Forge {
        return refuse(ctx, player, Refusal::WrongPhase);
      }
      let valid_value = matches!(set.value.as_str(), TILE_EMPTY | TILE_SOFT | TILE_HARD);
      let Some((x, y)) = parse_tile_key(&set.property_key).filter(|(x, y)| *x < BOARD_W && *y < BOARD_H) else {
        return refuse(ctx, player, Refusal::NoSuchTile);
      };
      if set.object_id != BOARD_OBJECT || !valid_value {
        return refuse(ctx, player, Refusal::NoSuchTile);
      }
      if state.locks.get_lock_owner(&region_of(x, y).to_string()) != Some(&player) {
        state.meters.paints_refused += 1;
        return refuse(ctx, player, Refusal::RegionNotLocked);
      }
      state.board.insert(set.property_key, set.value);
      state.meters.paints_applied += 1;
      true
    }

    ForgeOp::ClearTile(clear) => {
      if state.phase != ForgePhase::Forge {
        return refuse(ctx, player, Refusal::WrongPhase);
      }
      let Some((x, y)) = parse_tile_key(&clear.property_key) else {
        return refuse(ctx, player, Refusal::NoSuchTile);
      };
      if state.locks.get_lock_owner(&region_of(x, y).to_string()) != Some(&player) {
        state.meters.paints_refused += 1;
        return refuse(ctx, player, Refusal::RegionNotLocked);
      }
      state.board.remove(&clear.property_key);
      state.meters.paints_applied += 1;
      true
    }

    ForgeOp::InsertSpawn(insert) => {
      if insert.collection_key != SPAWN_LIST || state.spawns.iter().any(|(id, _)| *id == insert.item_id) {
        return refuse(ctx, player, Refusal::NoSuchTile);
      }
      let (x, y) = insert.item_payload;
      if x >= BOARD_W || y >= BOARD_H {
        return refuse(ctx, player, Refusal::NoSuchTile);
      }
      let index = match (insert.after_item_id, insert.at_index) {
        (Some(after), _) => state.spawns.iter().position(|(id, _)| *id == after).map(|i| i + 1),
        (None, Some(at)) => Some(at.min(state.spawns.len())),
        (None, None) => None,
      }
      .unwrap_or(state.spawns.len());
      state.spawns.insert(index, (insert.item_id, (x, y)));
      true
    }

    ForgeOp::RemoveSpawn(remove) => {
      let before = state.spawns.len();
      state.spawns.retain(|(id, _)| *id != remove.item_id_to_remove);
      state.spawns.len() != before
    }

    ForgeOp::MoveSpawn(mv) => {
      let Some(from) = state.spawns.iter().position(|(id, _)| *id == mv.item_id_to_move) else {
        return false;
      };
      let item = state.spawns.remove(from);
      let index = match (mv.new_after_item_id, mv.new_index) {
        (Some(after), _) => state.spawns.iter().position(|(id, _)| *id == after).map(|i| i + 1),
        (None, Some(at)) => Some(at.min(state.spawns.len())),
        (None, None) => None,
      }
      .unwrap_or(state.spawns.len());
      state.spawns.insert(index, item);
      true
    }

    ForgeOp::Presence(update) => {
      state.meters.presence_updates += 1;
      // Relayed, not stored: presence is a stream about now, and `app_common`
      // ships the envelope for exactly this.
      ctx
        .ops_q()
        .push(TargetedOp::new_system_all(vec![ForgeOp::PresenceChanged(
          PresenceChangedNoticePayload {
            agent_id: player,
            new_details: update.details,
          },
        )]));
      false
    }

    ForgeOp::StartPlaytest => {
      if state.phase != ForgePhase::Forge {
        return refuse(ctx, player, Refusal::WrongPhase);
      }
      start_playtest(state);
      true
    }

    ForgeOp::EndPlaytest => {
      if state.phase != ForgePhase::Playtest {
        return refuse(ctx, player, Refusal::WrongPhase);
      }
      state.playtest = None;
      state.phase = ForgePhase::Forge;
      info!("back to the bench; the authored board is untouched");
      true
    }

    ForgeOp::Walk(dir) => submit_intent(state, player, Intent::Walk(dir)),
    ForgeOp::Bomb => submit_intent(state, player, Intent::Bomb),

    _ => false,
  }
}

/// The crossing: the property store becomes bomb_grid's grid, the roster
/// becomes its seats, and from here on the rules are that crate's.
fn start_playtest(state: &mut ForgeState) {
  let party = state.editors.clone();
  let players = party.len().max(1);
  let mut server = Server::new(players, 7);
  server.grid = state.to_grid(players);
  let cells = state.spawn_cells(players);
  for (i, cell) in cells.iter().enumerate() {
    server.players[i].reset_for_round(*cell);
    server.take_seat(i);
  }
  state.playtest = Some(Playtest {
    server,
    controls: Controls::default(),
    seat_of: party,
  });
  state.phase = ForgePhase::Playtest;
  state.playtests_run += 1;
  info!(players, "playtest: the authored board goes live under bomb_grid's rules");
}

fn submit_intent(state: &mut ForgeState, player: PlayerId, intent: Intent) -> bool {
  let Some(test) = &mut state.playtest else {
    return false;
  };
  let Some(seat) = test.seat_of.iter().position(|p| *p == player) else {
    return false;
  };
  let tick = test.server.tick() + 1;
  let accepted = test.server.submit(seat, tick, intent, &test.controls);
  debug!(player, seat, accepted, "playtest intent");
  false
}

fn step_playtest(state: &mut ForgeState, ctx: &mut Ctx) -> bool {
  let Some(test) = &mut state.playtest else {
    return false;
  };
  let soft_before = test.server.grid.soft_walls();
  test.server.advance(TICK_MS, &test.controls);
  let carved = soft_before.saturating_sub(test.server.grid.soft_walls());
  state.meters.walls_carved += carved as u64;

  if state.tick % 2 == 0 {
    let grid = &test.server.grid;
    let mut tiles = Vec::with_capacity(BOARD_W as usize * BOARD_H as usize);
    for y in 0..BOARD_H {
      for x in 0..BOARD_W {
        tiles.push(match grid.get(bomb_grid::sim::types::Cell::new(x, y)) {
          Tile::Empty => 0,
          Tile::Soft => 1,
          Tile::Hard => 2,
        });
      }
    }
    let now = test.server.now_ms();
    let frame = TestFrame {
      tiles,
      players: test.server.players.iter().map(|p| p.draw_pos()).collect(),
      bombs: test
        .server
        .bombs
        .iter()
        .map(|b| ((b.cell.x, b.cell.y), b.fires_at_ms.saturating_sub(now)))
        .collect(),
      fire: test.server.fire_cells().iter().map(|c| (c.x, c.y)).collect(),
    };
    ctx
      .ops_q()
      .push(TargetedOp::new_system_all(vec![ForgeOp::Frame(Box::new(frame))]));
  }
  carved > 0
}

#[cfg(test)]
mod tests {
  use super::*;
  use plaza::app_common::locking::op_payloads::{ReleaseLockPayload, RequestLockPayload};
  use plaza::app_common::object_property_ops::op_payloads::SetObjectPropertyPayload;
  use plaza::app_common::ordered_collection_ops::op_payloads::{InsertListItemPayload, MoveListItemPayload};
  use crate::protocol::tile_key;

  async fn run(state: &mut ForgeState, input: LogicInput<ForgeOp, PlayerId>) {
    ForgeLogic.process_input(state, input).await.unwrap();
  }

  async fn act(state: &mut ForgeState, who: PlayerId, op: ForgeOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  async fn join(state: &mut ForgeState, player: PlayerId) {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(player),
    })
    .await;
  }

  async fn tick(state: &mut ForgeState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(TICK_MS),
    })
    .await;
  }

  fn lock(region: &str) -> ForgeOp {
    ForgeOp::RequestLock(RequestLockPayload {
      resource_id: region.to_string(),
    })
  }

  fn paint(x: u8, y: u8, tile: &str) -> ForgeOp {
    ForgeOp::SetTile(SetObjectPropertyPayload {
      object_id: BOARD_OBJECT.to_string(),
      property_key: tile_key(x, y),
      value: tile.to_string(),
    })
  }

  #[tokio::test]
  async fn a_lock_is_granted_denied_and_released_in_the_shipped_vocabulary() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;
    join(&mut state, 2).await;

    act(&mut state, 1, lock("north-west")).await;
    assert_eq!(state.locks.get_lock_owner(&"north-west".to_string()), Some(&1));

    act(&mut state, 2, lock("north-west")).await;
    assert_eq!(state.meters.lock_denials, 1, "the second hand is told no");

    act(&mut state, 1, ForgeOp::ReleaseLock(ReleaseLockPayload {
      resource_id: "north-west".to_string(),
    }))
    .await;
    act(&mut state, 2, lock("north-west")).await;
    assert_eq!(state.locks.get_lock_owner(&"north-west".to_string()), Some(&2));
  }

  #[tokio::test]
  async fn a_paint_needs_the_regions_lock() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;

    act(&mut state, 1, paint(2, 2, TILE_SOFT)).await;
    assert!(state.board.is_empty(), "no lock, no paint");
    assert_eq!(state.meters.paints_refused, 1, "the optimistic client now reverses it");

    act(&mut state, 1, lock("north-west")).await;
    act(&mut state, 1, paint(2, 2, TILE_SOFT)).await;
    assert_eq!(state.board.get(&tile_key(2, 2)).map(String::as_str), Some(TILE_SOFT));
    assert_eq!(state.meters.paints_applied, 1);
  }

  #[tokio::test]
  async fn a_leavers_locks_fall_open() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, lock("south-east")).await;

    run(&mut state, LogicInput::AgentLeft { agent_id: 1 }).await;
    assert_eq!(state.locks.get_lock_owner(&"south-east".to_string()), None);
  }

  #[tokio::test]
  async fn the_roster_keeps_its_order_through_insert_and_move() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;
    for (id, cell) in [(10, (1, 1)), (11, (5, 5)), (12, (9, 9))] {
      act(&mut state, 1, ForgeOp::InsertSpawn(InsertListItemPayload {
        collection_key: SPAWN_LIST.to_string(),
        item_id: id,
        item_payload: cell,
        after_item_id: None,
        at_index: None,
      }))
      .await;
    }
    assert_eq!(state.spawns.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![10, 11, 12]);

    act(&mut state, 1, ForgeOp::MoveSpawn(MoveListItemPayload {
      collection_key: SPAWN_LIST.to_string(),
      item_id_to_move: 12,
      new_after_item_id: None,
      new_index: Some(0),
    }))
    .await;
    assert_eq!(state.spawns.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![12, 10, 11]);
  }

  #[tokio::test]
  async fn the_property_store_round_trips_into_bomb_grids_grid() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, lock("north-west")).await;
    act(&mut state, 1, paint(3, 3, TILE_SOFT)).await;
    act(&mut state, 1, paint(4, 3, TILE_HARD)).await;

    let grid = state.to_grid(1);
    use bomb_grid::sim::types::Cell;
    assert_eq!(grid.get(Cell::new(3, 3)), Tile::Soft);
    assert_eq!(grid.get(Cell::new(4, 3)), Tile::Hard);
    assert_eq!(grid.get(Cell::new(5, 5)), Tile::Empty, "unset keys are open floor");
    assert_eq!(grid.get(Cell::new(0, 0)), Tile::Hard, "the ring is always wall");
  }

  #[tokio::test]
  async fn a_playtest_bomb_carves_the_authored_wall_and_the_bench_keeps_the_map() {
    let mut state = ForgeState::new();
    join(&mut state, 1).await;
    act(&mut state, 1, lock("north-west")).await;
    // A soft wall beside the spawn the roster names.
    act(&mut state, 1, ForgeOp::InsertSpawn(InsertListItemPayload {
      collection_key: SPAWN_LIST.to_string(),
      item_id: 1,
      item_payload: (2, 2),
      after_item_id: None,
      at_index: None,
    }))
    .await;
    act(&mut state, 1, paint(3, 2, TILE_SOFT)).await;

    act(&mut state, 1, ForgeOp::StartPlaytest).await;
    assert_eq!(state.phase, ForgePhase::Playtest);
    act(&mut state, 1, ForgeOp::Bomb).await;
    for _ in 0..400 {
      tick(&mut state).await;
    }
    assert!(state.meters.walls_carved > 0, "bomb_grid's blast took the authored wall");

    act(&mut state, 1, ForgeOp::EndPlaytest).await;
    assert_eq!(state.phase, ForgePhase::Forge);
    assert_eq!(
      state.board.get(&tile_key(3, 2)).map(String::as_str),
      Some(TILE_SOFT),
      "the artifact survives its own playtest"
    );
  }
}
