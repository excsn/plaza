//! One-use tickets, so a room learns who connected without asking them.
//!
//! Fills [`JoinRoomOutcomePayload::player_game_token`](crate::op_payloads::JoinRoomOutcomePayload),
//! which otherwise has nothing to put in it. The lobby already knows who it just
//! admitted; without carrying that across, a room's only source for the
//! connecting player's identity is the client, and a client that can name its
//! own id can name somebody else's.
//!
//! ```ignore
//! // In the lobby, after a successful admission:
//! let token = tickets.issue(player, room_id);
//! let endpoint = format!("{}?t={token}", handle.session_endpoint_info());
//!
//! // In the room's connect route:
//! let Some(ticket) = tickets.redeem(&token, &room_id) else { return unauthorized() };
//! session.handle_connection(&req, stream, Agent::new_human(ticket.player))
//! ```
//!
//! # Which store
//!
//! [`TicketStore`] is the seam. Two implementations ship, and they differ in who
//! drives expiry rather than in what they promise:
//!
//! - [`MapTicketRegistry`] is a `HashMap` behind a mutex, with no dependency
//!   beyond what the crate already has. Expiry is swept from `issue`, which is
//!   the operation that grows the map.
//! - [`CachedTicketRegistry`] (feature `cache`) is `fibre_cache`, whose janitor
//!   sweeps on its own and whose shards replace the single mutex. Off by
//!   default, so nothing downstream pays for it unasked.
//!
//! A third belongs to whoever needs a room in another process: verify a signed
//! token and build the [`Ticket`] from its claims, storing nothing. That case
//! is why this is a trait, since it cannot be a mode of either type here.
//!
//! # This is placement, not authentication
//!
//! [`issue`](TicketStore::issue) mints a counter, which is guessable in one try.
//! It is enough to stop a client *naming* another player, which is the failure
//! this closes, and it is not a credential. Anything facing untrusted clients
//! should mint its own signed, expiring value and hand it to
//! [`issue_with`](TicketStore::issue_with); no implementation here cares what
//! the string is.
//!
//! Plaza has no authentication story for this to be consistent with, which is
//! why the crate provides the bookkeeping and not the secret.
//!
//! # Expiry does not stand alone
//!
//! A ticket outliving its [`SeatReservations`](crate::reservations) entry lands
//! a placed player as a spectator with a spent ticket, and the reverse orphans a
//! seat. Redemption is two steps in two places, the route spending the ticket
//! and the room's logic consuming the reservation, so a window here must be
//! **shorter** than the reservation's by at least the time a session takes to
//! come up. Equal windows look right and are not.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use plaza::agent::AgentId;

use crate::types::RoomId;

/// What one ticket entitles its bearer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket<ID: AgentId> {
  pub player: ID,
  pub room: RoomId,
}

/// Outstanding tickets: where the lobby records an admission and the room's
/// route reads it back.
///
/// Object-safe, so a route can hold `Arc<dyn TicketStore<ID>>` and be written
/// once against whichever store a deployment picked.
pub trait TicketStore<ID: AgentId>: Send + Sync {
  /// Mints a ticket with a generated token. See the module docs: the token is a
  /// counter, not a secret.
  fn issue(&self, player: ID, room: RoomId) -> String;

  /// Records a ticket under a token you minted, for anything that needs a real
  /// credential. Replaces any existing entry for that token.
  fn issue_with(&self, token: String, player: ID, room: RoomId);

  /// Spends a ticket for `room`, or `None` if it was never issued, has been
  /// used, has expired, or was issued for somewhere else.
  ///
  /// One use, so a token that leaks cannot be replayed into a second connection
  /// alongside the one that already holds it.
  ///
  /// **The room is checked before the ticket is spent**, not after. Spending
  /// first and comparing afterwards burns a ticket that the room had no claim
  /// on, so under a guessable token anyone could destroy anyone's placement by
  /// presenting it at the wrong door.
  fn redeem(&self, token: &str, room: &RoomId) -> Option<Ticket<ID>>;

