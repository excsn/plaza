//! Subscription: the relevance a distance query cannot answer.
//!
//! [`relevance`](crate::relevance) answers *who is near me*, which is the
//! question every example in this tree asked until one asked a second one:
//! **who have I chosen to care about, wherever they are.** A party's health
//! bars update across a zone, raid frames work through a wall, a spectator
//! follows one player around a map, and a guild roster is not a distance query
//! at all.
//!
//! The two are different shapes as well as different questions, which is why
//! neither expresses the other. A grid query is a fresh answer every tick over
//! a set that changes constantly; a subscription is a handful of entries with a
//! lifetime measured in hours. Expressing a party as a relevance radius means
//! an infinite radius, and expressing a grid query as a subscription means
//! resubscribing everybody every tick.
//!
//! What this block is: a directed subscription set kept **both ways round**,
//! because both directions are asked every tick. A sender needs the set it must
//! include; a departing key needs everyone who has to be told it is gone. Kept
//! one way, the second question is a scan of every subscriber in the world.
//!
//! What stays the app's: whether a subscription is symmetric (a party) or not
//! (a spectator), what it costs, who may create one, and how it reaches the
//! wire. [`Subscriptions::group`] is here because symmetric membership is the
//! case that is easy to get subtly wrong, not because it is the only one.
//!
//! ## The union is the point
//!
//! A subscription channel is only expensive when its members are far away. Feed
//! [`Audience::of`] a spatial answer and a subscription set, and what it costs
//! is the members distance missed and nothing at all for the ones standing
//! beside you.
//!
//! Whatever reaches the wire has to say **why** each entity is in the frame.
//! "Near" and "subscribed" are different promises: the neighbour vanishes when
//! the viewer walks away and the subscribed entity does not, so a client that
//! cannot tell them apart cannot draw the interface the subscription exists
//! for, and will drop a party member the moment they leave view.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Why an entity is in an audience.
///
/// Not a hint, and not an optimisation. A client draws a nameplate for one and
/// a party frame for the other, and the two have different lifetimes: absence
/// from a later frame means "walked away" for [`Because::Near`] and "left the
/// world" for [`Because::Subscribed`].
///
/// **This does not cross the wire, and the copy in your protocol is
/// deliberate.** This crate carries no serde on purpose, and the coupling that
/// would follow is worse than the duplication: a protocol version is a hash of
/// the types on the wire, so a wire type owned by a library means upgrading
/// the library silently re-versions every application that uses it, and a
/// patch release disconnects clients. Spell it again in your protocol, three
/// variants and a name you chose, and let the two move on their own clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Because {
  /// Passed the spatial query.
  Near,
  /// Subscribed to, at any distance.
  Subscribed,
  /// Both, which is the common case and the reason the second channel is
  /// cheap.
  Either,
}

impl Because {
  pub fn is_near(self) -> bool {
    matches!(self, Because::Near | Because::Either)
  }

  pub fn is_subscribed(self) -> bool {
    matches!(self, Because::Subscribed | Because::Either)
  }
}

/// Who each key has chosen to care about, and who has chosen them.
///
/// Directed: `a` subscribing to `b` does not subscribe `b` to `a`. Use
/// [`group`](Self::group) when the relationship is symmetric, which is what a
/// party is: a one-sided party is a stalker.
#[derive(Debug, Clone)]
pub struct Subscriptions<K: Eq + Hash + Clone> {
  out: HashMap<K, HashSet<K>>,
  back: HashMap<K, HashSet<K>>,
  limit: usize,
}

impl<K: Eq + Hash + Clone> Default for Subscriptions<K> {
  fn default() -> Self {
    Self::new(usize::MAX)
  }
}

impl<K: Eq + Hash + Clone> Subscriptions<K> {
  /// A fresh set, refusing any subscription that would take a key past
  /// `limit` outgoing entries.
  ///
  /// Bounded because this is the channel with no natural ceiling: a spatial
  /// query is limited by how many entities fit in a radius, and a subscription
  /// is limited by nothing at all unless something says so.
  pub fn new(limit: usize) -> Self {
    Self {
      out: HashMap::new(),
      back: HashMap::new(),
      limit,
    }
  }

