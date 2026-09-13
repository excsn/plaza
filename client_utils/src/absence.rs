//! Acting on an entity the server has stopped mentioning.
//!
//! Relevance filtering has no despawn packet: a server that stops mentioning
//! an entity has said "you cannot see this any more" and no other message
//! will say it. The client has to act on silence. The application decides how
//! long to wait and which entities are exempt; there are no defaults.
//!
//! **The grace period.** A frame is a *set* and an entity at the edge of the
//! view radius flickers in and out of it as both ends drift. Dropping on the
//! first silent frame makes entities at the edge of the world flicker. A short
//! grace smooths that out. An entity streamed every frame it exists (a homing
//! missile, a cursor) can use a short grace, because its silence means it is
//! gone rather than skipped for bandwidth budget.
//!
//! **The exemptions.** A client must never forget its own entity. An entity
//! sent once by design (a spawn-only projectile) is never mentioned again, so
//! silence must not remove it either. Both are the `seen_of` closure answering
//! `None`.

use std::collections::HashMap;
use std::hash::Hash;

/// How long an entity may go unmentioned before its absence means it is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Silence {
  grace: u64,
}

impl Silence {
  /// `grace` is in whatever unit `seen` and `now` are stamped in, usually the
  /// frame number the entity was last mentioned on.
  ///
  /// # Panics
  /// Panics if `grace` is zero, which would forget everything every sweep.
  pub const fn new(grace: u64) -> Self {
    assert!(grace > 0, "a zero grace forgets the world on the first sweep");
    Self { grace }
  }

  pub const fn grace(&self) -> u64 {
    self.grace
  }

  /// Whether an entity last mentioned at `seen` is still present at `now`.
  pub fn keeps(&self, seen: u64, now: u64) -> bool {
    now.saturating_sub(seen) < self.grace
  }

  /// Drops everything whose silence has exceeded the grace and says how many.
  ///
  /// `seen_of` returns when the entity was last mentioned. It returns `None`
  /// for one that silence must never remove: the client's own entity or an
  /// entity sent once by design and never mentioned again.
  pub fn sweep<K: Eq + Hash, V, F: Fn(&K, &V) -> Option<u64>>(
    &self,
    entities: &mut HashMap<K, V>,
    now: u64,
    seen_of: F,
  ) -> usize {
    let before = entities.len();
    entities.retain(|key, value| match seen_of(key, value) {
      Some(seen) => self.keeps(seen, now),
      None => true,
    });
    before - entities.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn silence_ends_an_entity_only_after_the_grace() {
    let silence = Silence::new(3);
    let mut entities: HashMap<u32, u64> = [(1, 10u64)].into();
    assert_eq!(silence.sweep(&mut entities, 12, |_, seen| Some(*seen)), 0, "two frames quiet is not gone");
    assert_eq!(silence.sweep(&mut entities, 13, |_, seen| Some(*seen)), 1, "three is");
    assert!(entities.is_empty());
  }

  #[test]
  fn an_exempt_entity_outlives_any_silence() {
    // The client's own entity or one sent once by design: `seen_of` returns
    // None to exempt it.
    let silence = Silence::new(2);
    let mut entities: HashMap<u32, u64> = [(1, 0u64), (2, 0)].into();
    let dropped = silence.sweep(&mut entities, 1000, |key, seen| (*key != 1).then_some(*seen));
    assert_eq!(dropped, 1);
    assert!(entities.contains_key(&1), "the exemption held");
  }

  #[test]
  fn a_mention_now_is_never_silence() {
    let silence = Silence::new(1);
    assert!(silence.keeps(5, 5), "mentioned this frame");
    assert!(!silence.keeps(4, 5), "the tightest grace drops on the first quiet frame");
  }
}
