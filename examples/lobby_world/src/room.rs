//! One arena: a pot that refills, and whoever claims it keeps the coins.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::participants::ParticipantTracker;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::snapshot::{SnapshotContext, SnapshotError, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError};

use crate::types::{ArenaSettings, Occupant, PlayerId, RoomOp, RoomView, Seat};
use crate::wallets::WalletRegistry;

const POT_STEP: u64 = 5;

/// Capped, or an idle arena turns server uptime into a payout.
const POT_CAP: u64 = 50;

/// Per-arena only. The wallet lives in the shared registry: it outlives this room.
#[derive(Debug, Clone)]
pub struct Occupancy {
  pub seat: Seat,
  pub claims_here: u32,
}

/// `Default` exists only to satisfy `RoomFactory::GameStateType`; the factory
/// always builds this from the room's settings.
#[derive(Debug, Clone, Default)]
pub struct ArenaState {
  pub arena: String,
  pub settings: ArenaSettings,
  pub max_players: u32,
  pub pot: u64,
  pub occupants: ParticipantTracker<PlayerId, Occupancy>,
  /// Admitted by the lobby but not yet connected. Without it an arena cannot
  /// tell an admitted player from a passer-by.
  pub reserved: HashSet<PlayerId>,
  pub since_refresh: Duration,
  pub wallets: Arc<WalletRegistry>,
  /// Read by the lobby to refresh `RoomMetadata::current_players`.
  pub seats_taken: Arc<AtomicU32>,
}

impl ArenaState {
  pub fn new(
    arena: String,
    settings: ArenaSettings,
    max_players: u32,
    wallets: Arc<WalletRegistry>,
    seats_taken: Arc<AtomicU32>,
  ) -> Self {
    Self {
      arena,
      settings,
      max_players,
      pot: POT_STEP,
      occupants: ParticipantTracker::new(),
      reserved: HashSet::new(),
      since_refresh: Duration::ZERO,
      wallets,
      seats_taken,
    }
  }

  fn seated_players(&self) -> u32 {
    self
      .occupants
      .iter()
      .filter(|(_, info)| info.app_data.seat == Seat::Player)
      .count() as u32
  }

  fn spectators(&self) -> u32 {
    self
      .occupants
      .iter()
      .filter(|(_, info)| info.app_data.seat == Seat::Spectator)
      .count() as u32
  }

  fn publish_seat_count(&self) {
    self.seats_taken.store(self.seated_players(), Ordering::Relaxed);
  }

  fn everyone(&self) -> Vec<Agent<PlayerId>> {
    self.occupants.iter().map(|(_, info)| info.agent.clone()).collect()
  }

  fn everyone_but(&self, exclude: &PlayerId) -> Vec<Agent<PlayerId>> {
    self
      .occupants
      .iter()
      .filter(|(id, _)| *id != exclude)
      .map(|(_, info)| info.agent.clone())
      .collect()
  }

  /// Built per recipient; only `your_seat` differs.
  pub fn view_for(&self, viewer: Option<&PlayerId>) -> RoomView {
    let mut occupants: Vec<Occupant> = self
      .occupants
      .iter()
      .map(|(id, info)| Occupant {
        player: *id,
        seat: info.app_data.seat,
        coins: self.wallets.balance(*id),
        claims_here: info.app_data.claims_here,
      })
      .collect();
    occupants.sort_by_key(|o| o.player);

    RoomView {
      arena: self.arena.clone(),
      budget_ms: self.settings.budget_ms,
      pot: self.pot,
      seats_taken: self.seated_players(),
      seats_total: self.max_players,
      spectators: self.spectators(),
      occupants,
      your_seat: viewer
        .and_then(|id| self.occupants.get_participant_app_data(id))
        .map(|occupancy| occupancy.seat),
    }
  }
}

pub struct ArenaLogic;