  /// Subscribes `who` to `to`. Returns false if that would pass the limit.
  ///
  /// Refused rather than truncated. Dropping an entry silently to fit is how a
  /// client ends up in a party that cannot see one of its members.
  pub fn subscribe(&mut self, who: K, to: K) -> bool {
    if who == to {
      return false;
    }
    let held = self.out.get(&who).map(|s| s.len()).unwrap_or(0);
    if held >= self.limit && !self.out.get(&who).is_some_and(|s| s.contains(&to)) {
      return false;
    }
    self.out.entry(who.clone()).or_default().insert(to.clone());
    self.back.entry(to).or_default().insert(who);
    true
  }

  /// Subscribes both ways, or neither.
  ///
  /// All-or-nothing because a half-applied symmetric relationship is worse
  /// than a refused one: one side draws a party frame and the other does not.
  pub fn pair(&mut self, a: K, b: K) -> bool {
    if a == b {
      return false;
    }
    let room = |set: &Self, who: &K, to: &K| {
      set.out.get(who).is_some_and(|s| s.contains(to)) || set.out.get(who).map(|s| s.len()).unwrap_or(0) < set.limit
    };
    if !room(self, &a, &b) || !room(self, &b, &a) {
      return false;
    }
    self.subscribe(a.clone(), b.clone());
    self.subscribe(b, a);
    true
  }

  /// Merges the groups holding `a` and `b` into one, everyone subscribed to
  /// everyone.
  ///
  /// A party joining a party, which is the operation that is easy to get wrong
  /// by adding one person to one side. Refused whole if the merged group would
  /// pass the limit, and nothing is changed when it is refused.
  pub fn group(&mut self, a: K, b: K) -> bool {
    if a == b {
      return false;
    }
    let mut members: Vec<K> = self.group_of(&a);
    for member in self.group_of(&b) {
      if !members.contains(&member) {
        members.push(member);
      }
    }
    // Each member ends up subscribed to everyone but themselves.
    if members.len().saturating_sub(1) > self.limit {
      return false;
    }
    for who in &members {
      for to in &members {
        if who != to {
          self.out.entry(who.clone()).or_default().insert(to.clone());
          self.back.entry(to.clone()).or_default().insert(who.clone());
        }
      }
    }
    true
  }

  /// Everyone in the symmetric group holding `key`, `key` included.
  ///
  /// A key with no subscriptions is a group of one, which is what makes
  /// [`group`](Self::group) work on a key that has never been seen.
  pub fn group_of(&self, key: &K) -> Vec<K> {
    let mut members = vec![key.clone()];
    if let Some(set) = self.out.get(key) {
      for other in set {
        // Symmetric only. A spectator's target is not in the spectator's
        // group, or following somebody would drag them into a party.
        if self.back.get(key).is_some_and(|b| b.contains(other)) {
          members.push(other.clone());
        }
      }
    }
    members
  }

  /// Drops one subscription, in one direction.
  pub fn unsubscribe(&mut self, who: &K, from: &K) {
    if let Some(set) = self.out.get_mut(who) {
      set.remove(from);
      if set.is_empty() {
        self.out.remove(who);
      }
    }
    if let Some(set) = self.back.get_mut(from) {
      set.remove(who);
      if set.is_empty() {
        self.back.remove(from);
      }
    }
  }

  /// Removes a key entirely, both directions, and returns everyone who was
  /// subscribed to it.
  ///
  /// The returned list is the point of keeping the reverse index: those are
  /// the clients whose interface still has an entry for something that is no
  /// longer here, and they are the ones that have to be told. Finding them by
  /// scanning every subscriber is the alternative, and it is the whole world.
  ///
  /// Call this on departure. A subscription that outlives the thing it is
  /// about is a health bar that keeps updating for somebody who left.
  pub fn remove(&mut self, key: &K) -> Vec<K> {
    let watchers: Vec<K> = self.back.remove(key).map(|s| s.into_iter().collect()).unwrap_or_default();
    for watcher in &watchers {
      if let Some(set) = self.out.get_mut(watcher) {
        set.remove(key);
        if set.is_empty() {
          self.out.remove(watcher);
        }
      }
    }
    if let Some(mine) = self.out.remove(key) {
      for to in mine {
        if let Some(set) = self.back.get_mut(&to) {
          set.remove(key);
          if set.is_empty() {
            self.back.remove(&to);
          }
        }
      }
    }
    watchers
  }

