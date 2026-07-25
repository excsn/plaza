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

#[cfg(test)]
mod tests {
  use super::*;

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
    let mut baseline = vec![0u32; 2];

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
