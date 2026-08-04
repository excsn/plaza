//! The tools a host needs, and the meters that make each claim a count.
//!
//! All of it is the example's own, for the reasons `door_policy` established:
//! there is no server-initiated close, no agent-to-connection index, and no
//! per-connection accounting. What is new here is that moderation needs three
//! numbers the transport already touches and does not keep:
//!
//! - **last activity**, one store per inbound frame, so AFK is a policy rather
//!   than a second heartbeat
//! - **inbound rate per connection**, so a flood is attributable
//! - **whether a parting keeps the seat**, which is the difference between a
//!   kick and a netdrop and cannot be inferred from the socket

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use plaza::session::ConnectionId;

use crate::types::{Parting, Seat, FLOOD_OPS, FLOOD_WINDOW_MS};

#[derive(Debug, Default)]
pub struct Meters {
  /// Farewells whose reason reached the client before the socket shut.
  pub reasons_delivered: AtomicU64,
  /// Closes where it did not. The number that must stay at zero.
  pub silent_closes: AtomicU64,
  pub kicks: AtomicU64,
  pub drops: AtomicU64,
  pub afk_removals: AtomicU64,
  pub flood_removals: AtomicU64,
  pub drained: AtomicU64,
  /// Seats kept warm through a drop.
  pub seats_held: AtomicU64,
  /// Seats cleared because the parting was not a drop.
  pub seats_cleared: AtomicU64,
  /// Rejoins refused because the seat's owner was kicked rather than dropped.
  pub rejoins_refused: AtomicU64,
  /// Ops accepted from a connection already told to go.
  pub ops_after_close: AtomicU64,
  /// Ops the flooder's own connection refused to take.
  pub flooder_shed: AtomicU64,
}

/// One connection's live numbers.
#[derive(Debug)]
pub struct Watch {
  pub conn_id: ConnectionId,
  pub seat: Option<Seat>,
  pub last_activity: tokio::time::Instant,
  pub window_started: tokio::time::Instant,
  pub ops_this_window: u64,
  pub griefer: bool,
}

#[derive(Debug, Default)]
pub struct Host {
  watches: Mutex<HashMap<ConnectionId, Watch>>,
  /// Seats a dropped guest may return to, and when the grace expires.
  held: Mutex<HashMap<Seat, tokio::time::Instant>>,
  /// Seats whose owner was removed rather than dropped. A rejoin here is
  /// refused, which is the ban memory `door_policy` owns, applied to a seat.
  barred: Mutex<Vec<Seat>>,
  keys: Mutex<HashMap<u64, ConnectionId>>,
  pub meters: Arc<Meters>,
}

impl Host {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  pub fn bind_key(&self, key: u64, conn_id: ConnectionId) {
    self.keys.lock().insert(key, conn_id);
  }

  pub fn unbind_key(&self, key: u64) {
    self.keys.lock().remove(&key);
  }

  /// What `connections_of` would answer.
  pub fn conn_of_key(&self, key: u64) -> Option<ConnectionId> {
    self.keys.lock().get(&key).copied()
  }

  pub fn opened(&self, conn_id: ConnectionId, griefer: bool) {
    let now = tokio::time::Instant::now();
    self.watches.lock().insert(
      conn_id,
      Watch {
        conn_id,
        seat: None,
        last_activity: now,
        window_started: now,
        ops_this_window: 0,
        griefer,
      },
    );
  }

  /// One store per inbound frame, which is all AFK needs.
  ///
  /// **Probes must not count.** A link that answers a ping is not a person at
  /// the table, and counting it would make the timeout unreachable.
  pub fn saw_activity(&self, conn_id: ConnectionId, ops: u64) -> bool {
    let now = tokio::time::Instant::now();
    let mut watches = self.watches.lock();
    let Some(watch) = watches.get_mut(&conn_id) else {
      return true;
    };
    watch.last_activity = now;
    if now.duration_since(watch.window_started).as_millis() as u64 >= FLOOD_WINDOW_MS {
      watch.window_started = now;
      watch.ops_this_window = 0;
    }
    watch.ops_this_window += ops;
    watch.ops_this_window <= FLOOD_OPS
  }