  /// Everyone `key` is subscribed to.
  pub fn of<'a>(&'a self, key: &K) -> impl Iterator<Item = &'a K> + 'a {
    self.out.get(key).into_iter().flatten()
  }

  /// Everyone subscribed to `key`.
  pub fn watchers<'a>(&'a self, key: &K) -> impl Iterator<Item = &'a K> + 'a {
    self.back.get(key).into_iter().flatten()
  }

  pub fn count_of(&self, key: &K) -> usize {
    self.out.get(key).map(|s| s.len()).unwrap_or(0)
  }

  pub fn is_subscribed(&self, who: &K, to: &K) -> bool {
    self.out.get(who).is_some_and(|s| s.contains(to))
  }

  /// Keys with at least one outgoing subscription.
  pub fn subscribers(&self) -> impl Iterator<Item = &K> {
    self.out.keys()
  }

  pub fn clear(&mut self) {
    self.out.clear();
    self.back.clear();
  }
}

/// What one client is told about this tick, and why.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Audience<K> {
  /// Everyone to include, each labelled with why they are here.
  pub entries: Vec<(K, Because)>,
  /// How many came from the spatial query.
  pub near: usize,
  /// How many the subscription added that distance did not. **This is the
  /// number the second channel actually costs.**
  pub added: usize,
}

