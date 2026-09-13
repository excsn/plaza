//! Two channels of relevance, because an MMO has two questions and plaza only
//! answers one of them.
//!
//! **Spatial**: who is near me. That is `SpatialGrid`, it is what every example
//! in this tree uses, and it is rebuilt every tick because everyone moves.
//!
//! **Subscription**: who have I chosen to care about, wherever they are. Your
//! party's health bars update across the zone, your raid frames work through a
//! wall and a guild roster is not a distance query at all. Nothing in plaza
//! has a concept of it.
//!
//! The two also have different shapes, so one cannot be expressed as the
//! other. A grid query is a fresh answer every tick over a set that changes
//! constantly; a party is five entries that last about an hour. Modelling a
//! party as a relevance radius would need an infinite radius, while modelling a
//! grid query as a subscription would mean resubscribing everybody every tick.

use std::collections::{HashMap, HashSet};

/// A seat in the zone.
pub type Seat = u16;

/// The most a party holds. Small on purpose: a subscription set is small and
/// long-lived, unlike a grid query.
pub const PARTY_MAX: usize = 5;

/// Who each seat has chosen to care about, and who has chosen them.
///
/// Kept both ways round because both are asked every tick: a sender needs the
/// set it must include, and a leaver needs everyone who has to be told.
#[derive(Default)]
pub struct Parties {
  members: HashMap<Seat, Vec<Seat>>,
}

impl Parties {
  pub fn new() -> Self {
    Self::default()
  }

  /// Puts two seats in a party together, and everyone either was with.
  ///
  /// Symmetric: each member sees the other's health bar, so one side cannot
  /// hold the membership alone.
  pub fn join(&mut self, a: Seat, b: Seat) -> bool {
    let mut party: Vec<Seat> = self
      .members
      .get(&a)
      .cloned()
      .unwrap_or_else(|| vec![a])
      .into_iter()
      .chain(self.members.get(&b).cloned().unwrap_or_else(|| vec![b]))
      .collect();
    party.sort_unstable();
    party.dedup();
    if party.len() > PARTY_MAX {
      return false;
    }
    for seat in &party {
      self.members.insert(*seat, party.clone());
    }
    true
  }

  /// Takes a seat out, and dissolves what is left if it is now one person.
  pub fn leave(&mut self, seat: Seat) {
    let Some(party) = self.members.remove(&seat) else {
      return;
    };
    let left: Vec<Seat> = party.into_iter().filter(|s| *s != seat).collect();
    if left.len() < 2 {
      // A party of one is dissolved. Keeping it would cost a set lookup every
      // tick for nothing.
      for other in left {
        self.members.remove(&other);
      }
      return;
    }
    for other in &left {
      self.members.insert(*other, left.clone());
    }
  }

  /// Everyone this seat is subscribed to, itself excluded.
  pub fn of(&self, seat: Seat) -> impl Iterator<Item = Seat> + '_ {
    self
      .members
      .get(&seat)
      .into_iter()
      .flatten()
      .copied()
      .filter(move |s| *s != seat)
  }

  pub fn size_of(&self, seat: Seat) -> usize {
    self.members.get(&seat).map(|p| p.len()).unwrap_or(1)
  }

  pub fn parties(&self) -> usize {
    let mut seen: HashSet<Vec<Seat>> = HashSet::new();
    for party in self.members.values() {
      seen.insert(party.clone());
    }
    seen.len()
  }
}

/// What one client is told about this tick, and why.
///
/// A union of the two: a party member standing next to you is **one** entry
/// rather than two, so the subscription channel costs only the members that
/// the spatial one did not already cover.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Audience {
  pub seats: Vec<Seat>,
  /// How many came from the spatial query.
  pub near: usize,
  /// How many the subscription added that distance did not.
  pub subscribed: usize,
}

