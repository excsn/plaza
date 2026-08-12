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
//! **A choice names a slot, not a move.** Which move a slot means is a rule
//! both ends run, so a creature's four moves never cross the wire and a choice
//! cannot name a move its creature does not have.
//!
//! **A level is a field and everything it implies is a function.** Power and
//! speed are derived from `(kind, level)`, so a creature that grew stronger
//! costs one byte more than a creature that cannot grow at all, rather than
//! three.
//!
//! The creatures are invented, as are their names and everything they do.

use serde::{Deserialize, Serialize};

/// What a side can do with a turn.
///
/// An index into the acting creature's own move table. Keeping it an enum
/// rather than a `u8` makes an out-of-range move unrepresentable on the wire,
/// and leaves [`Battle::offer`] exactly the shape it had.
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

/// Which of the three ways a move can land against a creature.
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
  /// Damage multiplier in sixteenths.
  ///
  /// Integer, like every other comparison in this example: the argument the
  /// whole crate makes is that a discrete world compares with `==`, and a float
  /// in the damage path invites the question of whether two machines agree.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
  None,
  /// Halves the target's speed for the rest of the battle, which changes who
  /// acts first from the next turn on.
  Slow,
  /// Heals the actor instead of striking.
  Recover,
}

/// One of a creature's four. Never crosses the wire: a slot does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
  pub name: &'static str,
  pub element: Element,
  pub power: u8,
  /// Percent. Checked against a hash of the turn rather than a roll.
  pub accuracy: u8,
  pub effect: Effect,
}

/// What a level adds to a base number, in sixteenths per level.
const PER_LEVEL: u32 = 3;

/// One creature: what it is, how far it has come, and what it has left.
///
/// `health` is a field because it is accumulated history, and `level` because
/// it comes from a record the client does not hold. Everything else is a
/// function of the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creature {
  pub kind: u8,
  pub level: u8,
  pub xp: u16,
  pub health: u8,
}

impl Creature {
  /// The invented roster. Deliberately three, because a battle needs a reason
  /// to choose and not a collection to complete.
  /// Health, power, speed at level one.
  ///
  /// Health is deliberately several times what a move takes off. At the first
  /// numbers tried, a level-one creature with a type disadvantage went down in
  /// **two hits**, so a battle was over before either player had read what
  /// happened in it, and the moves might as well not have existed. A fight has
  /// to last long enough for a choice made in it to matter.
  fn base(kind: u8) -> (u32, u32, u32) {
    match kind % 3 {
      0 => (64, 6, 5),
      1 => (48, 8, 7),
      _ => (80, 5, 3),
    }
  }

  fn grown(base: u32, level: u8) -> u8 {
    (base + base * (level.max(1) - 1) as u32 * PER_LEVEL / 16).min(u8::MAX as u32) as u8
  }

  pub fn of_kind(kind: u8) -> Self {
    Self::of_kind_at(kind, 1)
  }

  pub fn of_kind_at(kind: u8, level: u8) -> Self {
    let level = level.max(1);
    Self {
      kind: kind % 3,
      level,
      xp: 0,
      health: Self::grown(Self::base(kind).0, level),
    }
  }