impl<K: Eq + Hash + Clone + Ord> Audience<K> {
  /// Unions a spatial answer with a subscription set.
  ///
  /// `near` is whatever the relevance query returned, in any order; the result
  /// is sorted so a client sees a stable order across ticks and a diff against
  /// the previous one means something.
  pub fn of(near: &[K], subscriptions: &Subscriptions<K>, viewer: &K) -> Self {
    let close: HashSet<&K> = near.iter().collect();
    let mut entries: Vec<(K, Because)> = Vec::with_capacity(near.len());
    let mut added = 0;

    for key in near {
      let also = key != viewer && subscriptions.is_subscribed(viewer, key);
      entries.push((key.clone(), if also { Because::Either } else { Because::Near }));
    }
    for key in subscriptions.of(viewer) {
      if !close.contains(key) {
        entries.push((key.clone(), Because::Subscribed));
        added += 1;
      }
    }
    entries.sort_unstable();
    Self {
      near: near.len(),
      added,
      entries,
    }
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  pub fn keys(&self) -> impl Iterator<Item = &K> {
    self.entries.iter().map(|(key, _)| key)
  }

  /// Only the ones close enough to draw a body for.
  pub fn visible(&self) -> impl Iterator<Item = &K> {
    self
      .entries
      .iter()
      .filter(|(_, why)| why.is_near())
      .map(|(key, _)| key)
  }

  pub fn why(&self, key: &K) -> Option<Because> {
    self.entries.iter().find(|(k, _)| k == key).map(|(_, why)| *why)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_subscription_is_directed_unless_you_ask_for_both() {
    // A spectator follows a player and the player is not thereby following the
    // spectator, which is why this is not symmetric by default.
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    assert!(subs.subscribe(1, 2));
    assert!(subs.is_subscribed(&1, &2));
    assert!(!subs.is_subscribed(&2, &1));

    assert!(subs.pair(3, 4));
    assert!(subs.is_subscribed(&3, &4) && subs.is_subscribed(&4, &3));
  }

  #[test]
  fn nobody_subscribes_to_themselves() {
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    assert!(!subs.subscribe(1, 1));
    assert!(!subs.pair(1, 1));
    assert!(!subs.group(1, 1));
    assert_eq!(subs.count_of(&1), 0);
  }

  #[test]
  fn grouping_merges_both_groups_rather_than_adding_one_person() {
    // The operation that is easy to get subtly wrong: joining a party of two
    // to a party of two makes a party of four, not two parties of three.
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.pair(1, 2);
    subs.pair(3, 4);
    assert!(subs.group(2, 3));

    for key in 1..=4u32 {
      assert_eq!(subs.group_of(&key).len(), 4, "everyone sees the whole group");
      assert_eq!(subs.count_of(&key), 3, "and is subscribed to everyone but themselves");
    }
  }

  #[test]
  fn a_group_refuses_to_grow_past_the_limit_and_changes_nothing_when_it_does() {
    // Refused whole. A partial merge is the failure this returns false to
    // avoid: two of the four subscribed and the other two not.
    let mut subs: Subscriptions<u32> = Subscriptions::new(3);
    subs.pair(1, 2);
    subs.pair(3, 4);
    assert!(subs.group(1, 3), "four members is three subscriptions each");

    subs.pair(5, 6);
    assert!(!subs.group(1, 5), "six would be five each, past the limit");
    assert_eq!(subs.group_of(&1).len(), 4, "and the existing group is untouched");
    assert_eq!(subs.group_of(&5).len(), 2);
  }

  #[test]
  fn a_one_sided_subscription_is_not_a_group() {
    // Or following somebody would drag them into your party, and they would
    // find themselves sharing a health bar with a stranger.
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.subscribe(1, 2);
    assert_eq!(subs.group_of(&1), vec![1]);
    assert_eq!(subs.group_of(&2), vec![2]);
  }

  #[test]
  fn removing_a_key_names_everyone_who_has_to_be_told() {
    // The reason the reverse index exists. Without it this answer costs a scan
    // of every subscriber in the world, on every departure.
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.subscribe(1, 9);
    subs.subscribe(2, 9);
    subs.subscribe(9, 3);

    let mut told = subs.remove(&9);
    told.sort_unstable();
    assert_eq!(told, vec![1, 2]);

    assert_eq!(subs.count_of(&1), 0, "and their subscription is gone with it");
    assert_eq!(subs.watchers(&3).count(), 0, "both directions, or the reverse index leaks");
  }

  #[test]
  fn a_removed_key_leaves_nothing_behind() {
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.pair(1, 2);
    subs.remove(&1);
    assert_eq!(subs.count_of(&2), 0);
    assert_eq!(subs.subscribers().count(), 0, "no empty sets left holding memory");
  }

  #[test]
  fn an_audience_labels_why_each_entry_is_there() {
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.pair(1, 2);
    subs.pair(1, 9);

    // 2 is both, 3 is near only, 9 is subscribed only.
    let audience = Audience::of(&[1, 2, 3], &subs, &1);
    assert_eq!(audience.why(&1), Some(Because::Near), "the viewer is not their own subscription");
    assert_eq!(audience.why(&2), Some(Because::Either));
    assert_eq!(audience.why(&3), Some(Because::Near));
    assert_eq!(audience.why(&9), Some(Because::Subscribed));
    assert_eq!(audience.added, 1, "which cost exactly one entry");
    assert_eq!(audience.visible().count(), 3, "and 9 is described without being drawable");
  }

  #[test]
  fn a_subscriber_standing_beside_you_costs_nothing_extra() {
    // The union is what keeps the second channel cheap, and this is the case
    // it is cheap in: a party that stays together, which is most of the time.
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.pair(1, 2);
    let audience = Audience::of(&[1, 2, 3], &subs, &1);
    assert_eq!(audience.added, 0);
    assert_eq!(audience.len(), 3, "and nobody is listed twice");
  }

  #[test]
  fn an_audience_is_ordered_so_a_diff_between_ticks_means_something() {
    let mut subs: Subscriptions<u32> = Subscriptions::default();
    subs.pair(1, 40);
    let a = Audience::of(&[7, 1, 22], &subs, &1);
    let b = Audience::of(&[22, 7, 1], &subs, &1);
    assert_eq!(a, b, "the order the query returned is not the order sent");
    assert_eq!(a.keys().copied().collect::<Vec<_>>(), vec![1, 7, 22, 40]);
  }

  #[test]
  fn what_the_second_channel_costs() {
    // The number worth having, and the argument for the whole module: a
    // subscription is expensive only when its members are far away.
    let mut subs: Subscriptions<u32> = Subscriptions::new(4);
    subs.group(1, 2);
    subs.group(1, 3);
    subs.group(1, 4);
    subs.group(1, 5);
    assert_eq!(subs.count_of(&1), 4);

    for together in [0usize, 2, 4] {
      let mut near: Vec<u32> = (100..140).collect();
      near.push(1);
      near.extend((2..=5).take(together));
      let audience = Audience::of(&near, &subs, &1);
      assert_eq!(audience.added, 4 - together, "only the ones distance missed");
    }
  }
}
