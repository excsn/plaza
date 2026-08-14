//! Bots in the roster: real seats, no connection.
//!
//! The fact every example rediscovered separately: a bot must occupy a seat
//! through the **same admission as a person**, so capacity, seat numbering and
//! displacement stay one system, and must hold **no agent**, so it lives on
//! the simulation path and never the send path. Half of each discovery was a
//! near miss: a bot seated outside the roster double-books a seat the next
//! joiner is given, and a bot with an agent is a client the server pays to
//! frame for nobody.
//!
//! The keys are the caller's, usually carved from the top of the id space
//! (`Id::MAX - index`), so a person's id can never collide with a bot's. The
//! rank is the caller's too: people at 0 and bots at 1 is the classic, so a
//! bot holds a seat only until a person wants one, and [`prune`](Crew::prune)
//! is how the crew hears that it happened.

use std::collections::BTreeMap;
use std::hash::Hash;

use super::{Admission, Roster};

/// The seats a fleet of bots holds, and the bookkeeping the roster cannot do
/// for them because nothing else remembers which keys are bots.
#[derive(Clone, Debug, Default)]
pub struct Crew<Key: Eq + Hash + Clone> {
  seats: BTreeMap<usize, Key>,
}

impl<Key: Eq + Hash + Clone> Crew<Key> {
  pub fn new() -> Self {
    Self { seats: BTreeMap::new() }
  }

  /// Admits up to `count` bots and says which seats they took, in admission
  /// order.
  ///
  /// `key_of` names bot `index` (`0..count`) and owns uniqueness: give each
  /// fill its own namespace, or the second fill resumes the first's seats.
  /// Stops at the first admission that is not a seat, because a full roster
  /// stays full for every later bot too.
  pub fn fill(
    &mut self,
    roster: &mut Roster<Key>,
    count: usize,
    rank: u32,
    key_of: impl Fn(usize) -> Key,
  ) -> Vec<usize> {
    let mut taken = Vec::new();
    for index in 0..count {
      let key = key_of(index);
      let Admission::Seated { seat, .. } = roster.admit_ranked(key.clone(), rank) else {
        break;
      };
      self.seats.insert(seat, key);
      taken.push(seat);
    }
    taken
  }

  /// Whether this seat is a bot's.
  pub fn holds(&self, seat: usize) -> bool {
    self.seats.contains_key(&seat)
  }

  /// The crew's seats, ascending.
  ///
  /// Sorted is not tidiness: bot thinking usually draws from one shared random
  /// stream, and an iteration order that came out of a hash map is an order
  /// that decides who draws what.
  pub fn seats(&self) -> impl Iterator<Item = usize> + '_ {
    self.seats.keys().copied()
  }

  pub fn len(&self) -> usize {
    self.seats.len()
  }

  pub fn is_empty(&self) -> bool {
    self.seats.is_empty()
  }

  /// Stands a bot down, freeing its seat for whoever asks next.
  pub fn vacate(&mut self, roster: &mut Roster<Key>, seat: usize) -> bool {
    let Some(key) = self.seats.remove(&seat) else {
      return false;
    };
    roster.depart(&key);
    true
  }

  /// Drops every bot the roster no longer seats, and says which seats went.
  ///
  /// A bot admitted at a worse rank than people is displaced when a person
  /// wants its seat, at `resolve`, and the roster does that without asking.
  /// Call this after admissions and remove the departed bots from the
  /// simulation, or the crew keeps steering a seat that now belongs to
  /// somebody real. The pruned keys are also withdrawn from the roster,
  /// because a displaced key is requeued on the waitlist and would otherwise
  /// re-seat itself, as a stranger to the crew, the moment a seat opened.
  pub fn prune(&mut self, roster: &mut Roster<Key>) -> Vec<usize> {
    let gone: Vec<(usize, Key)> = self
      .seats
      .iter()
      .filter(|(seat, key)| roster.seat_of(key) != Some(**seat))
      .map(|(seat, key)| (*seat, key.clone()))
      .collect();
    for (seat, key) in &gone {
      self.seats.remove(seat);
      roster.depart(key);
    }
    gone.into_iter().map(|(seat, _)| seat).collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn bot_key(index: usize) -> u32 {
    u32::MAX - index as u32
  }

  #[test]
  fn a_crew_takes_real_seats_through_the_real_admission() {
    let mut roster: Roster<u32> = Roster::new(4);
    let mut crew = Crew::new();
    let taken = crew.fill(&mut roster, 3, 1, bot_key);
    assert_eq!(taken.len(), 3);
    assert_eq!(crew.len(), 3);
    for seat in &taken {
      assert!(crew.holds(*seat));
    }
    // The next person gets the one seat the crew left, not a double booking.
    let Admission::Seated { seat, .. } = roster.admit(7) else {
      panic!("a seat was left");
    };
    assert!(!crew.holds(seat));
  }

  #[test]
  fn a_full_roster_stops_the_fill_rather_than_erroring() {
    let mut roster: Roster<u32> = Roster::new(2);
    let mut crew = Crew::new();
    let taken = crew.fill(&mut roster, 5, 1, bot_key);
    assert_eq!(taken.len(), 2, "two seats existed");
    assert_eq!(crew.len(), 2);
  }

  #[test]
  fn a_person_displaces_a_bot_and_prune_reports_it() {
    // The classic ranking: people at 0, bots at 1, so a bot holds a seat only
    // until a person wants one. The roster displaces at `resolve` without
    // asking the crew; prune is how the crew finds out, and the simulation
    // must then stand that bot down or it keeps steering somebody real.
    let mut roster: Roster<u32> = Roster::new(2).with_waitlist();
    let mut crew = Crew::new();
    crew.fill(&mut roster, 2, 1, bot_key);

    assert!(matches!(roster.admit_ranked(7, 0), Admission::Waitlisted { .. }));
    let shuffles = roster.resolve();
    assert!(!shuffles.is_empty(), "the better rank moved somebody");

    let gone = crew.prune(&mut roster);
    assert_eq!(gone.len(), 1, "one bot lost its seat");
    assert_eq!(crew.len(), 1);
    assert_eq!(roster.seat_of(&7), Some(gone[0]), "and the person holds it now");

    // A displaced key is requeued by the roster; prune must have withdrawn it,
    // or the bot re-seats itself as a stranger the moment the person leaves.
    roster.depart(&7);
    let shuffles = roster.resolve();
    assert!(shuffles.is_empty(), "nobody was waiting: {shuffles:?}");
  }

  #[test]
  fn a_vacated_seat_is_open_again() {
    let mut roster: Roster<u32> = Roster::new(1);
    let mut crew = Crew::new();
    let taken = crew.fill(&mut roster, 1, 1, bot_key);
    assert!(crew.vacate(&mut roster, taken[0]));
    assert!(!crew.vacate(&mut roster, taken[0]), "vacating twice is not an error");
    assert!(matches!(roster.admit(7), Admission::Seated { .. }));
  }

  #[test]
  fn the_seats_come_back_sorted_whatever_order_they_were_taken() {
    let mut roster: Roster<u32> = Roster::new(8);
    let mut crew = Crew::new();
    crew.fill(&mut roster, 6, 1, bot_key);
    let seats: Vec<usize> = crew.seats().collect();
    let mut sorted = seats.clone();
    sorted.sort_unstable();
    assert_eq!(seats, sorted);
  }
}
