//! Two people agreeing on something, with a server as the only thing that may
//! commit it.
//!
//! Not a state broadcast and not a rollback: nobody is predicting anything and
//! there is no world to reconcile. What there is instead is an **agreement**,
//! which is a shape neither of the other regimes in this tree has. Both sides
//! offer, both confirm, and only then does anything change hands; until then
//! either can walk away and both are exactly where they started.
//!
//! The rule that carries the whole thing is that **changing an offer clears
//! both confirmations**. Without it there is a bait and switch: confirm what
//! you can see, then swap what you are giving before the commit lands. Every
//! trade window that has ever shipped without that rule has had the same
//! exploit, and it is one line.

use serde::{Deserialize, Serialize};

/// Where a trade has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
  /// Waiting for both sides to put something up.
  Offering,
  /// Both have offered; waiting on confirmations.
  Confirming,
  /// Both confirmed and the swap happened.
  Done,
  /// Somebody walked away.
  Withdrawn { by: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trade {
  pub seats: [u16; 2],
  /// What each side is putting up, by creature kind.
  pub offers: [Option<u8>; 2],
  pub confirmed: [bool; 2],
  pub stage: Stage,
}

/// What came of an attempt to do something to a trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
  /// Taken, and the trade is still going.
  Ok,
  /// Taken, and that was the last thing needed.
  Committed,
  /// From a seat not in this trade.
  NotYours,
  /// The trade is already over.
  Over,
  /// Confirming before both sides have offered.
  TooSoon,
}

impl Trade {
  pub fn between(a: u16, b: u16) -> Self {
    Self {
      seats: [a, b],
      offers: [None, None],
      confirmed: [false, false],
      stage: Stage::Offering,
    }
  }

  fn index_of(&self, seat: u16) -> Option<usize> {
    self.seats.iter().position(|s| *s == seat)
  }

  pub fn over(&self) -> bool {
    matches!(self.stage, Stage::Done | Stage::Withdrawn { .. })
  }

  /// Puts something up, or changes what is already up.
  ///
  /// **Both confirmations are cleared.** Changing what you are giving after
  /// somebody has agreed to it means they agreed to something else, so their
  /// agreement is no longer about this trade.
  pub fn offer(&mut self, seat: u16, kind: u8) -> Moved {
    if self.over() {
      return Moved::Over;
    }
    let Some(side) = self.index_of(seat) else {
      return Moved::NotYours;
    };
    self.offers[side] = Some(kind);
    self.confirmed = [false, false];
    self.stage = if self.offers.iter().all(|o| o.is_some()) {
      Stage::Confirming
    } else {
      Stage::Offering
    };
    Moved::Ok
  }

  /// Agrees to what is on the table, which is only meaningful once there is
  /// something on both sides of it.
  pub fn confirm(&mut self, seat: u16) -> Moved {
    if self.over() {
      return Moved::Over;
    }
    let Some(side) = self.index_of(seat) else {
      return Moved::NotYours;
    };
    if self.stage != Stage::Confirming {
      return Moved::TooSoon;
    }
    self.confirmed[side] = true;
    if self.confirmed.iter().all(|c| *c) {
      self.stage = Stage::Done;
      return Moved::Committed;
    }
    Moved::Ok
  }

  /// Walks away, at any point, leaving both sides exactly where they were.
  pub fn withdraw(&mut self, seat: u16) -> Moved {
    if self.over() {
      return Moved::Over;
    }
    if self.index_of(seat).is_none() {
      return Moved::NotYours;
    }
    self.stage = Stage::Withdrawn { by: seat };
    Moved::Ok
  }

  /// What each side ends up with, once it is done.
  ///
  /// Only ever answered for a committed trade: a withdrawn one has no result,
  /// and returning halves of one is how a caller ends up applying half a swap.
  pub fn outcome(&self) -> Option<[(u16, u8); 2]> {
    if self.stage != Stage::Done {
      return None;
    }
    Some([
      (self.seats[0], self.offers[1]?),
      (self.seats[1], self.offers[0]?),
    ])
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn trade() -> Trade {
    Trade::between(4, 5)
  }

  #[test]
  fn nothing_changes_hands_until_both_have_confirmed() {
    let mut t = trade();
    assert_eq!(t.offer(4, 0), Moved::Ok);
    assert_eq!(t.stage, Stage::Offering, "one offer is not a trade");
    assert_eq!(t.offer(5, 1), Moved::Ok);
    assert_eq!(t.stage, Stage::Confirming);
    assert_eq!(t.outcome(), None, "and still nothing has happened");

    assert_eq!(t.confirm(4), Moved::Ok);
    assert_eq!(t.outcome(), None, "one agreement is not agreement");
    assert_eq!(t.confirm(5), Moved::Committed);
    assert_eq!(t.outcome(), Some([(4, 1), (5, 0)]), "and they swap");
  }

  #[test]
  fn changing_an_offer_clears_both_confirmations() {
    // The exploit this prevents: agree to what you can see, then swap what you
    // are giving before the commit lands. Every trade window shipped without
    // this rule has had it.
    let mut t = trade();
    t.offer(4, 0);
    t.offer(5, 1);
    t.confirm(5);
    assert_eq!(t.confirmed, [false, true]);

    // Seat four changes what it is giving after five agreed to the old one.
    t.offer(4, 2);
    assert_eq!(t.confirmed, [false, false], "five agreed to something else");
    assert_eq!(t.stage, Stage::Confirming, "and is asked again");

    t.confirm(4);
    assert_eq!(t.outcome(), None, "so one confirmation cannot finish it");
  }

  #[test]
  fn confirming_before_there_is_anything_to_confirm_is_refused() {
    let mut t = trade();
    assert_eq!(t.confirm(4), Moved::TooSoon);
    t.offer(4, 0);
    assert_eq!(t.confirm(4), Moved::TooSoon, "still only one side of a trade");
  }

  #[test]
  fn withdrawing_leaves_both_sides_where_they_started() {
    let mut t = trade();
    t.offer(4, 0);
    t.offer(5, 1);
    t.confirm(4);
    assert_eq!(t.withdraw(5), Moved::Ok);
    assert_eq!(t.stage, Stage::Withdrawn { by: 5 });
    assert_eq!(t.outcome(), None, "a withdrawn trade has no result to apply");
    assert_eq!(t.confirm(4), Moved::Over, "and nothing can restart it");
  }

  #[test]
  fn a_committed_trade_cannot_be_touched_again() {
    // Which is what makes a resend after a dropped connection harmless here,
    // the same property the battle gets from naming its turn.
    let mut t = trade();
    t.offer(4, 0);
    t.offer(5, 1);
    t.confirm(4);
    t.confirm(5);
    let done = t.clone();
    assert_eq!(t.confirm(5), Moved::Over);
    assert_eq!(t.offer(4, 2), Moved::Over);
    assert_eq!(t, done, "a finished trade is finished");
  }

  #[test]
  fn a_stranger_cannot_reach_into_a_trade() {
    let mut t = trade();
    assert_eq!(t.offer(99, 0), Moved::NotYours);
    assert_eq!(t.confirm(99), Moved::NotYours);
    assert_eq!(t.withdraw(99), Moved::NotYours);
  }

  #[test]
  fn an_unfinished_trade_never_yields_half_a_swap() {
    // A caller that applied one side of this would be creating a creature and
    // destroying another, which is worse than the trade failing.
    let mut t = trade();
    t.offer(4, 0);
    assert_eq!(t.outcome(), None);
    t.offer(5, 1);
    assert_eq!(t.outcome(), None);
    t.confirm(4);
    assert_eq!(t.outcome(), None);
  }
}