#[async_trait]
impl StateLogic<RoomOp, PlayerId, ArenaState> for ArenaLogic {
  async fn process_input(
    &self,
    state: &mut ArenaState,
    input: LogicInput<RoomOp, PlayerId>,
  ) -> Result<LogicOutput<RoomOp, PlayerId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(id) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };

        // Both checks: the lobby's capacity check and this connect are not
        // atomic, so the room may have filled in between.
        let admitted = state.reserved.remove(&id);
        let seat = if admitted && state.seated_players() < state.max_players {
          Seat::Player
        } else {
          Seat::Spectator
        };

        state.occupants.add_participant(agent, Occupancy { seat, claims_here: 0 });
        state.publish_seat_count();

        // The controller snapshots the joiner itself once this returns.
        Ok(LogicOutput::none().and_snapshot(SnapshotRequest::to(state.everyone_but(&id))))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.occupants.remove_participant(&agent_id);
        // The reservation deliberately survives: a room hop closes the old
        // socket after the new seat is reserved. Only `Withdraw` cancels.
        state.publish_seat_count();
        // Wallet untouched: surviving a room is the point.
        Ok(LogicOutput::none().and_snapshot(SnapshotRequest::to(state.everyone())))
      }

      LogicInput::TimeStep { delta_time } => {
        state.since_refresh += delta_time;
        let interval = Duration::from_millis(u64::from(state.settings.refresh_decis) * 100);
        if interval.is_zero() || state.since_refresh < interval {
          return Ok(LogicOutput::none());
        }
        state.since_refresh = Duration::ZERO;
        if state.pot >= POT_CAP {
          return Ok(LogicOutput::none());
        }
        state.pot = (state.pot + POT_STEP).min(POT_CAP);
        Ok(LogicOutput::ops(vec![TargetedOp::new_system_all(vec![
          RoomOp::PotRefreshed { pot: state.pot },
        ])]))
      }

      LogicInput::AgentOps { source, ops } => {
        let mut out = Vec::new();
        for op in ops {
          match op {
            RoomOp::Claim => {
              let Some(id) = source.id_cloned() else { continue };

              match state.occupants.get_participant_app_data(&id).map(|o| o.seat) {
                Some(Seat::Player) => {}
                Some(Seat::Spectator) => {
                  out.push(TargetedOp::new_system_to(
                    id,
                    vec![RoomOp::Rejected {
                      reason: "Spectators watch; they do not claim.".into(),
                    }],
                  ));
                  continue;
                }
                None => continue,
              }

              if state.pot == 0 {
                out.push(TargetedOp::new_system_to(
                  id,
                  vec![RoomOp::Rejected {
                    reason: "The pot is empty. Wait for it to refill.".into(),
                  }],
                ));
                continue;
              }

              let amount = std::mem::take(&mut state.pot);
              // To the registry, not this arena's state, so it survives the trip.
              let coins = state.wallets.credit(id, amount);
              if let Some(occupancy) = state.occupants.get_participant_app_data_mut(&id) {
                occupancy.claims_here += 1;
              }

              out.push(TargetedOp::new(
                source.clone(),
                MessageTarget::All,
                vec![RoomOp::Claimed {
                  player: id,
                  amount,
                  coins,
                }],
              ));
            }

            // No `authorize` hook ahead of `StateLogic`, so the sender check
            // lives in the rule.
            RoomOp::Reserve { player } => {
              if !source.is_system() {
                return Err(StateLogicError::InvalidOperation(
                  "Only the lobby may reserve a seat.".into(),
                ));
              }
              state.reserved.insert(player);
            }

            RoomOp::Withdraw { player } => {
              if !source.is_system() {
                return Err(StateLogicError::InvalidOperation(
                  "Only the lobby may cancel a reservation.".into(),
                ));
              }
              state.reserved.remove(&player);
            }

            // Server-to-client variants.
            other => {
              if !source.is_system() {
                return Err(StateLogicError::InvalidOperation(format!(
                  "Clients do not send {other:?}."
                )));
              }
            }
          }
        }

        let snapshot_everyone = !out.is_empty();
        let mut output = LogicOutput::ops(out);
        if snapshot_everyone {
          output = output.and_snapshot(SnapshotRequest::to(state.everyone()));
        }
        Ok(output)
      }
    }
  }
}