  pub fn name(kind: u8) -> &'static str {
    match kind % 3 {
      0 => "Bramblet",
      1 => "Quillick",
      _ => "Mossgab",
    }
  }

  pub fn element(kind: u8) -> Element {
    match kind % 3 {
      0 => Element::Thorn,
      1 => Element::Quill,
      _ => Element::Spore,
    }
  }

  /// Health at full, which is what `Guard` heals toward and what a health bar
  /// is drawn against.
  pub fn full_health(&self) -> u8 {
    Self::grown(Self::base(self.kind).0, self.level)
  }

  pub fn power(&self) -> u8 {
    Self::grown(Self::base(self.kind).1, self.level)
  }

  pub fn speed(&self) -> u8 {
    Self::grown(Self::base(self.kind).2, self.level)
  }

  /// The four a creature can choose between. Slot three is always `Guard`.
  pub fn moves(kind: u8) -> [Move; 4] {
    let guard = Move {
      name: "Guard",
      element: Self::element(kind),
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
          power: 12,
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
          power: 7,
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
          power: 11,
          accuracy: 65,
          effect: Effect::None,
        },
        Move {
          name: "Regrow",
          element: Element::Spore,
          power: 4,
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

  /// XP needed to leave this level.
  pub fn xp_to_level(level: u8) -> u16 {
    (level as u16) * 20
  }

  /// Takes XP, growing a level at a time. `true` if it grew.
  ///
  /// Damage already taken is carried across the level rather than healed, or
  /// winning a battle would restore you mid-fight.
  pub fn absorb(&mut self, xp: u16) -> bool {
    self.xp = self.xp.saturating_add(xp);
    let mut grew = false;
    while self.level < u8::MAX && self.xp >= Self::xp_to_level(self.level) {
      self.xp -= Self::xp_to_level(self.level);
      let taken = self.full_health().saturating_sub(self.health);
      self.level += 1;
      self.health = self.full_health().saturating_sub(taken).max(1);
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
  /// Hit by a `Slow`, which halves its speed for the rest of the battle.
  pub hobbled: bool,
}

impl Side {
  /// Speed as it stands, which is the only thing that decides turn order.
  pub fn speed(&self) -> u8 {
    let speed = self.creature.speed();
    if self.hobbled {
      speed / 2
    } else {
      speed
    }
  }
}

/// One action of a resolved turn, for the client to read out.
///
/// Presentation only: nothing in the rules consults it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
  pub seat: u16,
  pub choice: Choice,
  pub missed: bool,
  pub damage: u8,
  /// Sixteenths, so `24` reads as effective and `10` as resisted.
  pub effectiveness: u8,
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
  /// every battle in the town does not miss on the same turns.
  pub seed: u32,
  /// What the last resolved turn did. Replaced each turn, not appended to.
  pub log: Vec<Landed>,
}

impl Battle {
  pub fn between(a: u16, b: u16, kinds: (u8, u8)) -> Self {
    Self::between_at(
      Creature::of_kind(kinds.0),
      Creature::of_kind(kinds.1),
      (a, b),
      0,
    )
  }

  pub fn between_at(mine: Creature, theirs: Creature, seats: (u16, u16), seed: u32) -> Self {
    Self {
      turn: 1,
      sides: [
        Side {
          seat: seats.0,
          creature: mine,
          chosen: None,
          hobbled: false,
        },
        Side {
          seat: seats.1,
          creature: theirs,
          chosen: None,
          hobbled: false,
        },
      ],
      winner: None,
      seed,
      log: Vec::new(),
    }
  }

  fn index_of(&self, seat: u16) -> Option<usize> {
    self.sides.iter().position(|s| s.seat == seat)
  }

  pub fn finished(&self) -> bool {
    self.winner.is_some()
  }

  /// The move a side's choice names.
  pub fn move_of(&self, side: usize, choice: Choice) -> Move {
    Creature::moves(self.sides[side].creature.kind)[choice.slot()]
  }

  fn mix(mut seed: u64) -> u64 {
    seed ^= seed >> 33;
    seed = seed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    seed ^= seed >> 29;
    seed = seed.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    seed ^= seed >> 32;
    seed
  }

  /// Whether a move lands, as a function of the transcript rather than a roll.
  ///
  /// **Both sides' choices are mixed in**, and that is the load-bearing part: a
  /// roll a client could compute from what it already holds would let it pick
  /// whichever move is going to hit this turn, so an inaccurate move would
  /// carry no risk at all. Neither side can know the other's choice until both
  /// have committed, which is exactly when this is asked.
  ///
  /// Nothing here may read the server's clock. If it did, the same choice
  /// replayed at a different wall time would resolve differently, and a resend
  /// being harmless would weaken from a rule to a coincidence with every
  /// reconnection test still green.
  fn lands(&self, actor: usize, accuracy: u8) -> bool {
    if accuracy >= 100 {
      return true;
    }
    let chosen = |n: usize| self.sides[n].chosen.map_or(4u64, |c| c.slot() as u64);
    let mut seed = (self.seed as u64) << 32 | self.turn as u64;
    seed ^= (chosen(0) * 5 + chosen(1)) << 8;
    seed ^= (actor as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (Self::mix(seed) % 100) < accuracy as u64
  }

  /// What the wild side answers, which is a rule rather than the server's
  /// private mind: a client can name what it did without a byte being spent.
  pub fn wild_choice(&self, side: usize) -> Choice {
    let seed = Self::mix((self.seed as u64) << 32 | (self.turn as u64) << 4 | side as u64);
    // Never `Guard` twice running against a player who is striking, and never
    // the risky move on the turn that would end it.
    let creature = &self.sides[side].creature;
    let hurt = creature.health as u32 * 3 <= creature.full_health() as u32;
    match seed % 8 {
      0 => Choice::Guard,
      1 if hurt => Choice::Guard,
      1..=2 => Choice::Third,
      3..=4 => Choice::Second,
      _ => Choice::First,
    }
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
    self.log.clear();

    // Both speeds read before anything is applied, so a `Slow` landing this
    // turn cannot reorder the turn it landed on.
    let (first, second) = if self.sides[0].speed() >= self.sides[1].speed() {
      (0, 1)
    } else {
      (1, 0)
    };

    for (actor, target) in [(first, second), (second, first)] {
      if self.finished() {
        break;
      }
      let choice = self.sides[actor].chosen.unwrap_or(Choice::Guard);
      let mv = self.move_of(actor, choice);
      let mut entry = Landed {
        seat: self.sides[actor].seat,
        choice,
        missed: !self.lands(actor, mv.accuracy),
        damage: 0,
        effectiveness: 16,
      };

      if !entry.missed {
        match mv.effect {
          Effect::Recover => {
            let full = self.sides[actor].creature.full_health();
            let heal = full / 8 + mv.power;
            let health = &mut self.sides[actor].creature.health;
            *health = health.saturating_add(heal).min(full);
          }
          Effect::Slow => self.sides[target].hobbled = true,
          Effect::None => {}
        }

        if mv.power > 0 && !matches!(mv.effect, Effect::Recover) {
          let effectiveness = mv.element.against(Creature::element(self.sides[target].creature.kind));
          let base = mv.power as u32 + self.sides[actor].creature.power() as u32 / 3;
          let mut damage = base * effectiveness / 16;
          if self.sides[target].chosen == Some(Choice::Guard) {
            damage /= 2;
          }
          let damage = damage.clamp(1, u8::MAX as u32) as u8;
          entry.damage = damage;
          entry.effectiveness = effectiveness as u8;

          let hit = &mut self.sides[target].creature;
          hit.health = hit.health.saturating_sub(damage);
          if hit.health == 0 {
            self.winner = Some(self.sides[actor].seat);
          }
        }
      }

      self.log.push(entry);
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
    assert_eq!(b.offer(7, 1, Choice::First), Offered::Waiting);
    assert_eq!(b.turn, 1, "nothing happens until both have answered");
    assert_eq!(b.offer(9, 1, Choice::First), Offered::Resolved);
    assert_eq!(b.turn, 2);
  }

  #[test]
  fn a_resent_choice_names_a_turn_that_has_gone_and_does_nothing() {
    // The bug this prevents: a client whose connection dropped resends its
    // choice, and the move happens twice. Nothing about that looks wrong from
    // either end, which is why the turn number is on the wire rather than a
    // sequence number the server has to remember.
    let mut b = battle();
    b.offer(7, 1, Choice::First);
    b.offer(9, 1, Choice::First);
    let after = b.clone();

    assert_eq!(b.offer(7, 1, Choice::First), Offered::Stale { turn: 2 });
    assert_eq!(b, after, "a stale choice must change nothing at all");
  }

  #[test]
  fn a_choice_for_a_later_turn_is_refused_rather_than_queued() {
    // Queuing it would mean a client could pre-commit to a turn it has not
    // seen, and the server would be holding intent it cannot show anybody.
    let mut b = battle();
    assert_eq!(b.offer(7, 5, Choice::First), Offered::Ahead { turn: 1 });
    assert_eq!(b.sides[0].chosen, None);
  }

  #[test]
  fn changing_your_mind_before_the_other_side_answers_is_not_a_second_move() {
    let mut b = battle();
    b.offer(7, 1, Choice::First);
    assert_eq!(b.offer(7, 1, Choice::Guard), Offered::Waiting);
    assert_eq!(b.sides[0].chosen, Some(Choice::Guard), "the later choice stands");
    assert_eq!(b.turn, 1, "and the turn has still not resolved");
  }

  #[test]
  fn the_faster_side_acts_first_and_that_can_decide_it() {
    // Order is the only thing speed does, and it has to be the same order
    // everywhere: the slower side may not survive to act.
    let mut b = Battle::between_at(Creature::of_kind_at(1, 20), Creature::of_kind_at(2, 1), (1, 2), 0);
    b.sides[1].creature.health = 3;
    assert!(b.sides[0].speed() > b.sides[1].speed(), "the level 20 one is faster");

    b.offer(1, 1, Choice::First);
    b.offer(2, 1, Choice::First);
    assert_eq!(b.winner, Some(1), "the fast one landed first");
    assert_eq!(
      b.sides[0].creature.health,
      b.sides[0].creature.full_health(),
      "and took nothing back"
    );
  }

  #[test]
  fn guarding_halves_what_lands_and_recovers_a_little() {
    // Read off the log rather than off health, because a guard also heals: the
    // net change can be zero while the strike very much landed.
    let struck = |theirs: Choice| {
      let mut b = battle();
      let before = b.sides[1].creature.health;
      b.offer(7, 1, Choice::First);
      b.offer(9, 1, theirs);
      let dealt = b.log.iter().find(|l| l.seat == 7).map(|l| l.damage).unwrap_or(0);
      (dealt, before, b.sides[1].creature.health)
    };

    let (guarded, before, after_guard) = struck(Choice::Guard);
    let (open, _, _) = struck(Choice::First);
    assert!(guarded > 0 && open > 0, "both should land: {guarded} against {open}");
    assert!(guarded < open, "guarding should halve it: {guarded} against {open}");
    assert!(after_guard >= before - guarded, "and recover a little on top");
  }

  #[test]
  fn a_seat_that_is_not_in_the_battle_is_refused() {
    let mut b = battle();
    assert_eq!(b.offer(99, 1, Choice::First), Offered::NotYours);
  }

  #[test]
  fn nothing_is_accepted_once_it_is_over() {
    let mut b = Battle::between(1, 2, (1, 1));
    b.sides[1].creature.health = 1;
    b.offer(1, 1, Choice::First);
    b.offer(2, 1, Choice::First);
    assert!(b.finished());
    assert_eq!(b.offer(1, b.turn, Choice::First), Offered::Finished);
  }

  #[test]
  fn a_move_that_misses_is_a_property_of_the_turn_rather_than_a_roll() {
    // Two battles from the same seed, played the same way, must miss on the
    // same turns. A roll with a position in a stream would drift the moment
    // anything was resent.
    let play = |seed: u32| {
      let mut b = Battle::between_at(Creature::of_kind_at(0, 30), Creature::of_kind_at(2, 30), (1, 2), seed);
      let mut misses = Vec::new();
      for turn in 1..=6 {
        if b.finished() {
          break;
        }
        b.offer(1, turn, Choice::Second);
        b.offer(2, turn, Choice::First);
        misses.extend(b.log.iter().map(|l| l.missed));
      }
      misses
    };
    assert_eq!(play(4242), play(4242), "the same battle resolves the same way");
    assert_ne!(play(4242), play(99), "while a different battle does not");

    // Over many battles the risky move misses at about the rate it claims.
    // One battle could be lucky; a rate cannot.
    let (mut rolls, mut missed) = (0, 0);
    for seed in 0..200 {
      for landed in play(seed) {
        rolls += 1;
        missed += usize::from(landed);
      }
    }
    let rate = missed as f32 / rolls as f32;
    assert!(rolls > 100, "enough rolls to say anything: {rolls}");
    assert!((0.05..0.45).contains(&rate), "an inaccurate move should miss sometimes: {rate}");
  }

  #[test]
  fn a_roll_cannot_be_known_before_both_sides_have_committed() {
    // Otherwise a client computes which move is going to hit and the risky move
    // carries no risk. Changing only the *opponent's* choice has to change what
    // lands.
    let differs = Choice::ALL.iter().any(|theirs| {
      let outcome = |theirs: Choice| {
        let mut b = Battle::between_at(Creature::of_kind_at(1, 40), Creature::of_kind_at(0, 40), (1, 2), 7);
        b.offer(1, 1, Choice::Second);
        b.offer(2, 1, theirs);
        b.log.iter().find(|l| l.seat == 1).map(|l| l.missed)
      };
      outcome(*theirs) != outcome(Choice::First)
    });
    assert!(differs, "the opponent's choice has to be part of the roll");
  }

  #[test]
  fn a_status_landing_this_turn_does_not_reorder_this_turn() {
    // Both speeds are read before anything is applied. Otherwise a Slow that
    // lands first would move its own target to second, which is an ordering
    // that depends on which machine evaluated it first.
    let mut b = Battle::between_at(Creature::of_kind_at(1, 1), Creature::of_kind_at(0, 1), (1, 2), 0);
    assert!(b.sides[0].speed() > b.sides[1].speed());

    // Side 0 slows side 1, which was already slower: the log order must still
    // be side 0 then side 1.
    b.offer(1, 1, Choice::Third);
    b.offer(2, 1, Choice::First);
    assert_eq!(b.log[0].seat, 1, "the faster side acted first: {:?}", b.log);
    assert!(b.sides[1].hobbled, "and the slow landed");
  }

  #[test]
  fn a_level_scales_a_creature_without_the_wire_carrying_its_numbers() {
    // Power and speed are functions of (kind, level), so the wire carries a
    // level rather than three statistics that could disagree with it.
    let low = Creature::of_kind_at(0, 1);
    let high = Creature::of_kind_at(0, 20);
    assert!(high.power() > low.power(), "{} against {}", high.power(), low.power());
    assert!(high.speed() > low.speed());
    assert!(high.full_health() > low.full_health());
    assert_eq!(high.kind, low.kind);
  }

  #[test]
  fn experience_grows_a_level_without_healing_the_damage_it_arrived_with() {
    // Or winning a turn would restore you mid-battle, which is a heal nobody
    // chose and no move paid for.
    let mut c = Creature::of_kind_at(0, 1);
    let full = c.full_health();
    c.health = full / 2;
    let taken = full - c.health;

    assert!(c.absorb(Creature::xp_to_level(1)), "enough to grow");
    assert_eq!(c.level, 2);
    assert_eq!(c.health, c.full_health() - taken, "it carried the damage across");
  }

  #[test]
  fn a_type_it_is_strong_against_takes_more_than_one_it_is_weak_against() {
    assert!(Element::Thorn.against(Element::Spore) > Element::Thorn.against(Element::Thorn));
    assert!(Element::Thorn.against(Element::Thorn) > Element::Thorn.against(Element::Quill));
    for e in [Element::Thorn, Element::Spore, Element::Quill] {
      assert_eq!(e.against(e), 16, "a type is neutral against itself");
    }
  }

  #[test]
  fn every_creature_has_four_moves_and_the_last_is_always_guard() {
    for kind in 0..3u8 {
      let moves = Creature::moves(kind);
      assert_eq!(moves[Choice::Guard.slot()].name, "Guard");
      assert!(moves.iter().all(|m| m.accuracy > 0 && m.accuracy <= 100));
      assert!(moves[1].power > moves[0].power, "the risky one should hit harder");
      assert!(moves[1].accuracy < moves[0].accuracy, "and land less often");
    }
  }
}
