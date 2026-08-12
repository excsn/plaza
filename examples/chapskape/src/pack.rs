//! Twenty-eight squares that exist for one client.
//!
//! fog_skirmish filters a shared world per viewer, which is a different thing:
//! the fog hides something that is there for everybody. Nothing here is
//! filtered, because nobody else's world contains it. That makes a pack the one
//! genuinely private stream in this tree, and the instant worth watching is the
//! crossing: a drop turns private state into world state, and a pickup turns it
//! back.

use crate::protocol::Item;

/// Squares in a pack. Nothing stacks, which is what makes a full pack a thing
/// that happens rather than a limit nobody reaches.
pub const SLOTS: usize = 28;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
  slots: [Option<Item>; SLOTS],
}

impl Default for Pack {
  fn default() -> Self {
    Self::new()
  }
}

impl Pack {
  pub fn new() -> Self {
    Self {
      slots: [None; SLOTS],
    }
  }

  pub fn slots(&self) -> &[Option<Item>] {
    &self.slots
  }

  pub fn as_vec(&self) -> Vec<Option<Item>> {
    self.slots.to_vec()
  }

  pub fn get(&self, slot: usize) -> Option<Item> {
    self.slots.get(slot).copied().flatten()
  }

  pub fn is_full(&self) -> bool {
    self.slots.iter().all(|s| s.is_some())
  }

  pub fn count(&self) -> usize {
    self.slots.iter().filter(|s| s.is_some()).count()
  }

  pub fn count_of(&self, item: Item) -> usize {
    self.slots.iter().filter(|s| **s == Some(item)).count()
  }

  /// Puts something in the first free square, or says there was none.
  ///
  /// First free rather than appended, so a pack that has had a gap punched in
  /// it fills the gap. Anything else and a player who eats from the middle
  /// watches their pack grow past its own end.
  pub fn add(&mut self, item: Item) -> bool {
    for slot in self.slots.iter_mut() {
      if slot.is_none() {
        *slot = Some(item);
        return true;
      }
    }
    false
  }

  pub fn take(&mut self, slot: usize) -> Option<Item> {
    self.slots.get_mut(slot).and_then(|s| s.take())
  }

  /// Takes the first of something, for a caller that wants an item rather than
  /// a square.
  pub fn take_first(&mut self, item: Item) -> Option<usize> {
    let found = self.slots.iter().position(|s| *s == Some(item))?;
    self.slots[found] = None;
    Some(found)
  }

  pub fn find(&self, item: Item) -> Option<usize> {
    self.slots.iter().position(|s| *s == Some(item))
  }

  pub fn replace(&mut self, slot: usize, item: Item) {
    if let Some(square) = self.slots.get_mut(slot) {
      *square = Some(item);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_pack_fills_the_gap_rather_than_the_end() {
    // Otherwise a player who eats from the middle watches their pack grow past
    // its own last square and then refuse an item it plainly has room for.
    let mut pack = Pack::new();
    for _ in 0..SLOTS {
      assert!(pack.add(Item::Logs));
    }
    assert!(pack.is_full());
    assert!(!pack.add(Item::Ore), "a full pack took another");

    pack.take(3);
    assert!(pack.add(Item::Ore));
    assert_eq!(pack.get(3), Some(Item::Ore));
    assert!(pack.is_full());
  }

  #[test]
  fn taking_something_takes_exactly_one() {
    let mut pack = Pack::new();
    pack.add(Item::RawFish);
    pack.add(Item::RawFish);
    pack.add(Item::Logs);
    assert_eq!(pack.count_of(Item::RawFish), 2);
    assert_eq!(pack.take_first(Item::RawFish), Some(0));
    assert_eq!(pack.count_of(Item::RawFish), 1);
    assert_eq!(pack.count(), 2);
  }

  #[test]
  fn cooking_replaces_in_place() {
    // A fish that moved square when it was cooked would make the pack shuffle
    // under the player's hand for no reason they could see.
    let mut pack = Pack::new();
    pack.add(Item::Logs);
    pack.add(Item::RawFish);
    pack.replace(1, Item::CookedFish);
    assert_eq!(pack.get(0), Some(Item::Logs));
    assert_eq!(pack.get(1), Some(Item::CookedFish));
  }
}
