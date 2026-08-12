//! Handing out a fixed number of seats to whoever connects.
//!
//! A world with a bounded number of participants (four players, sixteen holes,
//! two paddles) has to answer three questions as people come and go: who is
//! driving which seat, what happens to a seat nobody holds, and what happens to
//! the state that seat accumulated while somebody else was in it.
//!
//! The first two are bookkeeping. The third is where the bugs are, and it is why
//! [`seat`](SeatTable::seat) returns a [`Seating`] rather than a bare index.
//!
//! # The reason `Seating` is an enum
//!
//! Anything kept per seat outlives the person in it: a delta baseline, an input
//! history, a score, a cooldown. A server that keeps advancing those for
//! unoccupied seats (which is usually the simplest thing, and often the right
//! thing, because bots drive the empties) hands a joiner a seat with somebody
//! else's history attached.
//!
//! That failure is close to invisible. In the case this was drawn from, a
//! relevance baseline had been advancing on an empty seat since startup, so a
//! joiner's first frame was computed as a *delta against a world it had never
//! received*: almost nothing was sent, and the arena arrived only as the slow
//! trickle of whatever later became newly relevant. It looked like packet loss.
//! It was a seat that remembered.
//!
//! Returning `Seating::Fresh` rather than `Some(index)` makes the caller decide
//! what to reset, at the one moment it is knowable, instead of remembering to.

use std::collections::HashMap;
use std::hash::Hash;

pub mod queue;
pub mod slots;

pub use queue::RankedQueue;
pub use slots::SeatSlots;

/// What happened when a key asked for a seat.
///
/// Deliberately not `Option<usize>`: the difference between a new occupant and a
/// key that already held a seat is exactly the difference between "reset this
/// seat's history" and "do not", and collapsing the two is the bug this type
/// exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seating {
  /// A newly occupied seat. Everything kept per seat belongs to whoever sat here
  /// before, and is now this occupant's problem unless it is reset.
  Fresh(usize),
  /// This key already held this seat. A duplicate join, or a rejoin that was
  /// never unseated. Nothing to reset; resetting anyway would throw away state
  /// the occupant is mid-way through using.
  Existing(usize),
  /// Every seat is taken. A real outcome rather than an assertion: a world with
  /// a fixed number of seats is a world people can overfill.
  Full,
}

impl Seating {
  /// The seat index, whether it was fresh or already held.
  ///
  /// Use it where the distinction genuinely does not matter, such as replying to
  /// the joiner. Prefer matching where it does.
  pub fn index(self) -> Option<usize> {
    match self {
      Seating::Fresh(index) | Seating::Existing(index) => Some(index),
      Seating::Full => None,
    }
  }

  /// Whether this occupancy is new, and so whether per-seat state is stale.
  pub fn is_fresh(self) -> bool {
    matches!(self, Seating::Fresh(_))
  }
}

/// Which connection is driving which seat.
///
/// Seat indices are stable and dense (`0..capacity`), so per-seat state can live
/// in plain vectors indexed by seat, which is what the simulation wants anyway.
#[derive(Clone, Debug, Default)]
pub struct SeatTable<Key: Eq + Hash + Clone> {
  occupied: HashMap<Key, usize>,
  free: Vec<usize>,
  capacity: usize,
}

impl<Key: Eq + Hash + Clone> SeatTable<Key> {
  /// A table with `capacity` seats, all empty.
  pub fn new(capacity: usize) -> Self {
    Self {
      occupied: HashMap::new(),
      // Popped from the back, so the first joiner takes the highest seat. Which
      // end does not matter as long as it is consistent, and a test that names a
      // specific seat depends on it.
      free: (0..capacity).collect(),
      capacity,
    }
  }

  /// Seats a key, or reports that the world is full.
  pub fn seat(&mut self, key: Key) -> Seating {
    if let Some(index) = self.occupied.get(&key) {
      return Seating::Existing(*index);
    }
    match self.free.pop() {
      Some(index) => {
        self.occupied.insert(key, index);
        Seating::Fresh(index)
      }
      None => Seating::Full,
    }
  }

  /// Empties a key's seat, returning which one it was so the caller can hand it
  /// back to a bot, clear its pending input, or whatever an empty seat means
  /// here.
  ///
  /// Idempotent: unseating a key that holds no seat is not an error, because a
  /// disconnect can be reported more than once.
  pub fn unseat(&mut self, key: &Key) -> Option<usize> {
    let index = self.occupied.remove(key)?;
    self.free.push(index);
    Some(index)
  }

