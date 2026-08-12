//! Three abilities, which is enough to be a game and few enough to stay one.
//!
//! Each is the genre's shape: a cost, a wait the design chose, a range checked
//! once on the server at the instant it lands, and an effect that names a
//! target rather than travelling toward one. Nothing here is predicted, and
//! nothing here has to be agreed between two machines.

use crate::casting::Ms;

/// What one press does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ability {
  pub name: &'static str,
  /// How long the bar runs before it goes off. Zero is instant.
  pub cast_ms: Ms,
  pub mana: u16,
  /// Taken off a hostile target.
  pub damage: u16,
  /// Put back on a friendly one, or on yourself.
  pub heal: u16,
  pub range: f32,
  /// Whether it wants an enemy or a friend.
  pub hostile: bool,
}

/// Strike: instant, cheap, short. What the global cooldown exists for.
pub const STRIKE: Ability = Ability {
  name: "Strike",
  cast_ms: 0,
  mana: 0,
  damage: 9,
  heal: 0,
  range: 5.0,
  hostile: true,
};

/// Bolt: the headline. A second and a half of bar, which is where the whole
/// latency argument lives.
pub const BOLT: Ability = Ability {
  name: "Bolt",
  cast_ms: 1500,
  mana: 22,
  damage: 26,
  heal: 0,
  range: 22.0,
  hostile: true,
};

/// Mend: the reason a party is worth having, and the only thing here that
/// reaches somebody who is not in front of you.
pub const MEND: Ability = Ability {
  name: "Mend",
  cast_ms: 2000,
  mana: 30,
  damage: 0,
  heal: 34,
  range: 24.0,
  hostile: false,
};

pub const BAR: [Ability; 3] = [STRIKE, BOLT, MEND];

pub fn ability(index: u8) -> Option<Ability> {
  BAR.get(index as usize).copied()
}

/// What a beast swings, which is on the same rules so nothing is special-cased.
pub const CLAW: Ability = Ability {
  name: "Claw",
  cast_ms: 0,
  mana: 0,
  damage: 7,
  heal: 0,
  range: 3.2,
  hostile: true,
};

#[cfg(test)]
mod tests {
  use super::*;
  use crate::casting::{press, GLOBAL_COOLDOWN_MS};

  #[test]
  fn every_ability_is_reachable_by_its_index() {
    for (i, listed) in BAR.iter().enumerate() {
      assert_eq!(ability(i as u8).as_ref(), Some(listed));
    }
    assert_eq!(ability(BAR.len() as u8), None, "an unknown index must not resolve");
  }

  #[test]
  fn the_bar_covers_both_ends_of_the_latency_argument() {
    // The point of shipping three rather than one: an instant press is all
    // delay and a cast bar is a tenth of it, and a player feels both in the
    // same session on the same connection.
    let rtt = 150;
    let instant = press(STRIKE.cast_ms, rtt).share();
    let cast = press(BOLT.cast_ms, rtt).share();
    assert!(instant > 0.99, "an instant ability is all delay: {instant}");
    assert!(cast < 0.11, "and a bar dilutes it: {cast}");
  }

  #[test]
  fn nothing_costs_more_mana_than_a_pool_holds() {
    for a in BAR {
      assert!(a.mana <= crate::zone::MAX_MANA, "{} costs {}", a.name, a.mana);
    }
  }

  #[test]
  fn an_instant_still_owes_the_cooldown() {
    // Otherwise Strike is a key you hold down, and the design has stopped
    // absorbing anything.
    const { assert!(STRIKE.cast_ms < GLOBAL_COOLDOWN_MS) };
  }

  #[test]
  fn a_healer_reaches_further_than_a_claw() {
    // The party exists because somebody out of the fight can still help.
    const { assert!(MEND.range > CLAW.range * 3.0) };
    const { assert!(!MEND.hostile && MEND.heal > 0) };
  }
}
