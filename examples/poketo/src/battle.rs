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
///
/// An index into the acting creature's own move table rather than a move name,
/// so a choice can never name a move its creature does not have, and the wire
/// carries two bits instead of a move list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Choice {
  First,
  Second,
  Third,
  /// Take less this turn, and recover a little. Every creature has it.
  Guard,
}

impl Choice {
  pub const ALL: [Choice; 4] = [Choice::First, Choice::Second, Choice::Third, Choice::Guard];

  pub fn slot(self) -> usize {
    match self {
      Choice::First => 0,
      Choice::Second => 1,
      Choice::Third => 2,
      Choice::Guard => 3,
    }
  }
}

/// Which of the three ways a move can land against another.
///
/// Three, in a cycle, because a battle needs a reason to choose and not a
/// spreadsheet: thorn beats spore beats quill beats thorn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Element {
  Thorn,
  Spore,
  Quill,
}

impl Element {
  /// Damage multiplier as sixteenths, so effectiveness is integer arithmetic
  /// and two machines cannot round it differently.
  pub fn against(self, other: Element) -> u32 {
    let beats = matches!(
      (self, other),
      (Element::Thorn, Element::Spore) | (Element::Spore, Element::Quill) | (Element::Quill, Element::Thorn)
    );
    let loses = matches!(
      (self, other),
      (Element::Spore, Element::Thorn) | (Element::Quill, Element::Spore) | (Element::Thorn, Element::Quill)
    );
    match (beats, loses) {
      (true, _) => 24,
      (_, true) => 10,
      _ => 16,
    }
  }
}

/// What a move does beyond its damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
  None,
  /// Halves the target's speed for the rest of the battle, which changes who
  /// acts first from the next turn on.
  Slow,
  /// Heals the actor rather than striking.
  Recover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
  pub name: &'static str,
  pub element: Element,
  pub power: u8,
  /// Percent, checked against a hash of the turn rather than a roll.
  pub accuracy: u8,
  pub effect: Effect,
}

/// Damage a level adds, as sixteenths of the base. Level one is base.
const LEVEL_SIXTEENTHS: u32 = 3;

/// One creature: what it is, how far it has come, and the three numbers a
/// battle needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creature {
  pub kind: u8,
  pub level: u8,
  pub xp: u16,
  pub health: u8,
  pub power: u8,
  pub speed: u8,
}

impl Creature {
  /// The invented roster. Deliberately three, because a battle needs a reason
  /// to choose and not a collection to complete.
  pub fn of_kind(kind: u8) -> Self {
    Self::of_kind_at(kind, 1)
  }

  /// The same roster grown to a level, by scaling the base numbers.
  pub fn of_kind_at(kind: u8, level: u8) -> Self {
    let (health, power, speed) = match kind % 3 {
      0 => (24u32, 6u32, 5u32),
      1 => (18, 8, 7),
      _ => (30, 5, 3),
    };
    let level = level.max(1);
    let grown = |base: u32| -> u8 {
      let steps = (level - 1) as u32;
      (base + base * steps * LEVEL_SIXTEENTHS / 16).min(u8::MAX as u32) as u8
    };
    Self {
      kind: kind % 3,
      level,
      xp: 0,
      health: grown(health),
      power: grown(power),
      speed: grown(speed),
    }
  }

