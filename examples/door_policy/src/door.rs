//! The door's policy, and the ledger that prices enforcing it.
//!
//! Everything mechanical now comes from the library: the factory refuses
//! ([`plaza_session::tcp::Refusal`]), `connections_of` resolves an account's
//! agent to a socket, `close_connection` ends a session with the reason ahead
//! of the close, and `set_deadline` sweeps the credit. What is left here is
//! only what plaza must never own: which rules exist, what they refuse for,
//! and who loses a duplicate login.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::types::{Account, AgentKey, DuplicateLogin, Refusal, PER_IP, SEATS};

/// What being turned away cost, by where the decision could be made.
#[derive(Debug, Default)]
pub struct Ledger {
  pub at_the_door: Mutex<HashMap<Refusal, u64>>,
  pub after_admitting: Mutex<HashMap<Refusal, u64>>,
  /// Connections registered: every one of these was announced and snapshotted
  /// before any identity rule could be judged.
  pub registered: AtomicU64,
  /// Reasons handed to a close, riding ahead of it. Whether one reached the
  /// client is asserted from the client's side; the server cannot watch its
  /// own farewell land.
  pub reasons_sent: AtomicU64,
  /// Ops accepted from a connection after it was told to leave. Zero is the
  /// claim; anything else means the close did not close.
  pub ops_after_close: AtomicU64,
}

impl Ledger {
  pub fn refused(&self, reason: Refusal) {
    let table = if reason.decidable_at_the_door() {
      &self.at_the_door
    } else {
      &self.after_admitting
    };
    *table.lock().entry(reason).or_insert(0) += 1;
  }

  pub fn total(&self) -> u64 {
    let a: u64 = self.at_the_door.lock().values().sum();
    let b: u64 = self.after_admitting.lock().values().sum();
    a + b
  }
}

/// Who is inside, who is barred, and how many connections each address holds.
///
/// No connection ids anywhere: an agent key resolves to its socket through
/// `ConnectionManager::connections_of` whenever a rule needs to act, so the
/// only indexes left are the ones that carry *policy* facts the library has no
/// business holding: address occupancy, account claims, and the ban list.
#[derive(Debug)]
pub struct Door {
  per_ip: Mutex<HashMap<std::net::IpAddr, usize>>,
  /// Which address each admitted key arrived from, so a departure frees the
  /// right slot. Written by the factory, released on `AgentLeft`.
  addrs: Mutex<HashMap<AgentKey, std::net::IpAddr>>,
  by_account: Mutex<HashMap<Account, Vec<AgentKey>>>,
  accounts: Mutex<HashMap<AgentKey, Account>>,
  banned: Mutex<Vec<Account>>,
  /// Keys this door has ordered closed; an op arriving for one afterwards is
  /// the number the panel must keep at zero.
  closed: Mutex<HashSet<AgentKey>>,
  next_key: AtomicU64,
  pub ledger: Ledger,
  pub duplicate_login: Mutex<DuplicateLogin>,
}

impl Door {
  pub fn new(policy: DuplicateLogin) -> Arc<Self> {
    Arc::new(Self {
      per_ip: Default::default(),
      addrs: Default::default(),
      by_account: Default::default(),
      accounts: Default::default(),
      banned: Default::default(),
      closed: Default::default(),
      next_key: AtomicU64::new(1),
      ledger: Default::default(),
      duplicate_login: Mutex::new(policy),
    })
  }

  pub fn ban(&self, account: Account) {
    self.banned.lock().push(account);
  }

  /// The decision the fallible factory makes, on what a socket shows.
  ///
  /// Nothing has been allocated, announced or encoded when this says no; on
  /// yes it mints the agent key the connection will be addressed by.
  pub fn knock(&self, addr: SocketAddr) -> Result<AgentKey, Refusal> {
    let mut per_ip = self.per_ip.lock();
    let held = per_ip.entry(addr.ip()).or_insert(0);
    if *held >= PER_IP {
      self.ledger.refused(Refusal::PerIpCap);
      return Err(Refusal::PerIpCap);
    }
    *held += 1;
    drop(per_ip);

    let key = self.next_key.fetch_add(1, Ordering::Relaxed);
    self.addrs.lock().insert(key, addr.ip());
    Ok(key)
  }

  /// The decision that needs identity, which arrives only after admission.
  ///
  /// Returns whoever else must be removed for this to be honoured, which under
  /// `KickOldest` is the session already in progress.
  pub fn present_identity(&self, key: AgentKey, account: Account, seated: usize) -> Result<Vec<AgentKey>, Refusal> {
    if self.banned.lock().contains(&account) {
      self.ledger.refused(Refusal::Banned);
      return Err(Refusal::Banned);
    }

    let mut by_account = self.by_account.lock();
    let existing = by_account.entry(account).or_default();
    let mut evict = Vec::new();
    if !existing.is_empty() {
      match *self.duplicate_login.lock() {
        DuplicateLogin::RefuseNewest => {
          self.ledger.refused(Refusal::AlreadyInside);
          return Err(Refusal::AlreadyInside);
        }
        DuplicateLogin::KickOldest => {
          evict.append(existing);
        }
      }
    } else if seated >= SEATS {
      self.ledger.refused(Refusal::OverCapacity);
      return Err(Refusal::OverCapacity);
    }

    existing.push(key);
    drop(by_account);
    self.accounts.lock().insert(key, account);
    Ok(evict)
  }

  /// Marks a close this door ordered, so a later op from the same key can be
  /// recognised as arriving after the goodbye.
  pub fn closing(&self, key: AgentKey) {
    self.closed.lock().insert(key);
    self.ledger.reasons_sent.fetch_add(1, Ordering::Relaxed);
  }

  pub fn was_closed(&self, key: AgentKey) -> bool {
    self.closed.lock().contains(&key)
  }

  /// Forgets a departed key, freeing its address slot and its account claim.
  pub fn left(&self, key: AgentKey) {
    if let Some(ip) = self.addrs.lock().remove(&key) {
      if let Some(held) = self.per_ip.lock().get_mut(&ip) {
        *held = held.saturating_sub(1);
      }
    }
    if let Some(account) = self.accounts.lock().remove(&key) {
      let mut by_account = self.by_account.lock();
      if let Some(list) = by_account.get_mut(&account) {
        list.retain(|held| *held != key);
        if list.is_empty() {
          by_account.remove(&account);
        }
      }
    }
  }

  /// Accounts currently seated, by the door's own book.
  pub fn seated(&self) -> usize {
    self.by_account.lock().len()
  }
}
