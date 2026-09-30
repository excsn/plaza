//! The arcade behind the door.
//!
//! Deliberately small. Every rule the door enforces guards something this
//! holds: a seat is scarce, a wallet is per account, and a credit buys time.
//! The game exists only to give the door something to guard.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};
use plaza_session::manager::ConnectionManager;
use plaza_session::Farewell;
use tracing::{info, warn};

use crate::door::Door;
use crate::types::{
  Account, AgentKey, ArcadeOp, Room, Seat, CREDIT_SECS, CREDIT_SPENT, REFILL_SECS, STARTING_CREDITS, TICK,
};

/// What one credit buys, in ticks.
const CREDIT_TICKS: u64 = CREDIT_SECS * 1000 / TICK.as_millis() as u64;
/// How often a credit comes back, in ticks.
const REFILL_TICKS: u64 = REFILL_SECS * 1000 / TICK.as_millis() as u64;

#[derive(Debug, Clone)]
pub struct Player {
  pub agent: Agent<AgentKey>,
  pub account: Account,
  pub score: u32,
  /// The tick this player's time runs out on.
  pub paid_until: u64,
}

/// Wallets outlive connections, which is the point of an account: a session
/// ending must not spend or split what the account holds. A spent wallet
/// refills slowly instead, so leaving and knocking again buys nothing.
#[derive(Debug, Default)]
pub struct Wallets {
  balances: HashMap<Account, Wallet>,
}

#[derive(Debug, Clone, Copy)]
struct Wallet {
  credits: u32,
  /// The tick the next refill is counted from. Only read below the start, so a
  /// full wallet banks nothing.
  refill_from: u64,
}

impl Wallet {
  fn at(self, now: u64) -> Self {
    if self.credits >= STARTING_CREDITS {
      return self;
    }
    let earned = (now.saturating_sub(self.refill_from) / REFILL_TICKS) as u32;
    let credits = (self.credits + earned).min(STARTING_CREDITS);
    Self {
      credits,
      refill_from: self.refill_from + u64::from(earned) * REFILL_TICKS,
    }
  }
}

impl Wallets {
  pub fn peek(&self, account: Account, now: u64) -> u32 {
    self.balances.get(&account).map_or(STARTING_CREDITS, |w| w.at(now).credits)
  }

  pub fn spend(&mut self, account: Account, now: u64) -> bool {
    let wallet = self.balances.entry(account).or_insert(Wallet {
      credits: STARTING_CREDITS,
      refill_from: now,
    });
    *wallet = wallet.at(now);
    if wallet.credits == 0 {
      return false;
    }
    if wallet.credits == STARTING_CREDITS {
      wallet.refill_from = now;
    }
    wallet.credits -= 1;
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
  /// What is left of `player`'s time, in the ticks the deadline counts.
  fn ticks_left(&self, player: &Player) -> u64 {
    player.paid_until.saturating_sub(self.tick)
  }

  /// Whole seconds, rounded up so a player with any time left sees at least one.
  pub fn seconds_left(&self, player: &Player) -> u64 {
    (self.ticks_left(player) * TICK.as_millis() as u64).div_ceil(1000)
  }

  pub fn room(&self) -> Room {
    let mut seats: Vec<Seat> = self
      .players
      .values()
      .map(|p| Seat {
        account: p.account,
        score: p.score,
        seconds_left: self.seconds_left(p),
        credits: self.wallets.peek(p.account, self.tick),
      })
      .collect();
    seats.sort_by_key(|s| s.account);
    Room {
      free_seats: crate::types::SEATS.saturating_sub(seats.len()),
      seats,
    }
  }
}

/// The game, which does not know what a ban is: by the time a join reaches
/// it the door has judged the account, so a join is a seat.
#[derive(Debug)]
pub struct ArcadeLogic {
  pub door: Arc<Door>,
  /// The registry, held directly: `set_deadline` is sync, so the logic acts
  /// on its own decisions with no relay task.
  pub manager: Arc<ConnectionManager<AgentKey>>,
}

impl ArcadeLogic {
  /// Sets the session's deadline to the time `ticks` from now.
  fn close_after(&self, key: AgentKey, ticks: u64) {
    for conn_id in self.manager.connections_of(&key) {
      self.manager.set_deadline(
        conn_id,
        Some(TICK * ticks as u32),
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
          warn!(key, ops = ops.len(), "ops from a connection the door already closed");
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
              let Some(player) = state.players.get(&key) else {
                warn!(key, "a coin from a key with no seat");
                continue;
              };
              let account = player.account;
              if !state.wallets.spend(account, state.tick) {
                info!(key, account, "a coin with no credit left");
                out.push(TargetedOp::new_system_to(key, vec![ArcadeOp::NoCredit { account }]));
              } else {
                let credits = state.wallets.peek(account, state.tick);
                let now = state.tick;
                let Some(player) = state.players.get_mut(&key) else { continue };
                player.paid_until = player.paid_until.max(now) + CREDIT_TICKS;
                let player = player.clone();
                self.close_after(key, state.ticks_left(&player));
                out.push(TargetedOp::new_system_to(
                  key,
                  vec![ArcadeOp::Admitted {
                    account,
                    seconds: state.seconds_left(&player),
                    credits,
                  }],
                ));
                info!(key, account, credits, seconds = state.seconds_left(&player), "a coin");
              }
            }
            ArcadeOp::Admitted { .. } | ArcadeOp::NoCredit { .. } | ArcadeOp::Snapshot(_) => {}
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
        // Arrival buys the first stretch, so knocking again is never free time.
        if !state.wallets.spend(account, state.tick) {
          info!(key, account, "arrived with no credit left");
          for conn_id in self.manager.connections_of(&key) {
            self
              .manager
              .close_connection(conn_id, Farewell::new(CREDIT_SPENT).with_detail("no credit left".as_bytes()));
          }
          return Ok(LogicOutput::ops(out));
        }
        let credits = state.wallets.peek(account, state.tick);
        state.players.insert(
          key,
          Player {
            agent: agent.clone(),
            account,
            score: 0,
            paid_until: state.tick + CREDIT_TICKS,
          },
        );
        self.close_after(key, CREDIT_TICKS);
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_spent_wallet_refills_one_credit_per_interval() {
    let mut wallets = Wallets::default();
    for _ in 0..STARTING_CREDITS {
      assert!(wallets.spend(7, 0));
    }
    assert!(!wallets.spend(7, 0));
    assert_eq!(wallets.peek(7, REFILL_TICKS - 1), 0);
    assert_eq!(wallets.peek(7, REFILL_TICKS), 1);
    assert_eq!(wallets.peek(7, 2 * REFILL_TICKS), 2);
    assert_eq!(wallets.peek(7, 99 * REFILL_TICKS), STARTING_CREDITS, "capped at the starting balance");
  }

  #[test]
  fn a_full_wallet_banks_no_refill() {
    let mut wallets = Wallets::default();
    assert!(wallets.spend(7, 10 * REFILL_TICKS));
    assert_eq!(
      wallets.peek(7, 11 * REFILL_TICKS - 1),
      STARTING_CREDITS - 1,
      "the refill clock starts at the spend, not at the account's creation"
    );
    assert_eq!(wallets.peek(7, 11 * REFILL_TICKS), STARTING_CREDITS);
  }
}