/// Unions a grid answer with a subscription set.
pub fn audience(near: &[Seat], parties: &Parties, seat: Seat) -> Audience {
  let mut seats: Vec<Seat> = near.to_vec();
  let already: HashSet<Seat> = near.iter().copied().collect();
  let mut added = 0;
  for member in parties.of(seat) {
    if !already.contains(&member) {
      seats.push(member);
      added += 1;
    }
  }
  seats.sort_unstable();
  Audience {
    near: near.len(),
    subscribed: added,
    seats,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_party_member_across_the_zone_is_in_the_audience_and_a_stranger_is_not() {
    // Distance decides who you can see and a subscription adds who you are
    // told about regardless of distance.
    let mut parties = Parties::new();
    parties.join(1, 9);

    let near = [1, 2, 3];
    let a = audience(&near, &parties, 1);
    assert!(a.seats.contains(&9), "a party member is included at any distance");
    assert!(!a.seats.contains(&4), "and a stranger across the zone is not");
    assert_eq!(a.subscribed, 1, "which cost exactly one entry");
  }

  #[test]
  fn a_party_member_standing_next_to_you_costs_nothing_extra() {
    // Taking the union keeps the second channel cheap: it only adds members
    // the first did not already cover.
    let mut parties = Parties::new();
    parties.join(1, 2);
    let a = audience(&[1, 2, 3], &parties, 1);
    assert_eq!(a.subscribed, 0);
    assert_eq!(a.seats, vec![1, 2, 3], "and nobody is listed twice");
  }

  #[test]
  fn a_party_is_symmetric_because_a_health_bar_goes_both_ways() {
    let mut parties = Parties::new();
    parties.join(4, 7);
    assert_eq!(parties.of(4).collect::<Vec<_>>(), vec![7]);
    assert_eq!(parties.of(7).collect::<Vec<_>>(), vec![4]);
  }

  #[test]
  fn joining_merges_the_two_parties_rather_than_adding_one_person() {
    let mut parties = Parties::new();
    parties.join(1, 2);
    parties.join(3, 4);
    assert!(parties.join(2, 3), "two pairs make a four");
    assert_eq!(parties.size_of(1), 4);
    assert_eq!(parties.of(1).collect::<Vec<_>>(), vec![2, 3, 4]);
  }

  #[test]
  fn a_party_refuses_to_grow_past_its_limit() {
    // Refused rather than truncated: dropping somebody silently to fit is how
    // a player ends up in a party that cannot see them.
    let mut parties = Parties::new();
    parties.join(1, 2);
    parties.join(3, 4);
    parties.join(2, 3);
    parties.join(5, 6);
    assert!(!parties.join(4, 5), "four and two is past five");
    assert_eq!(parties.size_of(1), 4, "and the existing party is untouched");
    assert_eq!(parties.size_of(5), 2);
  }

  #[test]
  fn the_last_two_leaving_dissolves_the_party() {
    // A party of one is dissolved. Keeping it would cost a lookup every tick
    // for nothing.
    let mut parties = Parties::new();
    parties.join(1, 2);
    parties.join(2, 3);
    assert_eq!(parties.size_of(1), 3);

    parties.leave(3);
    assert_eq!(parties.size_of(1), 2, "two is still a party");
    parties.leave(2);
    assert_eq!(parties.size_of(1), 1, "one is not");
    assert_eq!(parties.parties(), 0);
    assert_eq!(parties.of(1).count(), 0);
  }

  #[test]
  fn what_the_second_channel_costs() {
    // A subscription only costs entries for members who are far away, which
    // is rare in a game where parties stay together.
    let mut parties = Parties::new();
    parties.join(1, 2);
    parties.join(2, 3);
    parties.join(3, 4);
    parties.join(4, 5);

    println!("\n  a party of {} against a view of 40:\n", parties.size_of(1));
    for together in [0usize, 2, 4] {
      let mut near: Vec<Seat> = (100..140).collect();
      near.push(1);
      near.extend((2..=5).take(together));
      near.sort_unstable();
      let a = audience(&near, &parties, 1);
      println!(
        "    {together} of them nearby: {} near, {} added by subscription",
        a.near, a.subscribed
      );
      assert_eq!(a.subscribed, 4 - together, "only the ones distance missed");
    }
    println!("\n  the second channel costs the members the first one missed, and\n  nothing at all for the ones standing beside you.\n");
  }
}