  pub fn seat_of(&self, key: &Key) -> Option<usize> {
    self.occupied.get(key).copied()
  }

  /// Every occupied seat, as `(key, seat)`. Unordered.
  pub fn occupants(&self) -> impl Iterator<Item = (&Key, usize)> + '_ {
    self.occupied.iter().map(|(key, seat)| (key, *seat))
  }

  /// The keys holding a seat. Unordered.
  pub fn keys(&self) -> impl Iterator<Item = &Key> + '_ {
    self.occupied.keys()
  }

  /// Seat index back to the key holding it, for sending something built per seat
  /// to the right connection.
  pub fn by_seat(&self) -> HashMap<usize, Key> {
    self.occupied.iter().map(|(key, seat)| (*seat, key.clone())).collect()
  }

  pub fn occupied_count(&self) -> usize {
    self.occupied.len()
  }

  pub fn capacity(&self) -> usize {
    self.capacity
  }

  pub fn is_full(&self) -> bool {
    self.free.is_empty()
  }

  /// Empties every seat, keeping the capacity.
  pub fn clear(&mut self) {
    self.occupied.clear();
    self.free = (0..self.capacity).collect();
  }

  /// Rebuilds the table at a new capacity and reseats whoever was connected,
  /// returning the new `(key, seat)` pairs.
  ///
  /// For a world that is rebuilt under the players: a changed entity count, a
  /// changed layout, a reset. Everyone who fits is given a seat in the new world
  /// and the caller re-welcomes them, so nobody is left playing against a world
  /// that no longer exists. Anyone who does not fit is dropped from the table
  /// and is not in the returned list.
  ///
  /// Every returned seat is fresh by definition, since the world behind it is
  /// new.
  pub fn reseat_all(&mut self, capacity: usize) -> Vec<(Key, usize)> {
    let previous: Vec<Key> = self.occupied.keys().cloned().collect();
    self.capacity = capacity;
    self.occupied.clear();
    self.free = (0..capacity).collect();

    let mut reseated = Vec::new();
    for key in previous {
      if let Some(index) = self.free.pop() {
        self.occupied.insert(key.clone(), index);
        reseated.push((key, index));
      }
    }
    reseated
  }
}

/// One seat's occupancy, as [`Roster`] reports it.
///
/// `Open` says nothing about who drives the seat: a game with a bot bench reads
/// every open seat as bot-driven, a lobby reads it as empty. That
/// interpretation is the application's, which is why there is no `Bot` variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatState<'a, Key> {
  Human(&'a Key),
  /// The seat is kept for a departed key. Whose clock decides for how long is
  /// the application's: pair with `ReconnectTracker` or your own deadline, and
  /// call [`Roster::expire`] when it runs out.
  Held(&'a Key),
  Open,
}

/// What happened when a key asked in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
  /// In a seat. `fresh` carries the same warning as [`Seating::Fresh`]: a fresh
  /// seat's per-seat state belongs to whoever sat there before.
  Seated { seat: usize, fresh: bool },
  /// The key's held seat is theirs again, everything in it intact. Resend them
  /// the state they missed; reset nothing.
  Resumed { seat: usize },
  /// Queued for the next open seat, at this position (0 is next).
  Waitlisted { position: usize },
  /// Not seated and not queued. Whether that means spectating or a refusal is
  /// the application's answer to the same event.
  Turned(Turnaway),
}

/// Why a key was not seated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turnaway {
  Full,
  /// Seating is closed regardless of free seats; see [`Roster::lock`].
  Locked,
}

/// What happened when a key left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Departure {
  Freed { seat: usize },
  /// The seat is kept for them; start their clock and call
  /// [`Roster::expire`] if it runs out.
  Held { seat: usize },
  /// They were in the queue, not in a seat.
  Unwaitlisted,
  NotPresent,
}

/// What [`resolve`](Roster::resolve) did: who reached a seat, and who was
/// displaced to make room for a better-ranked waiter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shuffle<Key> {
  /// Promoted from the waitlist. The seat is fresh.
  Promoted { key: Key, seat: usize },
  /// Unseated in favour of a better-ranked waiter, and requeued at the tail of
  /// their own rank band.
  Displaced { key: Key, seat: usize },
}