  /// Drops a ticket without spending it, for an admission the lobby has since
  /// cancelled.
  fn revoke(&self, token: &str) -> bool;

  /// Tickets handed out, not yet dialled, and not yet expired. A number that
  /// climbs rather than hovering means placements are being issued and
  /// abandoned.
  ///
  /// A diagnostic rather than a hot path: both shipped implementations walk
  /// their contents to answer it.
  fn outstanding(&self) -> usize;
}

struct Held<ID: AgentId> {
  ticket: Ticket<ID>,
  issued_at: Instant,
}

struct Inner<ID: AgentId> {
  held: HashMap<String, Held<ID>>,
  last_swept: Instant,
}

/// A [`TicketStore`] over a `HashMap`, with expiry the caller opts into.
///
/// Internally synchronised rather than `&mut`, because the lobby that issues and
/// the route that redeems are different callers.
///
/// Without [`with_expiry`](MapTicketRegistry::with_expiry) a ticket is held
/// until the process ends, which is unbounded growth under a client that places
/// and never dials.
#[derive(Debug)]
pub struct MapTicketRegistry<ID: AgentId> {
  inner: Mutex<Inner<ID>>,
  next: AtomicU64,
  window: Option<Duration>,
}

impl<ID: AgentId> std::fmt::Debug for Inner<ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Inner").field("held", &self.held.len()).finish()
  }
}

impl<ID: AgentId> std::fmt::Debug for Held<ID> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Held").field("ticket", &self.ticket).finish()
  }
}

impl<ID: AgentId> Default for MapTicketRegistry<ID> {
  fn default() -> Self {
    Self::new()
  }
}

impl<ID: AgentId> MapTicketRegistry<ID> {
  /// A registry that never expires anything.
  pub fn new() -> Self {
    Self::build(None)
  }

  /// A registry that refuses a ticket older than `window` and drops it.
  ///
  /// Sweeping happens on [`issue`](TicketStore::issue), and at most once per
  /// `window`, so the map holds at most one window's worth of dead tickets
  /// however fast placements arrive. Nothing is spawned and nothing ticks.
  pub fn with_expiry(window: Duration) -> Self {
    Self::build(Some(window))
  }

  fn build(window: Option<Duration>) -> Self {
    Self {
      inner: Mutex::new(Inner {
        held: HashMap::new(),
        last_swept: Instant::now(),
      }),
      next: AtomicU64::new(0),
      window,
    }
  }

  /// The window a ticket stays valid for, or `None` if it never expires.
  pub fn expiry(&self) -> Option<Duration> {
    self.window
  }

  fn lapsed(&self, issued_at: Instant, now: Instant) -> bool {
    self.window.is_some_and(|w| now.duration_since(issued_at) >= w)
  }

  fn issue_with_at(&self, token: String, player: ID, room: RoomId, now: Instant) {
    let mut inner = self.inner.lock();
    if let Some(window) = self.window
      && now.duration_since(inner.last_swept) >= window
    {
      inner.held.retain(|_, held| now.duration_since(held.issued_at) < window);
      inner.last_swept = now;
    }
    inner.held.insert(
      token,
      Held {
        ticket: Ticket { player, room },
        issued_at: now,
      },
    );
  }

  fn redeem_at(&self, token: &str, room: &RoomId, now: Instant) -> Option<Ticket<ID>> {
    let mut inner = self.inner.lock();
    if inner.held.get(token).is_none_or(|held| held.ticket.room != *room) {
      return None;
    }
    let held = inner.held.remove(token)?;
    (!self.lapsed(held.issued_at, now)).then_some(held.ticket)
  }

  fn outstanding_at(&self, now: Instant) -> usize {
    self
      .inner
      .lock()
      .held
      .values()
      .filter(|held| !self.lapsed(held.issued_at, now))
      .count()
  }
}

impl<ID: AgentId> TicketStore<ID> for MapTicketRegistry<ID> {
  fn issue(&self, player: ID, room: RoomId) -> String {
    let token = format!("t{}", self.next.fetch_add(1, Ordering::Relaxed));
    self.issue_with(token.clone(), player, room);
    token
  }

