//! The tri-state slot map: which key holds which seat, counting seats kept
//! for someone who left. [`SeatTable`](super::SeatTable)'s sibling with one
//! more state, and one of the two blocks [`Roster`](super::Roster) is composed
//! of. Public for the same reason every prescription's blocks are: a seating
//! policy `Roster` does not express is built from these directly.

use std::collections::HashMap;
use std::hash::Hash;

use super::SeatState;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Slot<Key> {
  Open,
  Human(Key),
  Held(Key),
}

#[derive(Clone, Debug, Default)]
pub struct SeatSlots<Key: Eq + Hash + Clone> {
  slots: Vec<Slot<Key>>,
  index: HashMap<Key, usize>,
}

impl<Key: Eq + Hash + Clone> SeatSlots<Key> {
  pub fn new(capacity: usize) -> Self {
    Self {
      slots: vec![Slot::Open; capacity],
      index: HashMap::new(),
    }
  }

  /// The lowest open seat, if any.
  pub fn first_open(&self) -> Option<usize> {
    self.slots.iter().position(|slot| *slot == Slot::Open)
  }

  /// Puts `key` in `seat`, which must be open.
  pub fn seat(&mut self, key: Key, seat: usize) {
    debug_assert!(self.slots[seat] == Slot::Open, "seating into a non-open seat");
    self.slots[seat] = Slot::Human(key.clone());
    self.index.insert(key, seat);
  }

  /// `Human` to `Held`, keeping the seat theirs. `None` if they hold nothing
  /// or are already held.
  pub fn hold(&mut self, key: &Key) -> Option<usize> {
    let seat = *self.index.get(key)?;
    if !matches!(self.slots[seat], Slot::Human(_)) {
      return None;
    }
    self.slots[seat] = Slot::Held(key.clone());
    Some(seat)
  }

  /// `Held` back to `Human`, everything in the seat intact.
  pub fn resume(&mut self, key: &Key) -> Option<usize> {
    let seat = *self.index.get(key)?;
    if !matches!(self.slots[seat], Slot::Held(_)) {
      return None;
    }
    self.slots[seat] = Slot::Human(key.clone());
    Some(seat)
  }

  /// Opens `key`'s seat whatever state it was in, forgetting the key.
  pub fn open(&mut self, key: &Key) -> Option<usize> {
    let seat = self.index.remove(key)?;
    self.slots[seat] = Slot::Open;
    Some(seat)
  }

  pub fn seat_of(&self, key: &Key) -> Option<usize> {
    self.index.get(key).copied()
  }

  pub fn is_held(&self, key: &Key) -> bool {
    self
      .index
      .get(key)
      .is_some_and(|seat| matches!(self.slots[*seat], Slot::Held(_)))
  }

  pub fn state(&self, seat: usize) -> SeatState<'_, Key> {
    match &self.slots[seat] {
      Slot::Open => SeatState::Open,
      Slot::Human(key) => SeatState::Human(key),
      Slot::Held(key) => SeatState::Held(key),
    }
  }

  pub fn capacity(&self) -> usize {
    self.slots.len()
  }

  pub fn occupied_count(&self) -> usize {
    self.index.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_lifecycle_open_human_held_open() {
    let mut slots: SeatSlots<u64> = SeatSlots::new(2);
    assert_eq!(slots.first_open(), Some(0));
    slots.seat(1, 0);
    assert_eq!(slots.state(0), SeatState::Human(&1));
    assert_eq!(slots.first_open(), Some(1));

    assert_eq!(slots.hold(&1), Some(0));
    assert_eq!(slots.state(0), SeatState::Held(&1));
    assert_eq!(slots.hold(&1), None, "a held seat holds once");
    assert!(slots.is_held(&1));
    assert_eq!(slots.first_open(), Some(1), "a held seat is not open");

    assert_eq!(slots.resume(&1), Some(0));
    assert_eq!(slots.state(0), SeatState::Human(&1));
    assert_eq!(slots.resume(&1), None, "nothing to resume");

    assert_eq!(slots.open(&1), Some(0));
    assert_eq!(slots.state(0), SeatState::Open);
    assert_eq!(slots.open(&1), None);
    assert_eq!(slots.occupied_count(), 0);
  }

  #[test]
  fn the_index_survives_the_hold() {
    let mut slots: SeatSlots<u64> = SeatSlots::new(1);
    slots.seat(9, 0);
    slots.hold(&9);
    assert_eq!(slots.seat_of(&9), Some(0), "a held seat is still theirs");
  }
}