/// Seating with the policies games actually vary: a lock for games that seat
/// only between rounds, a ranked waitlist for the next open seat, and seats
/// held across an absence.
///
/// [`SeatTable`] is the plain case, seat-on-arrival and free-on-leave, and
/// stays the right choice for it. `Roster` is for everything the examples were
/// hand-rolling around it: a rink whose empty seats are bots until a human
/// takes one, two paddles that bots hold only until a person wants one, a run
/// that keeps a leaver's seat while their grace lasts. It is composed of two
/// smaller public blocks, [`SeatSlots`] and [`RankedQueue`]: a seating policy
/// this type does not express is built from those directly, the same
/// unmake-the-prescription contract as `SimHost` over `Host`.
///
/// Three rules run through it:
///
/// - **Promotion happens on the tick.** [`admit`](Self::admit) and
///   [`depart`](Self::depart) settle the arriving or leaving key immediately,
///   but a freed seat reaches the waitlist only in
///   [`resolve`](Self::resolve), called from your `TimeStep` arm. Seating
///   decisions made in two places, join handling and departure handling, is
///   the bug the pong example spent a comment warning about.
/// - **Ranks displace only across bands.** A waiter with a better (lower)
///   rank takes the worst-ranked human seat at `resolve`; equals never
///   displace each other, and a held seat is never displaced, because the
///   hold is a promise.
/// - **No clocks.** A held seat stays held until you call
///   [`expire`](Self::expire); how long that takes is between you and your
///   `ReconnectTracker`.
#[derive(Clone, Debug, Default)]
pub struct Roster<Key: Eq + Hash + Clone> {
  slots: SeatSlots<Key>,
  queue: RankedQueue<Key>,
  ranks: HashMap<Key, u32>,
  waitlist_on: bool,
  hold_on: bool,
  locked: bool,
}

impl<Key: Eq + Hash + Clone> Roster<Key> {
  /// `capacity` seats, all open, seating live, no waitlist, no holds.
  pub fn new(capacity: usize) -> Self {
    Self {
      slots: SeatSlots::new(capacity),
      queue: RankedQueue::new(),
      ranks: HashMap::new(),
      waitlist_on: false,
      hold_on: false,
      locked: false,
    }
  }

  /// Turned-away keys queue for the next open seat instead.
  pub fn with_waitlist(mut self) -> Self {
    self.waitlist_on = true;
    self
  }

  /// A departing key's seat is held for them until [`expire`](Self::expire).
  pub fn holding_seats(mut self) -> Self {
    self.hold_on = true;
    self
  }

  /// Closes seating: every `admit` waits or is turned away, whatever the free
  /// count, and the waitlist stops draining. For games that seat only between
  /// rounds; [`unlock`](Self::unlock) at the boundary and the next
  /// [`resolve`](Self::resolve) seats the queue.
  pub fn lock(&mut self) {
    self.locked = true;
  }

  pub fn unlock(&mut self) {
    self.locked = false;
  }

  pub fn is_locked(&self) -> bool {
    self.locked
  }

  /// [`admit_ranked`](Self::admit_ranked) at the best rank. A roster whose
  /// admissions all use this never displaces anyone.
  pub fn admit(&mut self, key: Key) -> Admission {
    self.admit_ranked(key, 0)
  }

  /// Seats, resumes, queues or turns away `key`, in that order of preference.
  ///
  /// `rank` orders the waitlist and the displacement rule; lower is better.
  /// The classic use is people at 0 and bots at 1, so a bot holds a seat only
  /// until a person wants one. A key already seated keeps the rank it was
  /// seated with.
  pub fn admit_ranked(&mut self, key: Key, rank: u32) -> Admission {
    if let Some(seat) = self.slots.seat_of(&key) {
      if self.slots.is_held(&key) {
        self.slots.resume(&key);
        return Admission::Resumed { seat };
      }
      return Admission::Seated { seat, fresh: false };
    }
    if let Some(position) = self.queue.position(&key) {
      return Admission::Waitlisted { position };
    }
    if !self.locked
      && let Some(seat) = self.slots.first_open() {
        self.slots.seat(key.clone(), seat);
        self.ranks.insert(key, rank);
        return Admission::Seated { seat, fresh: true };
      }
    if self.waitlist_on {
      let position = self.queue.push(key, rank);
      return Admission::Waitlisted { position };
    }
    Admission::Turned(if self.locked { Turnaway::Locked } else { Turnaway::Full })
  }

