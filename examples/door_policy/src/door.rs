//! The door's policy, the admitter that applies it and the ledger that prices
//! it.
//!
//! Everything mechanical comes from the library: a socket waits unregistered
//! until [`Doorman::admit`] answers, `deregister_agent` ends the loser of a
//! duplicate login with the reason in its goodbye and `set_deadline` sweeps
//! the credit. What is left here is only what plaza must never own: which
//! rules exist, what they refuse for and who loses a duplicate login.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::agent::Agent;
use plaza::common::closure::ClosureLog;
use plaza_session::codec::JsonCodec;
use plaza_session::manager::ConnectionManager;
use plaza_session::{ConnectionAdmission, ConnectionAdmitter, Farewell, Peer, WireCodec};

use crate::types::{Account, AgentKey, DuplicateLogin, Refusal, PER_IP, SEATS, SIGNED_IN_ELSEWHERE};

/// What saying no cost.
#[derive(Debug, Default)]
pub struct Ledger {
  pub refusals: Mutex<HashMap<Refusal, u64>>,
  /// Connections registered, which is now the same number as connections
  /// admitted: nothing registers before the door has judged it.
  pub registered: AtomicU64,
  /// Goodbyes this door ordered for a connection already inside. Whether one
  /// reached the client is asserted from the client's side; the server cannot
  /// watch its own goodbye land.
  pub reasons_sent: AtomicU64,
  /// Ops accepted from a connection after it was told to leave. This should
  /// stay at zero; anything else means the close did not close.
  pub ops_after_close: AtomicU64,
}

impl Ledger {
  pub fn refused(&self, reason: Refusal) {
    *self.refusals.lock().entry(reason).or_insert(0) += 1;
  }

  pub fn total(&self) -> u64 {
    self.refusals.lock().values().sum()
  }
}

/// Who is inside, who is barred, and how many connections each address holds.
///
/// No connection ids anywhere: an agent key resolves to its socket through
/// `ConnectionManager` whenever a rule needs to act, so the only indexes left
/// are the ones that carry *policy* facts the library has no business
/// holding: address occupancy, account claims and the ban list.
#[derive(Debug)]
pub struct Door {
  per_ip: Mutex<HashMap<IpAddr, usize>>,
  /// Which address each admitted key arrived from, so a departure frees the
  /// right slot.
  addrs: Mutex<HashMap<AgentKey, IpAddr>>,
  by_account: Mutex<HashMap<Account, Vec<AgentKey>>>,
  accounts: Mutex<HashMap<AgentKey, Account>>,
  banned: Mutex<Vec<Account>>,
  /// Closes this door ordered; an op arriving for one afterwards is the
  /// number the panel must keep at zero.
  closed: Mutex<ClosureLog<AgentKey, ()>>,
  next_key: AtomicU64,
  per_ip_cap: usize,
  pub ledger: Ledger,
  pub duplicate_login: Mutex<DuplicateLogin>,
}

impl Door {
  pub fn new(policy: DuplicateLogin) -> Arc<Self> {
    Self::with_per_ip(policy, PER_IP)
  }

  /// A door with its own address cap, for showing that rule on one machine
  /// where every client shares an address.
  pub fn with_per_ip(policy: DuplicateLogin, per_ip_cap: usize) -> Arc<Self> {
    Arc::new(Self {
      per_ip: Default::default(),
      addrs: Default::default(),
      by_account: Default::default(),
      accounts: Default::default(),
      banned: Default::default(),
      closed: Default::default(),
      next_key: AtomicU64::new(1),
      per_ip_cap,
      ledger: Default::default(),
      duplicate_login: Mutex::new(policy),
    })
  }

  pub fn ban(&self, account: Account) {
    self.banned.lock().push(account);
  }

