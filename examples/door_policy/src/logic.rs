//! The arcade behind the door.
//!
//! Deliberately small. Every rule the door enforces guards something this
//! holds: a seat is scarce, a wallet is per account, and a credit buys time.
//! The game exists so the door has something to be the door of.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};

use crate::door::Door;
use crate::types::{Account, AgentKey, ArcadeOp, Room, Seat, CREDIT_SECS, STARTING_CREDITS};

/// A close the game asked for and cannot perform.
pub type CloseRequest = (plaza::session::ConnectionId, ArcadeOp, &'static str);

#[derive(Debug, Clone)]
pub struct Player {
  pub agent: Agent<AgentKey>,
  pub account: Account,
  pub score: u32,
  pub seconds_left: u64,
}

/// Wallets outlive connections, which is the point of an account: a session
/// ending must not spend or split what the account holds.
#[derive(Debug, Default)]
pub struct Wallets {
  balances: HashMap<Account, u32>,
}

impl Wallets {
  pub fn balance(&mut self, account: Account) -> u32 {
    *self.balances.entry(account).or_insert(STARTING_CREDITS)
  }

  pub fn spend(&mut self, account: Account) -> bool {
    let held = self.balances.entry(account).or_insert(STARTING_CREDITS);
    if *held == 0 {
      return false;
    }
    *held -= 1;
    true
  }
}

#[derive(Debug, Default)]
pub struct ArcadeState {
  pub players: HashMap<AgentKey, Player>,
  pub wallets: Wallets,
  pub tick: u64,
}

impl ArcadeState {
  pub fn room(&self) -> Room {
    let mut seats: Vec<Seat> = self
      .players
      .values()
      .map(|p| Seat {
        account: p.account,
        score: p.score,
        seconds_left: p.seconds_left,
        credits: 0,
      })
      .collect();
    seats.sort_by_key(|s| s.account);
    Room {
      free_seats: crate::types::SEATS.saturating_sub(seats.len()),
      seats,
    }
  }
}

/// The door rules run *here*, and that is a finding rather than a design.
///
/// `subscribe_to_incoming_messages` has a single consumer and the controller
/// takes it, so an application has no second place to watch inbound ops from.
/// Admission therefore has to be judged inside the game's own rules, which is
/// the wrong home for it: the arcade now knows what a ban is.
#[derive(Debug)]
pub struct ArcadeLogic {
  pub door: Arc<Door>,
  /// Closing a connection is not something logic can do, so it asks.
  pub closes: mpsc::UnboundedSender<CloseRequest>,
}

#[async_trait]
impl StateLogic<ArcadeOp, AgentKey, ArcadeState> for ArcadeLogic {
  async fn process_input(
    &self,
    state: &mut ArcadeState,
    input: LogicInput<ArcadeOp, AgentKey>,
  ) -> Result<LogicOutput<ArcadeOp, AgentKey>, StateLogicError> {
    let mut out: Vec<TargetedOp<ArcadeOp, AgentKey>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        for op in ops {
          match op {
            // Admission is the door's decision, not the game's. The game is
            // told the outcome by `Seat`, never by a client's own claim.
            ArcadeOp::Hello { account } => {
              let Some(conn_id) = self.door.conn_of(key) else { continue };
              let seated = state.players.len();
              match self.door.present_identity(conn_id, account, seated) {
                Ok(evicted) => {
                  for old in evicted {
                    let _ = self.closes.send((
                      old,
                      ArcadeOp::Closed {
                        reason: "signed in from somewhere else".into(),
                      },
                      "duplicate login",
                    ));
                  }
                  let credits = state.wallets.balance(account);
                  state.players.insert(
                    key,
                    Player {
                      agent: source.clone(),
                      account,
                      score: 0,
                      seconds_left: CREDIT_SECS,
                    },
                  );
                  self.door.set_deadline(
                    conn_id,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(CREDIT_SECS),
                  );
                  out.push(TargetedOp::new_system_to(
                    key,
                    vec![ArcadeOp::Admitted {
                      account,
                      seconds: CREDIT_SECS,
                      credits,
                    }],
                  ));
                }
                Err(reason) => {
                  let _ = self.closes.send((conn_id, ArcadeOp::Refused { reason }, reason.as_str()));
                }
              }
            }
            ArcadeOp::Seat { account } => {
              let credits = state.wallets.balance(account);
              state.players.insert(
                key,
                Player {
                  agent: source.clone(),
                  account,
                  score: 0,
                  seconds_left: CREDIT_SECS,
                },
              );
              out.push(TargetedOp::new_system_to(
                key,
                vec![ArcadeOp::Admitted {
                  account,
                  seconds: CREDIT_SECS,
                  credits,
                }],
              ));
            }
            ArcadeOp::Push => {
              if let Some(player) = state.players.get_mut(&key) {
                player.score += 1;
              }
            }
            ArcadeOp::InsertCoin => {
              let Some(player) = state.players.get(&key) else { continue };
              let account = player.account;
              if state.wallets.spend(account) {
                let credits = state.wallets.balance(account);
                if let Some(player) = state.players.get_mut(&key) {
                  player.seconds_left += CREDIT_SECS;
                }
                out.push(TargetedOp::new_system_to(
                  key,
                  vec![ArcadeOp::Admitted {
                    account,
                    seconds: CREDIT_SECS,
                    credits,
                  }],
                ));
              }
            }
            ArcadeOp::Refused { .. } | ArcadeOp::Admitted { .. } | ArcadeOp::Closed { .. } | ArcadeOp::Snapshot(_) => {}
          }
        }
      }
      LogicInput::AgentLeft { agent_id } => {
        state.players.remove(&agent_id);
      }
      // Joining is not being admitted. The connection exists, and whether it
      // may play is decided elsewhere, which is why nothing is seated here.
      LogicInput::AgentJoined { .. } => {}
      LogicInput::TimeStep { .. } => {
        state.tick += 1;
      }
    }

    if state.players.is_empty() {
      return Ok(LogicOutput::ops(out));
    }
    let everyone = state.players.values().map(|p| p.agent.clone()).collect();
    Ok(LogicOutput::ops(out).and_snapshot(SnapshotRequest::uniform(everyone)))
  }
}
