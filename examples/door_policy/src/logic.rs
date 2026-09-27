//! The arcade behind the door.
//!
//! Deliberately small. Every rule the door enforces guards something this
//! holds: a seat is scarce, a wallet is per account, and a credit buys time.
//! The game exists only to give the door something to guard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};
use plaza_session::manager::ConnectionManager;
use plaza_session::Farewell;

use crate::door::Door;
use crate::types::{Account, AgentKey, ArcadeOp, Room, Seat, CREDIT_SECS, CREDIT_SPENT, STARTING_CREDITS};

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

/// The game, which no longer knows what a ban is: by the time a join reaches
/// it the door has judged the account, so a join is a seat.
#[derive(Debug)]
pub struct ArcadeLogic {
  pub door: Arc<Door>,
  /// The registry, held directly: `set_deadline` is sync, so the logic acts
  /// on its own decisions with no relay task.
  pub manager: Arc<ConnectionManager<AgentKey>>,
}

impl ArcadeLogic {
  fn buy_time(&self, key: AgentKey) {
    for conn_id in self.manager.connections_of(&key) {
      self.manager.set_deadline(
        conn_id,
        Some(Duration::from_secs(CREDIT_SECS)),
        Farewell::new(CREDIT_SPENT).with_detail("your credit ran out".as_bytes()),
      );
    }
  }
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
        if self.door.was_closed(key) {
          self
            .door
            .ledger
            .ops_after_close
            .fetch_add(ops.len() as u64, std::sync::atomic::Ordering::Relaxed);
          return Ok(LogicOutput::none());
        }
        for op in ops {
          match op {
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
                self.buy_time(key);
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
            ArcadeOp::Admitted { .. } | ArcadeOp::Snapshot(_) => {}
          }
        }
      }
      LogicInput::AgentLeft { agent_id } => {
        state.players.remove(&agent_id);
        self.door.left(agent_id);
      }
      // A join is an admission: the door judged the account before the
      // connection registered, so the seat is taken here without a question.
      LogicInput::AgentJoined { agent } => {
        self
          .door
          .ledger
          .registered
          .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(key) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let Some(account) = self.door.account_of(key) else {
          return Ok(LogicOutput::none());
        };
        let credits = state.wallets.balance(account);
        state.players.insert(
          key,
          Player {
            agent: agent.clone(),
            account,
            score: 0,
            seconds_left: CREDIT_SECS,
          },
        );
        self.buy_time(key);
        out.push(TargetedOp::new_system_to(
          key,
          vec![ArcadeOp::Admitted {
            account,
            seconds: CREDIT_SECS,
            credits,
          }],
        ));
      }
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
