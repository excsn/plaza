//! Balances that outlive the table they were won at.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::types::PlayerId;

/// Everyone starts with this, so a first match has a stake to play for.
pub const OPENING_BALANCE: u64 = 100;

#[derive(Debug, Default)]
pub struct WalletRegistry {
  balances: Mutex<HashMap<PlayerId, u64>>,
}

impl WalletRegistry {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn balance(&self, player: PlayerId) -> u64 {
    *self.balances.lock().entry(player).or_insert(OPENING_BALANCE)
  }

  /// Returns the new balance, so a caller reporting the result needs no second lock.
  pub fn credit(&self, player: PlayerId, amount: u64) -> u64 {
    let mut balances = self.balances.lock();
    let entry = balances.entry(player).or_insert(OPENING_BALANCE);
    *entry += amount;
    *entry
  }

  /// Saturating, so a stake larger than a balance empties it rather than
  /// wrapping. Returns what was actually taken.
  pub fn debit(&self, player: PlayerId, amount: u64) -> u64 {
    let mut balances = self.balances.lock();
    let entry = balances.entry(player).or_insert(OPENING_BALANCE);
    let taken = amount.min(*entry);
    *entry -= taken;
    taken
  }

  /// For leaving the lobby. Tables must not call this: surviving a table is the
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
  fn an_unknown_player_opens_with_a_balance_rather_than_nothing() {
    let wallets = WalletRegistry::new();
    assert_eq!(wallets.balance(7), OPENING_BALANCE);
  }

  #[test]
  fn a_debit_larger_than_the_balance_takes_what_is_there() {
    let wallets = WalletRegistry::new();
    assert_eq!(wallets.debit(7, OPENING_BALANCE + 50), OPENING_BALANCE);
    assert_eq!(wallets.balance(7), 0);
  }

  #[test]
  fn a_balance_survives_the_table_it_was_won_at() {
    let wallets = WalletRegistry::new();
    wallets.credit(7, 40);
    assert_eq!(wallets.balance(7), OPENING_BALANCE + 40);
  }

  #[test]
  fn leaving_the_lobby_forgets_the_wallet() {
    let wallets = WalletRegistry::new();
    wallets.credit(7, 40);
    wallets.forget(7);
    assert_eq!(wallets.balance(7), OPENING_BALANCE);
  }
}
