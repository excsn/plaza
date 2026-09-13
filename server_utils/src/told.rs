//! What each viewer has been told, so nothing is re-sent for a world that is
//! not changing.
//!
//! A frame that repeats everything in view every tick suits movers and wastes
//! bandwidth on anything that holds still: a depleted resource, a door, a spawn
//! announced once. Every example built the alternative by hand: a per-viewer
//! memory of what was already said, diffed against what is now true. This is
//! that memory, with the three cases the hand-rolled copies found one bug at a
//! time:
//!
//! - a key the viewer does not hold or holds with another value is **said**;
//! - a key they hold that is no longer in their view is **handed back to the
//!   caller**, because "no longer true" and "no longer visible" arrive as the
//!   same absence and only the application knows which it was: a prop that
//!   reverted while still in view must be said (or the client draws a stump
//!   for ever), one that fell out of view is forgotten silently;
//! - a key forgotten either way is said again in full if it returns, which is
//!   also what lets a reused slot be re-announced instead of inheriting its
//!   predecessor's entry.
//!
//! The change-only stream this produces needs **a stable value to diff
//! against**. A value that jitters is said every tick and the saving is lost;
//! measure with a `RateMeter` before assuming.
//!
//! This is the **state half** of a private channel: facts that stay true for a
//! while, repeated whenever they change. The transcript half ("what just
//! happened", said once to its one audience) is a `Vec` drained into the frame,
//! so it is not a block. A channel needs both: a transcript on a shared channel
//! makes "who is this for" a field somebody forgets and a state on the
//! transcript is an event the client has to remember for ever.

use std::collections::HashMap;
use std::hash::Hash;

/// Per-viewer memory of what was last said, diffed on demand.
///
/// `V = ()` is the announce-once case: a key is said the tick it first
/// appears in the viewer's view, and never again until it leaves and returns.
#[derive(Clone, Debug)]
pub struct Told<Viewer: Eq + Hash, K: Eq + Hash + Ord + Copy, V: PartialEq> {
  known: HashMap<Viewer, HashMap<K, V>>,
}

impl<Viewer: Eq + Hash, K: Eq + Hash + Ord + Copy, V: PartialEq> Default for Told<Viewer, K, V> {
  fn default() -> Self {
    Self::new()
  }
}

impl<Viewer: Eq + Hash, K: Eq + Hash + Ord + Copy, V: PartialEq> Told<Viewer, K, V> {
  pub fn new() -> Self {
    Self { known: HashMap::new() }
  }

  /// Diffs what `viewer` now sees against what they were last told, updates
  /// the record and hands each difference to `say`.
  ///
  /// `say(key, Some(value))` is a key to put on the wire: new to this viewer,
  /// or changed since they heard of it. `say(key, None)` is a key they hold
  /// that `current` no longer contains; the caller decides whether that goes
  /// on the wire and the record forgets it either way, so a returning key is
  /// announced from scratch.
  ///
  /// The `None` keys arrive sorted, so two identical runs produce identical
  /// wire output: they come out of a map whose order would otherwise decide the
  /// bytes.
  pub fn diff(
    &mut self,
    viewer: Viewer,
    current: impl IntoIterator<Item = (K, V)>,
    mut say: impl FnMut(K, Option<&V>),
  ) {
    let known = self.known.entry(viewer).or_default();
    let mut next: HashMap<K, V> = HashMap::with_capacity(known.len());
    for (key, value) in current {
      if known.get(&key) != Some(&value) {
        say(key, Some(&value));
      }
      known.remove(&key);
      next.insert(key, value);
    }
    let mut gone: Vec<K> = known.keys().copied().collect();
    gone.sort_unstable();
    for key in gone {
      say(key, None);
    }
    *known = next;
  }

  /// Forgets one viewer entirely: on departure or when switching them to a
  /// repeat-everything stream, where a stale memory would make the first
  /// change-only frame after switching back wrong.
  pub fn forget(&mut self, viewer: &Viewer) {
    self.known.remove(viewer);
  }

  /// How many keys this viewer currently holds.
  pub fn holdings(&self, viewer: &Viewer) -> usize {
    self.known.get(viewer).map_or(0, HashMap::len)
  }

  pub fn viewers(&self) -> usize {
    self.known.len()
  }

  pub fn clear(&mut self) {
    self.known.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn said(told: &mut Told<u32, u32, u8>, viewer: u32, current: &[(u32, u8)]) -> (Vec<(u32, u8)>, Vec<u32>) {
    let (mut updated, mut gone) = (Vec::new(), Vec::new());
    told.diff(viewer, current.iter().copied(), |key, value| match value {
      Some(v) => updated.push((key, *v)),
      None => gone.push(key),
    });
    (updated, gone)
  }

  #[test]
  fn a_still_world_is_said_once() {
    let mut told = Told::new();
    let world = [(1u32, 7u8), (2, 9)];
    let (updated, gone) = said(&mut told, 5, &world);
    assert_eq!(updated, vec![(1, 7), (2, 9)], "a fresh viewer hears everything");
    assert!(gone.is_empty());

    let (updated, gone) = said(&mut told, 5, &world);
    assert!(updated.is_empty(), "and then nothing while nothing moves");
    assert!(gone.is_empty());
  }

  #[test]
  fn a_change_is_said_and_the_rest_stays_quiet() {
    let mut told = Told::new();
    said(&mut told, 5, &[(1, 7), (2, 9)]);
    let (updated, _) = said(&mut told, 5, &[(1, 7), (2, 10)]);
    assert_eq!(updated, vec![(2, 10)]);
  }

  #[test]
  fn each_viewer_has_their_own_memory() {
    let mut told = Told::new();
    said(&mut told, 5, &[(1, 7)]);
    let (updated, _) = said(&mut told, 6, &[(1, 7)]);
    assert_eq!(updated, vec![(1, 7)], "a joiner hears what the veteran already knows");
  }

  #[test]
  fn a_key_that_leaves_is_handed_back_and_returns_as_new() {
    // "No longer true" and "no longer visible" are the same absence here and
    // only the caller knows which; the record forgets either way, so a
    // returning key is announced again rather than inheriting its old entry.
    let mut told = Told::new();
    said(&mut told, 5, &[(1, 7), (2, 9)]);
    let (updated, gone) = said(&mut told, 5, &[(1, 7)]);
    assert!(updated.is_empty());
    assert_eq!(gone, vec![2]);

    let (updated, _) = said(&mut told, 5, &[(1, 7), (2, 9)]);
    assert_eq!(updated, vec![(2, 9)], "back means said again");
  }

  #[test]
  fn announce_once_is_the_unit_value_case() {
    // spacemo's bolt spawns: said the tick they appear, pruned when they die,
    // and a reused slot id announces again because the record forgot it.
    let mut told: Told<u32, u32, ()> = Told::new();
    let (mut first, mut second) = (Vec::new(), Vec::new());
    told.diff(5, [(40, ()), (41, ())], |key, value| {
      if value.is_some() {
        first.push(key);
      }
    });
    told.diff(5, [(40, ()), (41, ()), (42, ())], |key, value| {
      if value.is_some() {
        second.push(key);
      }
    });
    assert_eq!(first, vec![40, 41]);
    assert_eq!(second, vec![42], "only the new spawn is announced");
  }

  #[test]
  fn a_forgotten_viewer_hears_everything_again() {
    let mut told = Told::new();
    said(&mut told, 5, &[(1, 7)]);
    told.forget(&5);
    let (updated, _) = said(&mut told, 5, &[(1, 7)]);
    assert_eq!(updated, vec![(1, 7)]);
  }
}