  /// Settles a leaving key: frees or holds their seat, or drops them from the
  /// queue. A freed seat reaches the waitlist at the next
  /// [`resolve`](Self::resolve), not here.
  ///
  /// Idempotent, because a disconnect can be reported more than once: a second
  /// report of a held key reports the hold again rather than breaking it.
  pub fn depart(&mut self, key: &Key) -> Departure {
    if let Some(seat) = self.slots.seat_of(key) {
      if self.slots.is_held(key) {
        return Departure::Held { seat };
      }
      if self.hold_on {
        self.slots.hold(key);
        return Departure::Held { seat };
      }
      self.slots.open(key);
      self.ranks.remove(key);
      return Departure::Freed { seat };
    }
    if self.queue.remove(key) {
      return Departure::Unwaitlisted;
    }
    Departure::NotPresent
  }

  /// Releases a held seat whose grace ran out, returning which one it was.
  /// The seat reaches the waitlist at the next [`resolve`](Self::resolve).
  pub fn expire(&mut self, key: &Key) -> Option<usize> {
    if !self.slots.is_held(key) {
      return None;
    }
    self.ranks.remove(key);
    self.slots.open(key)
  }

  /// Seats the waitlist into open seats in queue order, then settles rank
  /// displacement. Call from your `TimeStep` arm; a no-op while locked. Every
  /// promoted seat is fresh.
  pub fn resolve(&mut self) -> Vec<Shuffle<Key>> {
    let mut shuffles = Vec::new();
    if self.locked {
      return shuffles;
    }
    while let Some(seat) = self.slots.first_open() {
      let Some((key, rank)) = self.queue.pop_best() else {
        break;
      };
      self.slots.seat(key.clone(), seat);
      self.ranks.insert(key.clone(), rank);
      shuffles.push(Shuffle::Promoted { key, seat });
    }
    loop {
      let Some((_, best_rank)) = self.queue.best() else {
        break;
      };
      let Some((seat, worst_rank)) = self.worst_seated() else {
        break;
      };
      if best_rank >= worst_rank {
        break;
      }
      let SeatState::Human(displaced) = self.slots.state(seat) else {
        break;
      };
      let displaced = displaced.clone();
      self.slots.open(&displaced);
      self.ranks.remove(&displaced);
      self.queue.push(displaced.clone(), worst_rank);
      shuffles.push(Shuffle::Displaced { key: displaced, seat });
      let (key, rank) = self.queue.pop_best().expect("best was just observed");
      self.slots.seat(key.clone(), seat);
      self.ranks.insert(key.clone(), rank);
      shuffles.push(Shuffle::Promoted { key, seat });
    }
    shuffles
  }

  /// The worst-ranked human seat, later seats winning ties. Held seats are
  /// not candidates: the hold is a promise.
  fn worst_seated(&self) -> Option<(usize, u32)> {
    let mut worst: Option<(usize, u32)> = None;
    for seat in 0..self.slots.capacity() {
      if let SeatState::Human(key) = self.slots.state(seat) {
        let rank = self.ranks.get(key).copied().unwrap_or(0);
        if worst.is_none_or(|(_, worst_rank)| rank >= worst_rank) {
          worst = Some((seat, rank));
        }
      }
    }
    worst
  }

  pub fn seat_of(&self, key: &Key) -> Option<usize> {
    self.slots.seat_of(key)
  }

