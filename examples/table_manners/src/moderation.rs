//! The host's tools, over the library's readers.
//!
//! The transport this example used to carry is gone. Last activity comes from
//! `agent_idle_for`, attribution from `agent_inbound`, the close from
//! `deregister_agent`, and the drain from `disconnect_all`. What remains is
//! policy: the timeout numbers, which seat survives which parting, and the
//! book of seats held and barred.
//!
//! The parting reason lives *here*, not in a transport: the host initiated
//! every non-drop parting, so it already knows why, and a departure with no
//! pending reason is what a netdrop is. The transport never interprets a
//! disconnect, and keeping the division costs nothing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use plaza_session::manager::ConnectionManager;

use crate::types::{op_frame, Parting, PartyOp, Seat, FLOOD_OPS, FLOOD_WINDOW_MS};

/// How long a dropped guest's seat stays warm.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct Meters {
  /// Farewells handed to a close, riding ahead of it. Whether one reached the
  /// client is asserted from the client's side; the server cannot watch its
  /// own farewell land.
  pub reasons_sent: AtomicU64,
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
  /// Ops accepted from a guest already told to go.
  pub ops_after_close: AtomicU64,
}

/// One guest's book-keeping: the seat, and the flood window's baseline.
#[derive(Debug)]
pub struct Watch {
  pub seat: Option<Seat>,
  pub griefer: bool,
  window_started: tokio::time::Instant,
  frames_at_window_start: u64,
}

#[derive(Debug)]
pub struct Host {
  manager: Arc<ConnectionManager<u64>>,
  watches: Mutex<HashMap<u64, Watch>>,
  /// Partings this host ordered, awaiting the `Left` that confirms them. A
  /// departure with no entry here is a netdrop.
  pending: Mutex<HashMap<u64, Parting>>,
  /// Seats a dropped guest may return to, and when the grace expires.
  held: Mutex<plaza::common::reconnect::ReconnectTracker<Seat, std::time::Duration>>,
  /// The zero the tracker's time axis is measured from.
  epoch: tokio::time::Instant,
  /// Seats whose owner was removed rather than dropped. A rejoin here is
  /// refused: the ban memory `door_policy` keeps per account, applied to a
  /// seat.
  barred: Mutex<Vec<Seat>>,
  closed: Mutex<HashSet<u64>>,
  pub meters: Meters,
}

impl Host {
  pub fn new(manager: Arc<ConnectionManager<u64>>) -> Arc<Self> {
    Arc::new(Self {
      manager,
      watches: Default::default(),
      pending: Default::default(),
      held: Mutex::new(plaza::common::reconnect::ReconnectTracker::new(GRACE)),
      epoch: tokio::time::Instant::now(),
      barred: Default::default(),
      closed: Default::default(),
      meters: Default::default(),
    })
  }

  pub fn manager(&self) -> &Arc<ConnectionManager<u64>> {
    &self.manager
  }

  pub fn opened(&self, key: u64) {
    let volume = self.manager.agent_inbound(&key);
    self.watches.lock().insert(
      key,
      Watch {
        seat: None,
        griefer: false,
        window_started: tokio::time::Instant::now(),
        frames_at_window_start: volume.frames,
      },
    );
  }

  pub fn seat_taken(&self, key: u64, seat: Seat) {
    if let Some(watch) = self.watches.lock().get_mut(&key) {
      watch.seat = Some(seat);
    }
    self.held.lock().on_reconnect(&seat);
  }

  pub fn may_sit(&self, seat: Seat) -> bool {
    if self.barred.lock().contains(&seat) {
      self.meters.rejoins_refused.fetch_add(1, Ordering::Relaxed);
      return false;
    }
    true
  }

  pub fn key_of_seat(&self, seat: Seat) -> Option<u64> {
    self
      .watches
      .lock()
      .iter()
      .find(|(_, w)| w.seat == Some(seat))
      .map(|(key, _)| *key)
  }

  pub fn seat_of(&self, key: u64) -> Option<Seat> {
    self.watches.lock().get(&key).and_then(|w| w.seat)
  }

  pub fn seated_keys(&self) -> Vec<u64> {
    self
      .watches
      .lock()
      .iter()
      .filter(|(_, w)| w.seat.is_some())
      .map(|(key, _)| *key)
      .collect()
  }

  pub fn was_closed(&self, key: u64) -> bool {
    self.closed.lock().contains(&key)
  }

  /// Ends a guest's session with the reason ahead of the close, and remembers
  /// why so the `Left` that follows is not mistaken for a netdrop.
  pub fn close(&self, key: u64, reason: Parting, detail: impl Into<String>) {
    if !self.closed.lock().insert(key) {
      return;
    }
    self.pending.lock().insert(key, reason);
    let farewell = op_frame(PartyOp::Farewell {
      reason,
      detail: detail.into(),
    });
    if self.manager.deregister_agent(&key, Some(farewell)) > 0 {
      self.meters.reasons_sent.fetch_add(1, Ordering::Relaxed);
    }
  }

