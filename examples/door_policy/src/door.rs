//! The door, and the ledger of what it costs to be one.
//!
//! Everything here is the example's own. Plaza has no admission seam, so this
//! is written as the shape the extraction should take rather than as a
//! workaround: a fallible decision on what the socket shows, a second decision
//! once identity arrives, and an index from account to connection so a decision
//! can be *acted on*.
//!
//! The counts are the point. `at_the_door` is what a fallible factory could
//! have refused for free; `after_admitting` is what today's infallible one
//! forces you to accept, register, announce, snapshot, and then undo.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use parking_lot::Mutex;
use plaza::session::ConnectionId;

use crate::types::{Account, DuplicateLogin, Refusal, PER_IP, SEATS};

/// What being turned away cost, by where the decision could be made.
#[derive(Debug, Default)]
pub struct Ledger {
  pub at_the_door: Mutex<HashMap<Refusal, u64>>,
  pub after_admitting: Mutex<HashMap<Refusal, u64>>,
  /// Work done for connections that were then refused.
  pub registers_wasted: AtomicU64,
  pub presence_events_wasted: AtomicU64,
  pub snapshots_wasted: AtomicU64,
  /// Ops accepted from a connection after it was told to leave. Zero is the
  /// claim; anything else means the close did not close.
  pub ops_after_close: AtomicU64,
  /// Refusals whose reason reached the client before the socket shut.
  pub reasons_delivered: AtomicU64,
  pub silent_closes: AtomicU64,
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

/// One connection's standing with the door.
#[derive(Debug, Clone)]
pub struct Pass {
  pub conn_id: ConnectionId,
  pub addr: SocketAddr,
  pub account: Option<Account>,
  /// Ticks of wall clock this session may still run for.
  pub expires_at: Option<tokio::time::Instant>,
}

/// Who is inside, who is barred, and how many connections each address holds.
///
/// The index from account to connection is the piece plaza cannot supply:
/// `PresenceEvent` carries an agent and no connection, and there is no
/// `connections_of`, so an application on a shipped transport can learn that
/// someone misbehaved and still have no handle to act on.
#[derive(Debug)]
pub struct Door {
  inside: Mutex<HashMap<ConnectionId, Pass>>,
  by_account: Mutex<HashMap<Account, Vec<ConnectionId>>>,
  per_ip: Mutex<HashMap<IpAddr, usize>>,
  banned: Mutex<Vec<Account>>,
  /// Agent key to connection. The index plaza does not keep, needed here
  /// because a decoded op names an agent and ending a session needs a socket.
  keys: Mutex<HashMap<u64, ConnectionId>>,
  pub ledger: Arc<Ledger>,
  pub duplicate_login: Mutex<DuplicateLogin>,
}

impl Door {
  pub fn new(policy: DuplicateLogin) -> Arc<Self> {
    Arc::new(Self {
      inside: Default::default(),
      by_account: Default::default(),
      per_ip: Default::default(),
      banned: Default::default(),
      keys: Default::default(),
      ledger: Default::default(),
      duplicate_login: Mutex::new(policy),
    })
  }

  pub fn bind_key(&self, key: u64, conn_id: ConnectionId) {
    self.keys.lock().insert(key, conn_id);
  }

  pub fn unbind_key(&self, key: u64) {
    self.keys.lock().remove(&key);
  }

  /// What `connections_of` would answer for the agent in a `SessionMessage`.
  pub fn conn_of(&self, key: u64) -> Option<ConnectionId> {
    self.keys.lock().get(&key).copied()
  }

  pub fn ban(&self, account: Account) {
    self.banned.lock().push(account);
  }

  /// The decision a fallible factory could make, on what a socket shows.
  ///
  /// Called *before* `register`, which is the whole difference: nothing has
  /// been allocated, announced or encoded when this says no.
  pub fn knock(&self, addr: SocketAddr) -> Result<(), Refusal> {
    let mut per_ip = self.per_ip.lock();
    let held = per_ip.entry(addr.ip()).or_insert(0);
    if *held >= PER_IP {
      self.ledger.refused(Refusal::PerIpCap);
      return Err(Refusal::PerIpCap);
    }
    *held += 1;
    Ok(())
  }

  /// A connection that got past the socket rule and now exists.
  pub fn opened(&self, conn_id: ConnectionId, addr: SocketAddr) {
    self.inside.lock().insert(
      conn_id,
      Pass {
        conn_id,
        addr,
        account: None,
        expires_at: None,
      },
    );
  }

  /// The decision that needs identity, which arrives only after admission.
  ///
  /// Returns whoever else must be removed for this to be honoured, which under
  /// `KickOldest` is the session already in progress.
  pub fn present_identity(
    &self,
    conn_id: ConnectionId,
    account: Account,
    seated: usize,
  ) -> Result<Vec<ConnectionId>, Refusal> {
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

    existing.push(conn_id);
    drop(by_account);

    if let Some(pass) = self.inside.lock().get_mut(&conn_id) {
      pass.account = Some(account);
    }
    Ok(evict)
  }

  pub fn set_deadline(&self, conn_id: ConnectionId, at: tokio::time::Instant) {
    if let Some(pass) = self.inside.lock().get_mut(&conn_id) {
      pass.expires_at = Some(at);
    }
  }

  /// Connections whose credit has run out.
  pub fn expired(&self, now: tokio::time::Instant) -> Vec<(ConnectionId, Account)> {
    self
      .inside
      .lock()
      .values()
      .filter(|pass| pass.expires_at.is_some_and(|at| at <= now))
      .filter_map(|pass| pass.account.map(|account| (pass.conn_id, account)))
      .collect()
  }

  /// What `connections_of` would answer, if it existed.
  pub fn connections_of(&self, account: Account) -> Vec<ConnectionId> {
    self.by_account.lock().get(&account).cloned().unwrap_or_default()
  }

  pub fn account_of(&self, conn_id: ConnectionId) -> Option<Account> {
    self.inside.lock().get(&conn_id).and_then(|pass| pass.account)
  }

  pub fn is_inside(&self, conn_id: ConnectionId) -> bool {
    self.inside.lock().contains_key(&conn_id)
  }

  pub fn seated(&self) -> usize {
    self
      .inside
      .lock()
      .values()
      .filter(|pass| pass.account.is_some())
      .count()
  }

  /// Forgets a connection, freeing its address slot and its account claim.
  pub fn closed(&self, conn_id: ConnectionId) {
    let pass = self.inside.lock().remove(&conn_id);
    let Some(pass) = pass else { return };

    let mut per_ip = self.per_ip.lock();
    if let Some(held) = per_ip.get_mut(&pass.addr.ip()) {
      *held = held.saturating_sub(1);
    }
    drop(per_ip);

    if let Some(account) = pass.account {
      let mut by_account = self.by_account.lock();
      if let Some(list) = by_account.get_mut(&account) {
        list.retain(|id| *id != conn_id);
        if list.is_empty() {
          by_account.remove(&account);
        }
      }
    }
  }
}