  /// The seat's occupancy. Panics if `seat` is out of range, the same contract
  /// as indexing the per-seat state it sits beside.
  pub fn seat_state(&self, seat: usize) -> SeatState<'_, Key> {
    self.slots.state(seat)
  }

  /// Every seat in index order.
  pub fn seats(&self) -> impl Iterator<Item = SeatState<'_, Key>> + '_ {
    (0..self.slots.capacity()).map(|seat| self.slots.state(seat))
  }

  /// The queue, next out first.
  pub fn waiting(&self) -> impl Iterator<Item = &Key> + '_ {
    self.queue.iter()
  }

  pub fn capacity(&self) -> usize {
    self.slots.capacity()
  }

  /// Seats that are not open, held ones included: a held seat is not free.
  pub fn occupied_count(&self) -> usize {
    self.slots.occupied_count()
  }

  pub fn is_full(&self) -> bool {
    self.occupied_count() == self.capacity()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The two accessors nothing was calling.
  mod reading_the_table {
    use super::*;

    #[test]
    fn occupants_pairs_every_key_with_the_seat_it_holds() {
      // The iteration a server does every tick to build per-client output, and
      // the one place a seat and its holder must not drift apart.
      let mut table: SeatTable<u32> = SeatTable::new(4);
      table.seat(10);
      table.seat(20);

      let mut pairs: Vec<(u32, usize)> = table.occupants().map(|(key, seat)| (*key, seat)).collect();
      pairs.sort_unstable();
      assert_eq!(pairs.len(), 2);
      for (key, seat) in pairs {
        assert_eq!(table.seat_of(&key), Some(seat), "the pair agrees with the lookup");
      }
    }

    #[test]
    fn occupants_forgets_a_key_that_left() {
      let mut table: SeatTable<u32> = SeatTable::new(4);
      table.seat(10);
      table.seat(20);
      table.unseat(&10);
      let held: Vec<u32> = table.occupants().map(|(key, _)| *key).collect();
      assert_eq!(held, vec![20]);
    }

    #[test]
    fn a_locked_roster_says_so() {
      // Worth pinning because the flag is what a between-rounds game reads to
      // decide whether a joiner waits, and a lock that does not report itself
      // is one nobody can show a player.
      let mut roster: Roster<u32> = Roster::new(4);
      assert!(!roster.is_locked());
      roster.lock();
      assert!(roster.is_locked());
      roster.unlock();
      assert!(!roster.is_locked());
    }
  }

  #[test]
  fn a_joiner_gets_a_seat_and_a_rejoin_gets_the_same_one() {
    let mut seats: SeatTable<u64> = SeatTable::new(4);
    let first = seats.seat(1);
    assert!(first.is_fresh());
    let again = seats.seat(1);
    assert_eq!(again, Seating::Existing(first.index().unwrap()));
    assert_eq!(seats.occupied_count(), 1, "a duplicate join is not a second occupant");
  }

  #[test]
  fn a_new_occupant_is_reported_as_fresh_so_stale_seat_state_is_reset() {
    // The warm-arena join bug in one test. Seat state (here, a per-seat counter
    // standing in for a delta baseline) keeps advancing while the seat is empty,
    // because bots drive the empties. A joiner must be told the seat is fresh, or
    // it inherits a history it never lived through.
    let mut seats: SeatTable<u64> = SeatTable::new(2);
    let mut baseline = [0u32; 2];

    let seat = seats.seat(1).index().unwrap();
    for _ in 0..150 {
      // The world runs whether or not anybody is in the seat.
      baseline.iter_mut().for_each(|b| *b += 1);
    }
    seats.unseat(&1);
    for _ in 0..150 {
      baseline.iter_mut().for_each(|b| *b += 1);
    }

    match seats.seat(2) {
      Seating::Fresh(index) => {
        assert_eq!(index, seat, "the freed seat is handed out again");
        baseline[index] = 0;
      }
      other => panic!("a new key in a freed seat must be fresh, got {other:?}"),
    }
    assert_eq!(baseline[seat], 0, "the new occupant inherited the old one's history");
  }

  #[test]
  fn a_full_world_refuses_rather_than_panicking() {
    // A demo people can share is a demo people can overfill, so this is an
    // outcome to report, not an invariant to assert.
    let mut seats: SeatTable<u64> = SeatTable::new(2);
    assert!(seats.seat(1).is_fresh());
    assert!(seats.seat(2).is_fresh());
    assert_eq!(seats.seat(3), Seating::Full);
    assert!(seats.is_full());
    assert_eq!(seats.seat(3).index(), None);
  }

  #[test]
  fn an_unseated_seat_is_handed_out_again() {
    let mut seats: SeatTable<u64> = SeatTable::new(1);
    let taken = seats.seat(1).index().unwrap();
    assert_eq!(seats.seat(2), Seating::Full);
    assert_eq!(seats.unseat(&1), Some(taken));
    assert_eq!(seats.seat(2), Seating::Fresh(taken));
  }

  #[test]
  fn unseating_a_stranger_is_not_an_error() {
    // A disconnect can be reported more than once, and the second report must not
    // free a seat somebody else is now sitting in.
    let mut seats: SeatTable<u64> = SeatTable::new(2);
    seats.seat(1);
    assert!(seats.unseat(&1).is_some());
    assert_eq!(seats.unseat(&1), None);
    assert_eq!(seats.unseat(&99), None);
    assert_eq!(seats.occupied_count(), 0);
  }

  #[test]
  fn rebuilding_the_world_reseats_everyone_who_still_fits() {
    let mut seats: SeatTable<u64> = SeatTable::new(4);
    for key in 1..=3 {
      seats.seat(key);
    }
    let reseated = seats.reseat_all(8);
    assert_eq!(reseated.len(), 3, "everybody who was connected is in the new world");
    assert_eq!(seats.capacity(), 8);
    for (key, seat) in &reseated {
      assert_eq!(seats.seat_of(key), Some(*seat));
    }

    // Shrinking past the crowd drops whoever does not fit, rather than handing
    // out a seat index the world has no room for.
    let squeezed = seats.reseat_all(2);
    assert_eq!(squeezed.len(), 2);
    assert!(squeezed.iter().all(|(_, seat)| *seat < 2));
    assert_eq!(seats.occupied_count(), 2);
  }

  #[test]
  fn every_seat_index_stays_inside_the_capacity() {
    // Per-seat state lives in vectors indexed by these, so an index past the end
    // is a panic in the simulation rather than a bad seat.
    let mut seats: SeatTable<u64> = SeatTable::new(3);
    for key in 1..=3 {
      let index = seats.seat(key).index().unwrap();
      assert!(index < 3, "{index} is outside a capacity of 3");
    }
    assert_eq!(seats.by_seat().len(), 3, "each seat is held by exactly one key");
  }
}

