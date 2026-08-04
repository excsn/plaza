//! The party. Small on purpose: moderation is the subject.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};
use tokio::sync::mpsc;

use crate::moderation::Host;
use crate::types::{Guest, Parting, PartyOp, Seat, Table, SEATS};

/// A close the game asked for and cannot perform, for the reasons
/// `door_policy` recorded.
pub type CloseRequest = (plaza::session::ConnectionId, Parting, String);

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
  pub closes: mpsc::UnboundedSender<CloseRequest>,
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
        let Some(conn_id) = self.doorman_conn(key) else {
          return Ok(LogicOutput::none());
        };

        for op in ops {
          match op {
            PartyOp::Sit { seat } => {
              if state.players.len() >= SEATS && !state.players.contains_key(&key) {
                continue;
              }
              // A seat whose owner was removed is not available again. The ban
              // memory is the door's, applied to a seat rather than an account.
              if !self.host.may_sit(seat) {
                let _ = self.closes.send((
                  conn_id,
                  Parting::Kicked,
                  "that seat was taken away".into(),
                ));
                continue;
              }
              self.host.seat_taken(conn_id, seat);
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
              if let Some(target) = self.host.conn_of_seat(seat) {
                let _ = self
                  .closes
                  .send((target, Parting::Kicked, Parting::Kicked.as_str().into()));
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
      }
      LogicInput::AgentJoined { .. } => {}
      LogicInput::TimeStep { .. } => {}
    }

    if state.players.is_empty() {
      return Ok(LogicOutput::ops(out));
    }
    let everyone = state.players.values().map(|p| p.agent.clone()).collect();
    Ok(LogicOutput::ops(out).and_snapshot(SnapshotRequest::uniform(everyone)))
  }
}

impl PartyLogic {
  /// The agent-to-connection lookup again, kept by the host for want of one in
  /// the library.
  fn doorman_conn(&self, key: u64) -> Option<plaza::session::ConnectionId> {
    self.host.conn_of_key(key)
  }
}

impl PartyState {
  pub fn table(&self, host: &Host) -> Table {
    let mut guests: Vec<Guest> = self
      .players
      .values()
      .map(|p| {
        let conn = host.conn_of_seat(p.seat);
        Guest {
          seat: p.seat,
          said: p.said,
          quiet_for_ms: conn.map(|c| host.quiet_for(c)).unwrap_or(0),
          ops_this_window: conn.map(|c| host.ops_this_window(c)).unwrap_or(0),
          griefer: conn.map(|c| host.is_griefer(c)).unwrap_or(false),
        }
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
