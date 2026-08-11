//! A turn-based battle, which is the opposite of every other netcode in this
//! tree.
//!
//! Nothing here is predicted, interpolated, quantised or budgeted. Latency is
//! irrelevant: a turn takes as long as the slower player takes to choose, so a
//! second of delay costs nothing anyone can perceive. All the difficulty moves
//! somewhere the rest of these examples never look.
//!
//! **A choice is addressed to a turn.** That one decision does the work of
//! ordering, deduplication and reconnection together. An op that arrives twice
//! names a turn that has already resolved and is ignored; an op that arrives
//! late does the same; and a client that reconnects can be told the turn number
//! and work out for itself whether what it sent ever landed. Without it, the
//! obvious bug is a resend after a dropped connection applying a move twice,
//! and nothing about that looks wrong from either end.
//!
//! The creatures are invented, as are their names and everything they do.

use serde::{Deserialize, Serialize};

/// What a side can do with a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Choice {
  /// Trade damage, decided by speed.
  Strike,
  /// Take less this turn, and recover a little.
  Guard,
}

/// One creature, with the three numbers a battle needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creature {
  pub kind: u8,
  pub health: u8,
  pub power: u8,
  pub speed: u8,
}

impl Creature {
  /// The invented roster. Deliberately three, because a battle needs a reason
  /// to choose and not a collection to complete.
  pub fn of_kind(kind: u8) -> Self {
    match kind % 3 {
      0 => Self {
        kind: 0,
        health: 24,
        power: 6,
        speed: 5,
      },
      1 => Self {
        kind: 1,
        health: 18,
        power: 8,
        speed: 7,
      },
      _ => Self {
        kind: 2,
        health: 30,
        power: 5,
        speed: 3,
      },
    }
  }

  pub fn name(kind: u8) -> &'static str {
    match kind % 3 {
      0 => "Bramblet",
      1 => "Quillick",
      _ => "Mossgab",
    }
  }
}

/// Which side of a battle, by the seat sitting there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
  pub seat: u16,
  pub creature: Creature,
  /// What this side has chosen for the current turn, if anything.
  pub chosen: Option<Choice>,
}

/// What happened when a choice was offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
  /// Taken, and the turn is still waiting on the other side.
  Waiting,
  /// Taken, and both sides had chosen, so the turn resolved.
  Resolved,
  /// A choice for a turn that has already gone by.
  ///
  /// The case worth having a name for: this is what a resend after a dropped
  /// connection looks like, and treating it as a fresh choice is how a move
  /// gets applied twice.
  Stale { turn: u32 },
  /// A choice for a turn that has not started.
  Ahead { turn: u32 },
  /// From a seat that is not in this battle.
  NotYours,
  /// The battle is over.
  Finished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battle {
  /// Counted from one, and named by every choice.
  pub turn: u32,
  pub sides: [Side; 2],
  /// Whose seat won, once somebody has.
  pub winner: Option<u16>,
}

impl Battle {
  pub fn between(a: u16, b: u16, kinds: (u8, u8)) -> Self {
    Self {
      turn: 1,
      sides: [
        Side {
          seat: a,
          creature: Creature::of_kind(kinds.0),
          chosen: None,
        },
        Side {
          seat: b,
          creature: Creature::of_kind(kinds.1),
          chosen: None,
        },
      ],
      winner: None,
    }
  }

  fn index_of(&self, seat: u16) -> Option<usize> {
    self.sides.iter().position(|s| s.seat == seat)
  }

  pub fn finished(&self) -> bool {
    self.winner.is_some()
  }

  /// Offers a choice for a named turn.
  ///
  /// The turn number is the whole of the protocol. A duplicate names a turn
  /// that has resolved, so it is stale and ignored rather than applied; there
  /// is no sequence number, no dedup table and no window to keep.
  pub fn offer(&mut self, seat: u16, turn: u32, choice: Choice) -> Offered {
    if self.finished() {
      return Offered::Finished;
    }
    let Some(side) = self.index_of(seat) else {
      return Offered::NotYours;
    };
    if turn < self.turn {
      return Offered::Stale { turn: self.turn };
    }
    if turn > self.turn {
      return Offered::Ahead { turn: self.turn };
    }

    // Idempotent within the turn as well: choosing again before the other side
    // answers is a player changing their mind, which is allowed and is not a
    // second move.
    self.sides[side].chosen = Some(choice);
    if self.sides.iter().all(|s| s.chosen.is_some()) {
      self.resolve();
      return Offered::Resolved;
    }
    Offered::Waiting
  }

