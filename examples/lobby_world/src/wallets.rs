//! Balances that outlive the room they were earned in.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::types::PlayerId;

#[derive(Debug, Default)]
pub struct WalletRegistry {
  balances: Mutex<HashMap<PlayerId, u64>>,
}

impl WalletRegistry {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn balance(&self, player: PlayerId) -> u64 {
    self.balances.lock().get(&player).copied().unwrap_or(0)
  }

  /// Returns the new balance, so a caller reporting the result needs no second lock.
  pub fn credit(&self, player: PlayerId, amount: u64) -> u64 {
    let mut balances = self.balances.lock();
    let entry = balances.entry(player).or_insert(0);
    *entry += amount;
    *entry
  }

  /// For leaving the lobby. Arenas must not call this: surviving a room is the
  /// point of the registry.
  pub fn forget(&self, player: PlayerId) {
    self.balances.lock().remove(&player);
  }

  pub fn tracked(&self) -> usize {
    self.balances.lock().len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_unknown_player_carries_nothing() {
    let wallets = WalletRegistry::new();
    assert_eq!(wallets.balance(7), 0);
  }

  #[test]
  fn credits_accumulate_across_calls() {
    let wallets = WalletRegistry::new();
    assert_eq!(wallets.credit(7, 10), 10);
    assert_eq!(wallets.credit(7, 5), 15);
    assert_eq!(wallets.balance(7), 15);
  }

  #[test]
  fn balances_are_per_player() {
    let wallets = WalletRegistry::new();
    wallets.credit(1, 10);
    wallets.credit(2, 3);
    assert_eq!(wallets.balance(1), 10);
    assert_eq!(wallets.balance(2), 3);
  }

  #[test]
  fn a_balance_outlives_the_room_it_was_earned_in() {
    let wallets = WalletRegistry::new();
    wallets.credit(7, 40);
    assert_eq!(wallets.balance(7), 40);
    wallets.credit(7, 2);
    assert_eq!(wallets.balance(7), 42);
  }

  #[test]
  fn leaving_the_world_clears_the_wallet() {
    let wallets = WalletRegistry::new();
    wallets.credit(7, 40);
    wallets.forget(7);
    assert_eq!(wallets.balance(7), 0);
    assert_eq!(wallets.tracked(), 0);
  }
}