  fn issue_with(&self, token: String, player: ID, room: RoomId) {
    self.issue_with_at(token, player, room, Instant::now());
  }

  fn redeem(&self, token: &str, room: &RoomId) -> Option<Ticket<ID>> {
    self.redeem_at(token, room, Instant::now())
  }

  fn revoke(&self, token: &str) -> bool {
    self.inner.lock().held.remove(token).is_some()
  }

  fn outstanding(&self) -> usize {
    self.outstanding_at(Instant::now())
  }
}

#[cfg(feature = "cache")]
mod cached {
  use std::sync::Arc;

  use super::*;
  use fibre_cache::{Cache, CacheBuilder};

  /// A [`TicketStore`] over `fibre_cache`, expiring on a TTL its janitor
  /// enforces.
  ///
  /// No capacity is set, so nothing is ever evicted for pressure and a ticket
  /// leaves only by being spent, revoked, or reaching its TTL. The shards
  /// replace [`MapTicketRegistry`]'s single mutex, which the lobby and every
  /// room route on a host otherwise share.
  pub struct CachedTicketRegistry<ID: AgentId> {
    held: Cache<String, Ticket<ID>>,
    next: AtomicU64,
  }

  impl<ID: AgentId> std::fmt::Debug for CachedTicketRegistry<ID> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("CachedTicketRegistry").finish_non_exhaustive()
    }
  }

  impl<ID: AgentId> CachedTicketRegistry<ID> {
    /// A registry whose tickets expire `window` after they are issued.
    pub fn with_expiry(window: Duration) -> Self {
      Self {
        held: CacheBuilder::default()
          .time_to_live(window)
          .build()
          .expect("an unbounded cache with a ttl and no loader is always buildable"),
        next: AtomicU64::new(0),
      }
    }

    /// Forces the expiry pass the janitor would otherwise run on its own
    /// schedule.
    ///
    /// Deterministic, so a test does not have to sleep past the window and hope.
    pub fn run_maintenance(&self) {
      self.held.run_maintenance();
    }
  }

  impl<ID: AgentId> TicketStore<ID> for CachedTicketRegistry<ID> {
    fn issue(&self, player: ID, room: RoomId) -> String {
      let token = format!("t{}", self.next.fetch_add(1, Ordering::Relaxed));
      self.issue_with(token.clone(), player, room);
      token
    }

    fn issue_with(&self, token: String, player: ID, room: RoomId) {
      self.held.insert(token, Ticket { player, room }, 1);
    }

    fn redeem(&self, token: &str, room: &RoomId) -> Option<Ticket<ID>> {
      // `fetch` filters expired entries and `remove` does not, so an entry the
      // janitor has not reached yet would otherwise still redeem.
      if self.held.fetch(token)?.room != *room {
        return None;
      }
      let spent: Arc<Ticket<ID>> = self.held.remove(token)?;
      Some(Arc::try_unwrap(spent).unwrap_or_else(|shared| (*shared).clone()))
    }

    fn revoke(&self, token: &str) -> bool {
      self.held.invalidate(token)
    }

    fn outstanding(&self) -> usize {
      self.held.iter().count()
    }
  }
}

#[cfg(feature = "cache")]
pub use cached::CachedTicketRegistry;

#[cfg(test)]
mod tests {
  use super::*;
  use uuid::Uuid;