  /// Every rule, judged once, before anything is registered.
  ///
  /// The address rule goes first because it needs no identity; the rest need
  /// the account. On yes it mints the key the connection will be addressed by
  /// and names whoever must be removed for the admission to hold, which under
  /// `KickOldest` is the session already in progress.
  pub fn admit(&self, addr: Option<IpAddr>, account: Account) -> Result<(AgentKey, Vec<AgentKey>), Refusal> {
    if let Some(ip) = addr {
      let mut per_ip = self.per_ip.lock();
      let held = per_ip.entry(ip).or_insert(0);
      if *held >= self.per_ip_cap {
        self.ledger.refused(Refusal::PerIpCap);
        return Err(Refusal::PerIpCap);
      }
      *held += 1;
    }

    match self.present_identity(account) {
      Ok((key, evicted)) => {
        if let Some(ip) = addr {
          self.addrs.lock().insert(key, ip);
        }
        Ok((key, evicted))
      }
      Err(reason) => {
        if let Some(ip) = addr
          && let Some(held) = self.per_ip.lock().get_mut(&ip) {
            *held = held.saturating_sub(1);
          }
        self.ledger.refused(reason);
        Err(reason)
      }
    }
  }

  fn present_identity(&self, account: Account) -> Result<(AgentKey, Vec<AgentKey>), Refusal> {
    if self.banned.lock().contains(&account) {
      return Err(Refusal::Banned);
    }

    let mut by_account = self.by_account.lock();
    let mut evict = Vec::new();
    let seated = by_account.len();
    let existing = by_account.entry(account).or_default();
    if !existing.is_empty() {
      match *self.duplicate_login.lock() {
        DuplicateLogin::RefuseNewest => return Err(Refusal::AlreadyInside),
        DuplicateLogin::KickOldest => evict.append(existing),
      }
    } else if seated >= SEATS {
      by_account.remove(&account);
      return Err(Refusal::OverCapacity);
    }

    let key = self.next_key.fetch_add(1, Ordering::Relaxed);
    by_account.entry(account).or_default().push(key);
    drop(by_account);
    self.accounts.lock().insert(key, account);
    Ok((key, evict))
  }

  pub fn account_of(&self, key: AgentKey) -> Option<Account> {
    self.accounts.lock().get(&key).copied()
  }

  /// Marks a close this door ordered, so a later op from the same key can be
  /// recognised as arriving after the goodbye.
  pub fn closing(&self, key: AgentKey) {
    if self.closed.lock().order(key, ()) {
      self.ledger.reasons_sent.fetch_add(1, Ordering::Relaxed);
    }
  }

  pub fn was_closed(&self, key: AgentKey) -> bool {
    self.closed.lock().was_ordered(&key)
  }

  /// Forgets a departed key, freeing its address slot and its account claim.
  pub fn left(&self, key: AgentKey) {
    self.closed.lock().departed(&key);
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

/// The door as the transport meets it: the layer between the socket and the
/// game where governance runs.
///
/// Holds the manager so that under `KickOldest` it can end the session in
/// progress itself, which works because the loser is registered and the
/// newcomer is not yet.
#[derive(Debug)]
pub struct Doorman {
  pub door: Arc<Door>,
  manager: OnceLock<Arc<ConnectionManager<AgentKey>>>,
}

impl Doorman {
  pub fn new(door: Arc<Door>) -> Self {
    Self {
      door,
      manager: OnceLock::new(),
    }
  }

  /// The session's registry, once the session exists. The admitter is built
  /// before the session it admits into, so this comes second.
  pub fn attach(&self, manager: Arc<ConnectionManager<AgentKey>>) {
    let _ = self.manager.set(manager);
  }
}

#[async_trait]
impl ConnectionAdmitter<AgentKey> for Doorman {
  async fn admit(&self, credential: &[u8], peer: &Peer) -> ConnectionAdmission<AgentKey> {
    let Ok(account) = JsonCodec.decode::<Account>(credential) else {
      self.door.ledger.refused(Refusal::Unreadable);
      return refuse(Refusal::Unreadable);
    };
    match self.door.admit(peer.addr.map(|a| a.ip()), account) {
      Ok((key, evicted)) => {
        for old in evicted {
          if let Some(manager) = self.manager.get() {
            manager.deregister_agent(
              &old,
              Farewell::new(SIGNED_IN_ELSEWHERE).with_detail("signed in from somewhere else".as_bytes()),
            );
          }
          self.door.closing(old);
        }
        ConnectionAdmission::Admitted(Agent::new_human(key))
      }
      Err(reason) => refuse(reason),
    }
  }
}

fn refuse(reason: Refusal) -> ConnectionAdmission<AgentKey> {
  ConnectionAdmission::Refused(Farewell::new(reason.code()).with_detail(reason.as_str().as_bytes()))
}