#[cfg(test)]
mod roster_tests {
  use super::*;

  #[test]
  fn plain_seating_matches_the_table() {
    let mut roster: Roster<u64> = Roster::new(2);
    assert_eq!(roster.admit(1), Admission::Seated { seat: 0, fresh: true });
    assert_eq!(roster.admit(1), Admission::Seated { seat: 0, fresh: false });
    assert_eq!(roster.admit(2), Admission::Seated { seat: 1, fresh: true });
    assert_eq!(roster.admit(3), Admission::Turned(Turnaway::Full));
    assert_eq!(roster.depart(&1), Departure::Freed { seat: 0 });
    assert_eq!(roster.depart(&1), Departure::NotPresent);
    assert_eq!(roster.admit(3), Admission::Seated { seat: 0, fresh: true });
  }

  #[test]
  fn a_lock_turns_away_with_seats_free_and_the_unlock_seats_the_queue() {
    let mut roster: Roster<u64> = Roster::new(3).with_waitlist();
    roster.admit(1);
    roster.lock();
    assert_eq!(roster.admit(2), Admission::Waitlisted { position: 0 });
    assert_eq!(roster.admit(3), Admission::Waitlisted { position: 1 });
    assert!(roster.resolve().is_empty(), "a lock also stops the queue draining");

    roster.unlock();
    assert_eq!(
      roster.resolve(),
      vec![Shuffle::Promoted { key: 2, seat: 1 }, Shuffle::Promoted { key: 3, seat: 2 }],
      "queue order, at the boundary"
    );
  }

  #[test]
  fn without_a_waitlist_a_lock_is_a_turnaway() {
    let mut roster: Roster<u64> = Roster::new(3);
    roster.lock();
    assert_eq!(roster.admit(1), Admission::Turned(Turnaway::Locked));
  }

  #[test]
  fn a_freed_seat_reaches_the_waitlist_at_the_tick_not_at_the_departure() {
    let mut roster: Roster<u64> = Roster::new(1).with_waitlist();
    roster.admit(1);
    assert_eq!(roster.admit(2), Admission::Waitlisted { position: 0 });
    roster.depart(&1);
    assert_eq!(roster.seat_state(0), SeatState::Open, "not promoted mid-departure");
    assert_eq!(roster.resolve(), vec![Shuffle::Promoted { key: 2, seat: 0 }]);
    assert_eq!(roster.seat_of(&2), Some(0));
  }

  #[test]
  fn asking_again_while_queued_reports_the_position_rather_than_queueing_twice() {
    let mut roster: Roster<u64> = Roster::new(1).with_waitlist();
    roster.admit(1);
    roster.admit(2);
    assert_eq!(roster.admit(2), Admission::Waitlisted { position: 0 });
    assert_eq!(roster.waiting().count(), 1);
  }