  #[test]
  fn a_ticket_resolves_to_who_it_was_issued_for() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    let ticket = tickets.redeem(&token, &room).expect("issued");
    assert_eq!(ticket.player, 7);
    assert_eq!(ticket.room, room);
  }

  #[test]
  fn a_ticket_spends_once() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    assert!(tickets.redeem(&token, &room).is_some());
    assert!(tickets.redeem(&token, &room).is_none(), "replay is refused");
    assert_eq!(tickets.outstanding(), 0);
  }

  #[test]
  fn an_unissued_token_resolves_to_nobody() {
    let tickets: MapTicketRegistry<u32> = MapTicketRegistry::new();
    assert!(tickets.redeem("t999", &Uuid::new_v4()).is_none());
  }

  #[test]
  fn tickets_are_distinct_per_issue() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    assert_ne!(tickets.issue(1u32, room), tickets.issue(1u32, room));
  }

  #[test]
  fn a_caller_can_supply_its_own_token() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    tickets.issue_with("signed.jwt.value".to_string(), 7u32, room);
    assert_eq!(tickets.redeem("signed.jwt.value", &room).unwrap().player, 7);
  }

  #[test]
  fn a_revoked_ticket_cannot_be_redeemed() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    assert!(tickets.revoke(&token));
    assert!(tickets.redeem(&token, &room).is_none());
  }

  #[test]
  fn outstanding_tracks_what_was_issued_and_not_dialled() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    let a = tickets.issue(1u32, room);
    tickets.issue(2u32, room);
    tickets.redeem(&a, &room);
    assert_eq!(tickets.outstanding(), 1);
  }

  #[test]
  fn a_ticket_presented_at_the_wrong_room_survives_being_refused() {
    // Refusing after spending would let anyone holding a guess destroy a
    // placement they have no claim on, and the token is a counter.
    let tickets = MapTicketRegistry::new();
    let mine = Uuid::new_v4();
    let token = tickets.issue(7u32, mine);

    assert!(tickets.redeem(&token, &Uuid::new_v4()).is_none(), "wrong room is refused");
    assert_eq!(tickets.outstanding(), 1, "and the ticket is still there");
    assert_eq!(tickets.redeem(&token, &mine).unwrap().player, 7);
  }

  #[test]
  fn a_registry_without_a_window_never_expires() {
    let tickets = MapTicketRegistry::new();
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    let ages_later = Instant::now() + Duration::from_secs(86_400);
    assert!(tickets.redeem_at(&token, &room, ages_later).is_some(), "today's behaviour");
  }

  #[test]
  fn a_ticket_past_its_window_is_refused() {
    let tickets = MapTicketRegistry::with_expiry(Duration::from_secs(30));
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    let now = Instant::now();
    assert!(tickets.redeem_at(&token, &room, now + Duration::from_secs(29)).is_some());

    let token = tickets.issue(7u32, room);
    assert!(tickets.redeem_at(&token, &room, now + Duration::from_secs(31)).is_none());
  }

  #[test]
  fn a_refused_ticket_is_dropped_rather_than_left_behind() {
    let tickets = MapTicketRegistry::with_expiry(Duration::from_secs(30));
    let room = Uuid::new_v4();
    let token = tickets.issue(7u32, room);
    let expired = Instant::now() + Duration::from_secs(31);
    assert!(tickets.redeem_at(&token, &room, expired).is_none());
    assert_eq!(tickets.inner.lock().held.len(), 0, "refusing also collects");
  }

  #[test]
  fn issuing_sweeps_what_the_window_has_passed() {
    let tickets = MapTicketRegistry::with_expiry(Duration::from_secs(30));
    let room = Uuid::new_v4();
    let start = Instant::now();
    for player in 0..5u32 {
      tickets.issue_with_at(format!("abandoned{player}"), player, room, start);
    }
    assert_eq!(tickets.inner.lock().held.len(), 5);

    tickets.issue_with_at("fresh".to_string(), 9, room, start + Duration::from_secs(31));
    assert_eq!(
      tickets.inner.lock().held.len(),
      1,
      "the abandoned five go, the one being issued stays"
    );
  }

  #[test]
  fn a_sweep_runs_at_most_once_per_window() {
    let tickets = MapTicketRegistry::with_expiry(Duration::from_secs(30));
    let room = Uuid::new_v4();
    let start = Instant::now();
    tickets.issue_with_at("old".to_string(), 1u32, room, start);

    tickets.issue_with_at("a".to_string(), 2, room, start + Duration::from_secs(10));
    assert_eq!(tickets.inner.lock().held.len(), 2, "too soon to sweep");

    tickets.issue_with_at("b".to_string(), 3, room, start + Duration::from_secs(31));
    assert_eq!(tickets.inner.lock().held.len(), 2, "old swept, a and b remain minus old");
  }

  #[test]
  fn outstanding_does_not_count_the_expired() {
    let tickets = MapTicketRegistry::with_expiry(Duration::from_secs(30));
    let room = Uuid::new_v4();
    let start = Instant::now();
    tickets.issue_with_at("stale".to_string(), 1u32, room, start);
    tickets.issue_with_at("live".to_string(), 2, room, start + Duration::from_secs(25));
    assert_eq!(tickets.outstanding_at(start + Duration::from_secs(31)), 1);
  }

  #[test]
  fn a_store_is_usable_behind_a_trait_object() {
    // The property the cross-host implementation depends on: a route can be
    // written against the seam rather than against a concrete registry.
    let tickets: std::sync::Arc<dyn TicketStore<u32>> = std::sync::Arc::new(MapTicketRegistry::new());
    let room = Uuid::new_v4();
    let token = tickets.issue(7, room);
    assert_eq!(tickets.redeem(&token, &room).unwrap().player, 7);
  }

  #[cfg(feature = "cache")]
  mod cached {
    use super::*;

    #[test]
    fn a_cached_ticket_resolves_to_who_it_was_issued_for() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_secs(30));
      let room = Uuid::new_v4();
      let token = tickets.issue(7u32, room);
      let ticket = tickets.redeem(&token, &room).expect("issued");
      assert_eq!(ticket.player, 7);
      assert_eq!(ticket.room, room);
    }

    #[test]
    fn a_cached_ticket_spends_once() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_secs(30));
      let room = Uuid::new_v4();
      let token = tickets.issue(7u32, room);
      assert!(tickets.redeem(&token, &room).is_some());
      assert!(tickets.redeem(&token, &room).is_none(), "replay is refused");
    }

    #[test]
    fn a_cached_ticket_can_be_revoked() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_secs(30));
      let room = Uuid::new_v4();
      let token = tickets.issue(7u32, room);
      assert!(tickets.revoke(&token));
      assert!(tickets.redeem(&token, &room).is_none());
    }

    #[test]
    fn a_cached_caller_can_supply_its_own_token() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_secs(30));
      let room = Uuid::new_v4();
      tickets.issue_with("signed.jwt.value".to_string(), 7u32, room);
      assert_eq!(tickets.redeem("signed.jwt.value", &room).unwrap().player, 7);
    }

    #[test]
    fn a_cached_ticket_at_the_wrong_room_survives_being_refused() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_secs(30));
      let mine = Uuid::new_v4();
      let token = tickets.issue(7u32, mine);

      assert!(tickets.redeem(&token, &Uuid::new_v4()).is_none(), "wrong room is refused");
      assert_eq!(tickets.outstanding(), 1, "and the ticket is still there");
      assert_eq!(tickets.redeem(&token, &mine).unwrap().player, 7);
    }

    #[test]
    fn a_cached_ticket_past_its_window_is_refused() {
      let tickets = CachedTicketRegistry::with_expiry(Duration::from_millis(20));
      let room = Uuid::new_v4();
      let token = tickets.issue(7u32, room);
      std::thread::sleep(Duration::from_millis(60));
      tickets.run_maintenance();
      assert!(tickets.redeem(&token, &room).is_none());
      assert_eq!(tickets.outstanding(), 0);
    }

    #[test]
    fn a_cached_store_is_usable_behind_a_trait_object() {
      let tickets: std::sync::Arc<dyn TicketStore<u32>> =
        std::sync::Arc::new(CachedTicketRegistry::with_expiry(Duration::from_secs(30)));
      let room = Uuid::new_v4();
      let token = tickets.issue(7, room);
      assert_eq!(tickets.redeem(&token, &room).unwrap().player, 7);
    }
  }
}