  pub fn name(kind: u8) -> &'static str {
    match kind % 3 {
      0 => "Bramblet",
      1 => "Quillick",
      _ => "Mossgab",
    }
  }

  /// Full health for this creature as it stands, which is what `Guard` heals
  /// toward and what a health bar is drawn against.
  pub fn full_health(&self) -> u8 {
    Self::of_kind_at(self.kind, self.level).health
  }

  /// The four a creature can choose between. Slot three is always `Guard`.
  pub fn moves(kind: u8) -> [Move; 4] {
    let guard = Move {
      name: "Guard",
      element: Element::Thorn,
      power: 0,
      accuracy: 100,
      effect: Effect::Recover,
    };
    match kind % 3 {
      0 => [
        Move {
          name: "Thornlash",
          element: Element::Thorn,
          power: 6,
          accuracy: 95,
          effect: Effect::None,
        },
        Move {
          name: "Bramblewrack",
          element: Element::Thorn,
          power: 11,
          accuracy: 60,
          effect: Effect::None,
        },
        Move {
          name: "Rootbind",
          element: Element::Spore,
          power: 3,
          accuracy: 90,
          effect: Effect::Slow,
        },
        guard,
      ],
      1 => [
        Move {
          name: "Quillshot",
          element: Element::Quill,
          power: 8,
          accuracy: 95,
          effect: Effect::None,
        },
        Move {
          name: "Bristlestorm",
          element: Element::Quill,
          power: 14,
          accuracy: 55,
          effect: Effect::None,
        },
        Move {
          name: "Dartstep",
          element: Element::Quill,
          power: 4,
          accuracy: 100,
          effect: Effect::Slow,
        },
        guard,
      ],
      _ => [
        Move {
          name: "Mosscuff",
          element: Element::Spore,
          power: 5,
          accuracy: 95,
          effect: Effect::None,
        },
        Move {
          name: "Sporecloud",
          element: Element::Spore,
          power: 10,
          accuracy: 65,
          effect: Effect::None,
        },
        Move {
          name: "Regrow",
          element: Element::Spore,
          power: 0,
          accuracy: 100,
          effect: Effect::Recover,
        },
        guard,
      ],
    }
  }

  /// What beating `beaten` is worth.
  pub fn xp_for_win(beaten: &Creature) -> u16 {
    (beaten.level as u16 + 1) * 6
  }

  /// XP to reach the next level from this one.
  pub fn xp_to_level(level: u8) -> u16 {
    (level as u16) * 20
  }

  /// Takes XP, growing a level at a time. `true` if it grew.
  ///
  /// Health is not restored by a level: a creature that levels mid-battle keeps
  /// the damage it has taken, or winning would heal you.
  pub fn absorb(&mut self, xp: u16) -> bool {
    self.xp = self.xp.saturating_add(xp);
    let mut grew = false;
    while self.level < u8::MAX && self.xp >= Self::xp_to_level(self.level) {
      self.xp -= Self::xp_to_level(self.level);
      let taken = self.full_health().saturating_sub(self.health);
      self.level += 1;
      let grown = Self::of_kind_at(self.kind, self.level);
      self.health = grown.health.saturating_sub(taken);
      self.power = grown.power;
      self.speed = grown.speed;
      grew = true;
    }
    grew
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
  /// Distinguishes two battles that would otherwise resolve identically, so
  /// the accuracy hash is not the same sequence in every battle in the town.
  pub seed: u32,
  /// What the last resolved turn did, for the client to read out. Presentation
  /// only: nothing in the rules consults it.
  pub log: Vec<Landed>,
}

/// One action of a resolved turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
  pub seat: u16,
  pub choice: Choice,
  pub missed: bool,
  pub damage: u8,
  /// Sixteenths, so `24` reads as effective and `10` as resisted.
  pub effectiveness: u8,
}

impl Battle {
  pub fn between(a: u16, b: u16, kinds: (u8, u8)) -> Self {
    Self::between_at(a, b, kinds, (1, 1), 0)
  }

  pub fn between_at(a: u16, b: u16, kinds: (u8, u8), levels: (u8, u8), seed: u32) -> Self {
    Self {
      turn: 1,
      sides: [
        Side {
          seat: a,
          creature: Creature::of_kind_at(kinds.0, levels.0),
          chosen: None,
        },
        Side {
          seat: b,
          creature: Creature::of_kind_at(kinds.1, levels.1),
          chosen: None,
        },
      ],
      winner: None,
      seed,
      log: Vec::new(),
    }
  }

  /// Whether a move lands, as a function of the transcript rather than a roll.
  ///
  /// Every input is already in the battle both ends hold, so a replay produces
  /// the same misses and there is no stream position to drift after a resend.
  fn lands(&self, side: usize, mv: &Move) -> bool {
    if mv.accuracy >= 100 {
      return true;
    }
    let mut seed = (self.seed as u64) << 32 | self.turn as u64;
    seed ^= (side as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    seed ^= (mv.power as u64) << 8;
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    seed = seed.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    seed ^= seed >> 32;
    (seed % 100) < mv.accuracy as u64
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