pub struct ArenaSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, ArenaState, RoomOp> for ArenaSnapshotter {
  async fn create_snapshot(
    &self,
    state: &ArenaState,
    target: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<RoomOp>, SnapshotError<PlayerId>> {
    let viewer = target.and_then(|agent| agent.id());
    Ok(Some(RoomOp::Snapshot(Box::new(state.view_for(viewer)))))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn arena() -> ArenaState {
    ArenaState::new(
      "test".into(),
      ArenaSettings {
        refresh_decis: 10,
        budget_ms: Some(50),
      },
      2,
      Arc::new(WalletRegistry::new()),
      Arc::new(AtomicU32::new(0)),
    )
  }

  async fn join(state: &mut ArenaState, id: PlayerId) {
    ArenaLogic
      .process_input(state, LogicInput::AgentJoined {
        agent: Agent::new_human(id),
      })
      .await
      .unwrap();
  }

  async fn claim(state: &mut ArenaState, id: PlayerId) -> LogicOutput<RoomOp, PlayerId> {
    ArenaLogic
      .process_input(state, LogicInput::AgentOps {
        source: Agent::new_human(id),
        ops: vec![RoomOp::Claim],
      })
      .await
      .unwrap()
  }

  async fn reserve(state: &mut ArenaState, id: PlayerId) {
    ArenaLogic
      .process_input(state, LogicInput::AgentOps {
        source: Agent::system(),
        ops: vec![RoomOp::Reserve { player: id }],
      })
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn an_unreserved_arrival_is_a_spectator() {
    let mut state = arena();
    join(&mut state, 1).await;
    assert_eq!(state.view_for(Some(&1)).your_seat, Some(Seat::Spectator));
    assert_eq!(state.seated_players(), 0);
  }

  #[tokio::test]
  async fn a_reserved_arrival_takes_a_seat() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    join(&mut state, 1).await;
    assert_eq!(state.view_for(Some(&1)).your_seat, Some(Seat::Player));
    assert_eq!(state.seats_taken.load(Ordering::Relaxed), 1);
  }

  /// Admitted, but the seats went while the client was opening its socket.
  #[tokio::test]
  async fn a_reservation_that_loses_the_race_becomes_a_spectator() {
    let mut state = arena();
    for id in 1..=3 {
      reserve(&mut state, id).await;
      join(&mut state, id).await;
    }
    assert_eq!(state.seated_players(), 2, "capacity holds");
    assert_eq!(state.view_for(Some(&3)).your_seat, Some(Seat::Spectator));
  }

  #[tokio::test]
  async fn spectators_do_not_consume_capacity() {
    let mut state = arena();
    join(&mut state, 1).await;
    join(&mut state, 2).await;
    join(&mut state, 3).await;
    assert_eq!(state.spectators(), 3);
    assert_eq!(state.seats_taken.load(Ordering::Relaxed), 0);
  }

  #[tokio::test]
  async fn a_claim_credits_the_registry_not_the_arena() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    join(&mut state, 1).await;
    let pot = state.pot;
    claim(&mut state, 1).await;
    assert_eq!(state.pot, 0);
    assert_eq!(state.wallets.balance(1), pot);
  }

  #[tokio::test]
  async fn a_spectator_cannot_claim() {
    let mut state = arena();
    join(&mut state, 1).await;
    let before = state.pot;
    claim(&mut state, 1).await;
    assert_eq!(state.pot, before, "the pot is untouched");
    assert_eq!(state.wallets.balance(1), 0);
  }

  #[tokio::test]
  async fn a_client_cannot_reserve_its_own_seat() {
    let mut state = arena();
    let result = ArenaLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(1),
        ops: vec![RoomOp::Reserve { player: 1 }],
      })
      .await;
    assert!(result.is_err());
    assert!(state.reserved.is_empty());
  }

  async fn withdraw(state: &mut ArenaState, id: PlayerId) {
    ArenaLogic
      .process_input(state, LogicInput::AgentOps {
        source: Agent::system(),
        ops: vec![RoomOp::Withdraw { player: id }],
      })
      .await
      .unwrap();
  }

  /// A room hop closes the old socket after the new seat is reserved.
  #[tokio::test]
  async fn a_closing_socket_does_not_cancel_a_reservation() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    ArenaLogic
      .process_input(&mut state, LogicInput::AgentLeft { agent_id: 1 })
      .await
      .unwrap();
    assert!(state.reserved.contains(&1), "the seat is still held");

    join(&mut state, 1).await;
    assert_eq!(state.view_for(Some(&1)).your_seat, Some(Seat::Player));
  }

  #[tokio::test]
  async fn the_lobby_can_cancel_a_reservation() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    withdraw(&mut state, 1).await;
    assert!(state.reserved.is_empty());

    join(&mut state, 1).await;
    assert_eq!(state.view_for(Some(&1)).your_seat, Some(Seat::Spectator));
  }

  #[tokio::test]
  async fn a_client_cannot_cancel_someone_elses_reservation() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    let result = ArenaLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(2),
        ops: vec![RoomOp::Withdraw { player: 1 }],
      })
      .await;
    assert!(result.is_err());
    assert!(state.reserved.contains(&1));
  }

  #[tokio::test]
  async fn leaving_frees_the_seat_but_keeps_the_wallet() {
    let mut state = arena();
    reserve(&mut state, 1).await;
    join(&mut state, 1).await;
    claim(&mut state, 1).await;
    let earned = state.wallets.balance(1);
    assert!(earned > 0);

    ArenaLogic
      .process_input(&mut state, LogicInput::AgentLeft { agent_id: 1 })
      .await
      .unwrap();

    assert_eq!(state.seats_taken.load(Ordering::Relaxed), 0);
    assert_eq!(state.wallets.balance(1), earned, "the baggage travels");
  }

  #[tokio::test]
  async fn the_pot_refills_on_its_own_schedule() {
    let mut state = arena();
    let before = state.pot;
    ArenaLogic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: Duration::from_millis(400),
      })
      .await
      .unwrap();
    assert_eq!(state.pot, before, "not yet");

    ArenaLogic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: Duration::from_millis(700),
      })
      .await
      .unwrap();
    assert_eq!(state.pot, before + POT_STEP);
  }

  #[tokio::test]
  async fn the_pot_stops_at_its_ceiling() {
    let mut state = arena();
    for _ in 0..500 {
      ArenaLogic
        .process_input(&mut state, LogicInput::TimeStep {
          delta_time: Duration::from_millis(1000),
        })
        .await
        .unwrap();
    }
    assert_eq!(state.pot, POT_CAP);
  }

  /// Silent at the ceiling, rather than chattering at the tick rate.
  #[tokio::test]
  async fn a_full_pot_announces_nothing() {
    let mut state = arena();
    state.pot = POT_CAP;
    let out = ArenaLogic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: Duration::from_secs(10),
      })
      .await
      .unwrap();
    assert!(out.ops.is_empty());
  }
}