  pub fn seat_taken(&self, conn_id: ConnectionId, seat: Seat) {
    if let Some(watch) = self.watches.lock().get_mut(&conn_id) {
      watch.seat = Some(seat);
    }
    self.held.lock().remove(&seat);
  }

  pub fn may_sit(&self, seat: Seat) -> bool {
    if self.barred.lock().contains(&seat) {
      self.meters.rejoins_refused.fetch_add(1, Ordering::Relaxed);
      return false;
    }
    true
  }

  pub fn conn_of_seat(&self, seat: Seat) -> Option<ConnectionId> {
    self
      .watches
      .lock()
      .values()
      .find(|w| w.seat == Some(seat))
      .map(|w| w.conn_id)
  }

  pub fn seat_of(&self, conn_id: ConnectionId) -> Option<Seat> {
    self.watches.lock().get(&conn_id).and_then(|w| w.seat)
  }

  pub fn connections(&self) -> Vec<ConnectionId> {
    self.watches.lock().keys().copied().collect()
  }

  /// Connections silent for longer than the timeout.
  pub fn afk(&self, timeout: std::time::Duration) -> Vec<ConnectionId> {
    let now = tokio::time::Instant::now();
    self
      .watches
      .lock()
      .values()
      .filter(|w| w.seat.is_some() && now.duration_since(w.last_activity) >= timeout)
      .map(|w| w.conn_id)
      .collect()
  }

  pub fn quiet_for(&self, conn_id: ConnectionId) -> u64 {
    self
      .watches
      .lock()
      .get(&conn_id)
      .map(|w| tokio::time::Instant::now().duration_since(w.last_activity).as_millis() as u64)
      .unwrap_or(0)
  }

  pub fn ops_this_window(&self, conn_id: ConnectionId) -> u64 {
    self.watches.lock().get(&conn_id).map(|w| w.ops_this_window).unwrap_or(0)
  }

  pub fn is_griefer(&self, conn_id: ConnectionId) -> bool {
    self.watches.lock().get(&conn_id).map(|w| w.griefer).unwrap_or(false)
  }

  /// Records how a connection ended, and decides what happens to its seat.
  pub fn parted(&self, conn_id: ConnectionId, how: Parting, grace: std::time::Duration) {
    let watch = self.watches.lock().remove(&conn_id);
    let Some(watch) = watch else { return };

    match how {
      Parting::Dropped => self.meters.drops.fetch_add(1, Ordering::Relaxed),
      Parting::Kicked => self.meters.kicks.fetch_add(1, Ordering::Relaxed),
      Parting::Afk => self.meters.afk_removals.fetch_add(1, Ordering::Relaxed),
      Parting::Flooding => self.meters.flood_removals.fetch_add(1, Ordering::Relaxed),
      Parting::Drained => self.meters.drained.fetch_add(1, Ordering::Relaxed),
    };

    let Some(seat) = watch.seat else { return };
    if how.keeps_the_seat() {
      // The seat stays warm. This is what `ReconnectTracker` is for, and the
      // reason a kick has to be told apart from a drop at all.
      self.held.lock().insert(seat, tokio::time::Instant::now() + grace);
      self.meters.seats_held.fetch_add(1, Ordering::Relaxed);
    } else {
      self.held.lock().remove(&seat);
      self.barred.lock().push(seat);
      self.meters.seats_cleared.fetch_add(1, Ordering::Relaxed);
    }
  }

  pub fn held_seats(&self) -> Vec<(Seat, u64)> {
    let now = tokio::time::Instant::now();
    self
      .held
      .lock()
      .iter()
      .map(|(seat, until)| (*seat, until.saturating_duration_since(now).as_millis() as u64))
      .collect()
  }

  pub fn is_held(&self, seat: Seat) -> bool {
    self.held.lock().contains_key(&seat)
  }
}