  /// Both choices are in, so the turn happens.
  ///
  /// Faster side first, which is the only thing speed does. Order matters
  /// because the slower side may not survive to act, and it has to be the same
  /// order on every machine that ever replays this.
  fn resolve(&mut self) {
    let (first, second) = if self.sides[0].creature.speed >= self.sides[1].creature.speed {
      (0, 1)
    } else {
      (1, 0)
    };

    for (actor, target) in [(first, second), (second, first)] {
      if self.finished() {
        break;
      }
      let choice = self.sides[actor].chosen.unwrap_or(Choice::Guard);
      match choice {
        Choice::Guard => {
          let creature = &mut self.sides[actor].creature;
          creature.health = creature.health.saturating_add(2).min(Creature::of_kind(creature.kind).health);
        }
        Choice::Strike => {
          let power = self.sides[actor].creature.power;
          let guarding = self.sides[target].chosen == Some(Choice::Guard);
          let damage = if guarding { power / 2 } else { power };
          let hit = &mut self.sides[target].creature;
          hit.health = hit.health.saturating_sub(damage);
          if hit.health == 0 {
            self.winner = Some(self.sides[actor].seat);
          }
        }
      }
    }

    for side in self.sides.iter_mut() {
      side.chosen = None;
    }
    self.turn += 1;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn battle() -> Battle {
    // Kinds 1 and 2: the fast weak one against the slow tough one, so speed
    // order is unambiguous and matters.
    Battle::between(7, 9, (1, 2))
  }

  #[test]
  fn a_turn_waits_for_both_sides() {
    let mut b = battle();
    assert_eq!(b.offer(7, 1, Choice::Strike), Offered::Waiting);
    assert_eq!(b.turn, 1, "nothing happens until both have answered");
    assert_eq!(b.offer(9, 1, Choice::Strike), Offered::Resolved);
    assert_eq!(b.turn, 2);
  }

  #[test]
  fn a_resent_choice_names_a_turn_that_has_gone_and_does_nothing() {
    // The bug this prevents: a client whose connection dropped resends its
    // choice, and the move happens twice. Nothing about that looks wrong from
    // either end, which is why the turn number is on the wire rather than a
    // sequence number the server has to remember.
    let mut b = battle();
    b.offer(7, 1, Choice::Strike);
    b.offer(9, 1, Choice::Strike);
    let after = b.clone();

    assert_eq!(b.offer(7, 1, Choice::Strike), Offered::Stale { turn: 2 });
    assert_eq!(b, after, "a stale choice must change nothing at all");
  }

  #[test]
  fn a_choice_for_a_later_turn_is_refused_rather_than_queued() {
    // Queuing it would mean a client could pre-commit to a turn it has not
    // seen, and the server would be holding intent it cannot show anybody.
    let mut b = battle();
    assert_eq!(b.offer(7, 5, Choice::Strike), Offered::Ahead { turn: 1 });
    assert_eq!(b.sides[0].chosen, None);
  }

  #[test]
  fn changing_your_mind_before_the_other_side_answers_is_not_a_second_move() {
    let mut b = battle();
    b.offer(7, 1, Choice::Strike);
    assert_eq!(b.offer(7, 1, Choice::Guard), Offered::Waiting);
    assert_eq!(b.sides[0].chosen, Some(Choice::Guard), "the later choice stands");
    assert_eq!(b.turn, 1, "and the turn has still not resolved");
  }

  #[test]
  fn the_faster_side_acts_first_and_that_can_decide_it() {
    // Order is the only thing speed does, and it has to be the same order
    // everywhere: the slower side may not survive to act.
    let mut b = Battle::between(1, 2, (1, 1));
    b.sides[0].creature.speed = 9;
    b.sides[1].creature.speed = 1;
    b.sides[1].creature.health = 4;
    b.sides[0].creature.power = 9;

    b.offer(1, 1, Choice::Strike);
    b.offer(2, 1, Choice::Strike);
    assert_eq!(b.winner, Some(1), "the fast one landed first");
    assert_eq!(b.sides[0].creature.health, 18, "and took nothing back");
  }

  #[test]
  fn guarding_halves_what_lands_and_recovers_a_little() {
    let mut b = battle();
    let before = b.sides[1].creature.health;
    b.offer(7, 1, Choice::Strike);
    b.offer(9, 1, Choice::Guard);
    let took = before - b.sides[1].creature.health;
    assert!(took > 0 && took < 8, "half of eight, plus two back: {took}");
  }

  #[test]
  fn a_seat_that_is_not_in_the_battle_is_refused() {
    let mut b = battle();
    assert_eq!(b.offer(99, 1, Choice::Strike), Offered::NotYours);
  }

  #[test]
  fn nothing_is_accepted_once_it_is_over() {
    let mut b = Battle::between(1, 2, (1, 1));
    b.sides[1].creature.health = 1;
    b.offer(1, 1, Choice::Strike);
    b.offer(2, 1, Choice::Strike);
    assert!(b.finished());
    assert_eq!(b.offer(1, b.turn, Choice::Strike), Offered::Finished);
  }
}