  /// Ends the party: everyone told, then closed, through the same path.
  pub fn drain(&self, reason: Parting) {
    for key in self.watches.lock().keys() {
      self.closed.lock().insert(*key);
      self.pending.lock().insert(*key, reason);
    }
    let told = self.manager.disconnect_all(Some(op_frame(PartyOp::Farewell {
      reason,
      detail: reason.as_str().into(),
    })));
    self.meters.reasons_sent.fetch_add(told as u64, Ordering::Relaxed);
  }

  /// Applies a departure, deciding what happens to the seat.
  ///
  /// The reason is whatever this host recorded when it ordered the close;
  /// nothing else knows one, so no entry means the network went away.
  pub fn parted(&self, key: u64) {
    let how = self.pending.lock().remove(&key).unwrap_or(Parting::Dropped);
    match how {
      Parting::Dropped => self.meters.drops.fetch_add(1, Ordering::Relaxed),
      Parting::Kicked => self.meters.kicks.fetch_add(1, Ordering::Relaxed),
      Parting::Afk => self.meters.afk_removals.fetch_add(1, Ordering::Relaxed),
      Parting::Flooding => self.meters.flood_removals.fetch_add(1, Ordering::Relaxed),
      Parting::Drained => self.meters.drained.fetch_add(1, Ordering::Relaxed),
    };

    let watch = self.watches.lock().remove(&key);
    let Some(seat) = watch.and_then(|w| w.seat) else { return };
    if how.keeps_the_seat() {
      // The seat stays warm: the reason a kick has to be told apart from a
      // drop at all.
      self.held.lock().on_disconnect(seat, self.epoch.elapsed());
      self.meters.seats_held.fetch_add(1, Ordering::Relaxed);
    } else {
      self.held.lock().forget(&seat);
      self.barred.lock().push(seat);
      self.meters.seats_cleared.fetch_add(1, Ordering::Relaxed);
    }
  }

  /// Whether a guest's inbound rate has crossed the flood line, advancing its
  /// window. The counters are the manager's; the window and the number are
  /// this host's.
  pub fn over_rate(&self, key: u64) -> bool {
    let volume = self.manager.agent_inbound(&key);
    let now = tokio::time::Instant::now();
    let mut watches = self.watches.lock();
    let Some(watch) = watches.get_mut(&key) else {
      return false;
    };
    if now.duration_since(watch.window_started).as_millis() as u64 >= FLOOD_WINDOW_MS {
      watch.window_started = now;
      watch.frames_at_window_start = volume.frames;
      return false;
    }
    volume.frames.saturating_sub(watch.frames_at_window_start) > FLOOD_OPS
  }

  pub fn quiet_for_ms(&self, key: u64) -> u64 {
    self
      .manager
      .agent_idle_for(&key)
      .map(|idle| idle.as_millis() as u64)
      .unwrap_or(0)
  }

  pub fn ops_this_window(&self, key: u64) -> u64 {
    let volume = self.manager.agent_inbound(&key);
    self
      .watches
      .lock()
      .get(&key)
      .map(|w| volume.frames.saturating_sub(w.frames_at_window_start))
      .unwrap_or(0)
  }

  pub fn is_griefer(&self, key: u64) -> bool {
    self.watches.lock().get(&key).map(|w| w.griefer).unwrap_or(false)
  }

  pub fn held_seats(&self) -> Vec<(Seat, u64)> {
    let now = self.epoch.elapsed();
    self
      .held
      .lock()
      .awaiting()
      .map(|(seat, until)| (*seat, until.saturating_sub(now).as_millis() as u64))
      .collect()
  }

  pub fn is_held(&self, seat: Seat) -> bool {
    self.held.lock().is_awaiting_reconnect(&seat)
  }
}

/// Applies the timeouts the host set, from the manager's own readers.
///
/// The one timer in the example, and it is the application's: the session
/// keeps the readings, the host owns the numbers and the sweep.
pub async fn steward(host: Arc<Host>, afk: std::time::Duration) {
  let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
  loop {
    ticker.tick().await;
    for key in host.seated_keys() {
      if host.was_closed(key) {
        continue;
      }
      if host.manager().agent_idle_for(&key).is_some_and(|idle| idle >= afk) {
        host.close(key, Parting::Afk, Parting::Afk.as_str());
      } else if host.over_rate(key) {
        host.close(key, Parting::Flooding, Parting::Flooding.as_str());
      }
    }
  }
}
