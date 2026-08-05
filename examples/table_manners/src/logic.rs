//! The party. Small on purpose: moderation is the subject.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};

use crate::moderation::{Host, GRACE};
use crate::types::{Guest, Parting, PartyOp, Seat, Table, SEATS};

#[derive(Debug, Clone)]
pub struct Player {
  pub agent: Agent<u64>,
  pub seat: Seat,
  pub said: u32,
}

#[derive(Debug, Default)]
pub struct PartyState {
  pub players: HashMap<u64, Player>,
  pub ended: bool,
}

#[derive(Debug)]
pub struct PartyLogic {
  pub host: Arc<Host>,
  /// Which agent key the host tools may be used from. A party has one host.
  pub host_key: parking_lot::Mutex<Option<u64>>,
}

#[async_trait]
impl StateLogic<PartyOp, u64, PartyState> for PartyLogic {
  async fn process_input(
    &self,
    state: &mut PartyState,
    input: LogicInput<PartyOp, u64>,
  ) -> Result<LogicOutput<PartyOp, u64>, StateLogicError> {
    let mut out: Vec<TargetedOp<PartyOp, u64>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        if self.host.was_closed(key) {
          self
            .host
            .meters
            .ops_after_close
            .fetch_add(ops.len() as u64, std::sync::atomic::Ordering::Relaxed);
          return Ok(LogicOutput::none());
        }

        for op in ops {
          match op {
            PartyOp::Sit { seat } => {
              if state.players.len() >= SEATS && !state.players.contains_key(&key) {
                continue;
              }
              // A seat whose owner was removed is not available again. The ban
              // memory is the door's, applied to a seat rather than an account.
              if !self.host.may_sit(seat) {
                self.host.close(key, Parting::Kicked, "that seat was taken away");
                continue;
              }
              self.host.seat_taken(key, seat);
              if self.host_key.lock().is_none() {
                *self.host_key.lock() = Some(key);
              }
              state.players.insert(
                key,
                Player {
                  agent: source.clone(),
                  seat,
                  said: 0,
                },
              );
              out.push(TargetedOp::new_system_to(key, vec![PartyOp::Seated { seat }]));
            }
            PartyOp::Say(_) => {
              if let Some(player) = state.players.get_mut(&key) {
                player.said += 1;
              }
            }
            PartyOp::Kick { seat } => {
              if *self.host_key.lock() != Some(key) {
                continue;
              }
              if let Some(target) = self.host.key_of_seat(seat) {
                self.host.close(target, Parting::Kicked, Parting::Kicked.as_str());
              }
            }
            PartyOp::EndParty => {
              if *self.host_key.lock() != Some(key) {
                continue;
              }
              state.ended = true;
            }
            PartyOp::Farewell { .. } | PartyOp::Seated { .. } | PartyOp::Snapshot(_) => {}
          }
        }
      }
      LogicInput::AgentLeft { agent_id } => {
        state.players.remove(&agent_id);
        self.host.parted(agent_id, GRACE);
      }
      LogicInput::AgentJoined { agent } => {
        if let Some(key) = agent.id_cloned() {
          self.host.opened(key);
        }
      }
      LogicInput::TimeStep { .. } => {}
    }

    if state.players.is_empty() {
      return Ok(LogicOutput::ops(out));
    }
    let everyone = state.players.values().map(|p| p.agent.clone()).collect();
    Ok(LogicOutput::ops(out).and_snapshot(SnapshotRequest::uniform(everyone)))
  }
}

impl PartyState {
  pub fn table(&self, host: &Host) -> Table {
    let mut guests: Vec<Guest> = self
      .players
      .iter()
      .map(|(key, p)| Guest {
        seat: p.seat,
        said: p.said,
        quiet_for_ms: host.quiet_for_ms(*key),
        ops_this_window: host.ops_this_window(*key),
        griefer: host.is_griefer(*key),
      })
      .collect();
    guests.sort_by_key(|g| g.seat);
    Table {
      guests,
      held: host.held_seats(),
      ended: self.ended,
    }
  }
}
