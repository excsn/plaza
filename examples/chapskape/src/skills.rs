//! Five skills, one closed loop, and the arithmetic that turns work into a
//! level.
//!
//! Not on the wire. A frame carries five experience totals and the client works
//! out what level that is with this, which is the same bargain the map makes:
//! anything both ends can derive is something nobody has to send. It is also
//! why retuning the curve does not move the protocol version.
//!
//! The loop is deliberately closed rather than five separate numbers going up.
//! Chop a tree for logs, set light to them, catch a fish, cook it on the fire,
//! eat it to heal, go and get hit. A world where every activity ends in a
//! counter is a world with nothing to walk between.

/// What can be trained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Skill {
  Woodcutting,
  Mining,
  Fishing,
  Cooking,
  Combat,
}

pub const SKILLS: usize = 5;

pub const ALL: [Skill; SKILLS] = [
  Skill::Woodcutting,
  Skill::Mining,
  Skill::Fishing,
  Skill::Cooking,
  Skill::Combat,
];

impl Skill {
  pub fn index(self) -> usize {
    match self {
      Skill::Woodcutting => 0,
      Skill::Mining => 1,
      Skill::Fishing => 2,
      Skill::Cooking => 3,
      Skill::Combat => 4,
    }
  }

  pub fn from_index(index: usize) -> Option<Skill> {
    ALL.get(index).copied()
  }

  pub fn name(self) -> &'static str {
    match self {
      Skill::Woodcutting => "woodcutting",
      Skill::Mining => "mining",
      Skill::Fishing => "fishing",
      Skill::Cooking => "cooking",
      Skill::Combat => "combat",
    }
  }

  pub fn short(self) -> &'static str {
    match self {
      Skill::Woodcutting => "WC",
      Skill::Mining => "MIN",
      Skill::Fishing => "FSH",
      Skill::Cooking => "CK",
      Skill::Combat => "CMB",
    }
  }
}

/// The highest level there is.
pub const TOP: u8 = 40;

/// Experience needed to reach a level.
///
/// Quadratic and shallow at the bottom, so the first few arrive within a
/// minute of picking up an axe. A curve that makes a player wait ten minutes
/// for the first one is a curve that has decided the demonstration is about
/// patience.
pub fn xp_for(level: u8) -> u32 {
  let steps = level.saturating_sub(1) as u32;
  8 * steps * steps + 12 * steps
}

/// What a total of experience is worth.
pub fn level_for(xp: u32) -> u8 {
  let mut level = 1;
  while level < TOP && xp >= xp_for(level + 1) {
    level += 1;
  }
  level
}

/// How far through the current level a total is, `0.0..1.0`.
pub fn progress(xp: u32) -> f32 {
  let level = level_for(xp);
  if level >= TOP {
    return 1.0;
  }
  let floor = xp_for(level);
  let ceiling = xp_for(level + 1);
  ((xp - floor) as f32 / (ceiling - floor).max(1) as f32).clamp(0.0, 1.0)
}

/// Experience for a point of damage dealt.
pub const XP_PER_DAMAGE: u32 = 4;

/// Experience for cooking one fish.
pub const XP_COOKING: u32 = 22;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_level_and_its_threshold_agree() {
    for level in 1..=TOP {
      assert_eq!(level_for(xp_for(level)), level, "level {level}");
      if level > 1 {
        assert_eq!(level_for(xp_for(level) - 1), level - 1);
      }
    }
  }

  #[test]
  fn the_first_levels_arrive_while_somebody_is_still_watching() {
    // A demonstration whose first reward is ten minutes away has decided it is
    // about patience. Twelve experience is one tree.
    assert!(xp_for(2) <= 24, "the second level costs {} , which is too many trees", xp_for(2));
    assert!(xp_for(5) <= 200);
    println!("\n  trees to each level, at 12 experience a tree:\n");
    for level in [2u8, 3, 5, 10, 20, TOP] {
      println!("    {level:>3}  {:>5} xp  {:>4} trees", xp_for(level), xp_for(level).div_ceil(12));
    }
    println!();
  }

  #[test]
  fn progress_runs_from_one_level_to_the_next() {
    assert_eq!(progress(xp_for(3)), 0.0);
    let midway = (xp_for(3) + xp_for(4)) / 2;
    assert!((progress(midway) - 0.5).abs() < 0.05, "{}", progress(midway));
    assert_eq!(progress(xp_for(TOP)), 1.0);
    assert_eq!(progress(u32::MAX), 1.0);
  }

  #[test]
  fn every_skill_has_an_index_that_round_trips() {
    // The wire carries an index rather than a name, so a mismatch here is a
    // frame that credits the wrong skill and nothing that says so.
    for (index, skill) in ALL.iter().enumerate() {
      assert_eq!(skill.index(), index);
      assert_eq!(Skill::from_index(index), Some(*skill));
    }
    assert_eq!(Skill::from_index(SKILLS), None);
  }
}
