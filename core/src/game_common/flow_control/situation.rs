//! A mark of the situation as it stood, for work scheduled against it.
//!
//! [`Phased::epoch`](super::phases::Phased::epoch) tells you whether the phase
//! is still the same, but not whether the same *decision* is still pending. A
//! turn clock, a bot's think timer, a response window's deadline: each is
//! scheduled against a moment ("seat 3 owes the next action") that ends when
//! anything moves the game on. The phase epoch cannot see that, because the
//! phase did not change. Four examples wrote the same bare `key: u64` with the
//! same compare-on-fire guard before this existed.
//!
//! The marker type parameter is what keeps two situations in one state from
//! answering for each other: a `Mark<Ask>` does not compare against a
//! `Mark<March>`, the same guarantee [`Epoch`](super::phases::Epoch) gets from
//! its private field. A single-situation state uses the `()` default.

use std::fmt::Debug;
use std::marker::PhantomData;

/// A counter for one recurring decision. Advance it whenever the situation
/// moves on; everything scheduled against the old value then goes stale.
pub struct Situation<T = ()> {
  n: u64,
  _marker: PhantomData<fn() -> T>,
}

/// The situation as it stood when work was scheduled. Carried inside the
/// scheduled event and checked when it fires.
pub struct Mark<T = ()>(u64, PhantomData<fn() -> T>);

impl<T> Situation<T> {
  pub fn new() -> Self {
    Self {
      n: 0,
      _marker: PhantomData,
    }
  }

  /// The situation moved on; every outstanding [`Mark`] is now stale.
  pub fn advance(&mut self) {
    self.n += 1;
  }

  /// The mark to stamp scheduled work with.
  pub fn mark(&self) -> Mark<T> {
    Mark(self.n, PhantomData)
  }

  /// Whether the situation `mark` was taken in still stands.
  pub fn holds(&self, mark: Mark<T>) -> bool {
    self.n == mark.0
  }
}

impl<T> Default for Situation<T> {
  fn default() -> Self {
    Self::new()
  }
}

impl<T> Clone for Situation<T> {
  fn clone(&self) -> Self {
    Self {
      n: self.n,
      _marker: PhantomData,
    }
  }
}

impl<T> Debug for Situation<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("Situation").field(&self.n).finish()
  }
}

impl<T> Clone for Mark<T> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<T> Copy for Mark<T> {}

impl<T> PartialEq for Mark<T> {
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}

impl<T> Eq for Mark<T> {}

impl<T> Debug for Mark<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("Mark").field(&self.0).finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn advancing_strands_every_outstanding_mark() {
    let mut ask: Situation = Situation::new();
    let stamped = ask.mark();
    assert!(ask.holds(stamped), "nothing moved yet");

    ask.advance();
    assert!(!ask.holds(stamped), "the situation the clock was set in is gone");
    assert!(ask.holds(ask.mark()));
  }

  #[test]
  fn marks_are_of_their_moment_not_interchangeable_tickets() {
    let mut ask: Situation = Situation::new();
    let first = ask.mark();
    ask.advance();
    ask.advance();
    let third = ask.mark();
    assert_ne!(first, third);
    assert!(ask.holds(third) && !ask.holds(first));
  }
}