  #[test]
  fn a_waiter_who_leaves_is_out_of_the_queue() {
    let mut roster: Roster<u64> = Roster::new(1).with_waitlist();
    roster.admit(1);
    roster.admit(2);
    roster.admit(3);
    assert_eq!(roster.depart(&2), Departure::Unwaitlisted);
    roster.depart(&1);
    assert_eq!(
      roster.resolve(),
      vec![Shuffle::Promoted { key: 3, seat: 0 }],
      "the leaver's place is not held in the queue"
    );
  }

  #[test]
  fn a_held_seat_resumes_intact_and_expires_open() {
    let mut roster: Roster<u64> = Roster::new(2).holding_seats();
    roster.admit(1);
    assert_eq!(roster.depart(&1), Departure::Held { seat: 0 });
    assert_eq!(roster.seat_state(0), SeatState::Held(&1));
    assert!(roster.is_full() || roster.occupied_count() == 1, "a held seat is not free");
    assert_eq!(roster.admit(2), Admission::Seated { seat: 1, fresh: true }, "holds do not block other seats");

    assert_eq!(roster.admit(1), Admission::Resumed { seat: 0 });
    assert_eq!(roster.depart(&1), Departure::Held { seat: 0 });
    assert_eq!(roster.expire(&1), Some(0));
    assert_eq!(roster.expire(&1), None, "an expiry lands once");
    assert_eq!(roster.seat_state(0), SeatState::Open);
  }

  #[test]
  fn an_expired_hold_feeds_the_waitlist_on_the_tick() {
    let mut roster: Roster<u64> = Roster::new(1).holding_seats().with_waitlist();
    roster.admit(1);
    roster.depart(&1);
    assert_eq!(roster.admit(2), Admission::Waitlisted { position: 0 }, "a held seat is not open");
    roster.expire(&1);
    assert_eq!(roster.resolve(), vec![Shuffle::Promoted { key: 2, seat: 0 }]);
  }

  #[test]
  fn a_better_ranked_waiter_displaces_the_worst_seated() {
    // Two bots hold the paddles; two people arrive. Both bots stand aside, on
    // the tick, later seats first.
    let mut roster: Roster<u64> = Roster::new(2).with_waitlist();
    roster.admit_ranked(10, 1);
    roster.admit_ranked(11, 1);
    assert_eq!(roster.admit_ranked(1, 0), Admission::Waitlisted { position: 0 });
    assert_eq!(roster.admit_ranked(2, 0), Admission::Waitlisted { position: 1 });

    assert_eq!(
      roster.resolve(),
      vec![
        Shuffle::Displaced { key: 11, seat: 1 },
        Shuffle::Promoted { key: 1, seat: 1 },
        Shuffle::Displaced { key: 10, seat: 0 },
        Shuffle::Promoted { key: 2, seat: 0 },
      ]
    );
    let displaced: Vec<u64> = roster.waiting().copied().collect();
    assert_eq!(displaced, vec![11, 10], "the bots wait in their band");
  }

  #[test]
  fn equal_ranks_never_displace_each_other() {
    let mut roster: Roster<u64> = Roster::new(1).with_waitlist();
    roster.admit(1);
    roster.admit(2);
    assert!(roster.resolve().is_empty(), "waiting your turn is not being outranked");
  }

  #[test]
  fn a_held_seat_is_never_displaced() {
    // The hold is a promise, and a better-ranked arrival does not break it.
    let mut roster: Roster<u64> = Roster::new(1).holding_seats().with_waitlist();
    roster.admit_ranked(10, 1);
    roster.depart(&10);
    roster.admit_ranked(1, 0);
    assert!(roster.resolve().is_empty());
    assert_eq!(roster.seat_state(0), SeatState::Held(&10));
  }

  #[test]
  fn a_second_disconnect_report_keeps_the_hold() {
    let mut roster: Roster<u64> = Roster::new(1).holding_seats();
    roster.admit(1);
    assert_eq!(roster.depart(&1), Departure::Held { seat: 0 });
    assert_eq!(roster.depart(&1), Departure::Held { seat: 0 }, "a repeat report must not break the hold");
    assert_eq!(roster.seat_state(0), SeatState::Held(&1));
  }

  #[test]
  fn open_seats_read_as_whatever_the_game_says_they_are() {
    // The bot bench: every open seat is bot-driven, and the block does not know.
    let mut roster: Roster<u64> = Roster::new(4);
    roster.admit(7);
    let occupants: Vec<bool> = roster.seats().map(|state| matches!(state, SeatState::Open)).collect();
    assert_eq!(occupants.iter().filter(|open| **open).count(), 3, "three seats for bots");
  }
}
