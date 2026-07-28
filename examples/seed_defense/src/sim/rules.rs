//! The wave, as one piece of code that every machine runs.
//!
//! The other playgrounds here share a rule so that a *correction* is small.
//! This one shares a rule because there are no corrections: the wire carries a
//! seed and a handful of build ops, and everything on screen after that is
//! produced locally. Two implementations of this file would not drift, they
//! would be two different games.
//!
//! Three things follow, and they are all visible in the code rather than
//! promised in a comment.
//!
//! **The step is a pure function of the field.** [`step`] takes the whole world
//! and advances it. The server calls it. Every client calls it. There is no
//! "server does the real one and the client approximates".
//!
//! **Every ordering is defined.** Towers fire in placement order, targets are
//! chosen by progress along the path with the id as the tie-break, and the
//! spawn schedule is a list built once from the seed. Nowhere does a rule
//! depend on the order a collection happens to iterate in, because two builds
//! are entitled to iterate differently.
//!
//! **The ways to break it are here too**, as [`Quirks`]. The panel can turn
//! each one on for one client, and each is a real change to the arithmetic
//! rather than a fault injected into a readout. A determinism claim that cannot
//! be falsified on demand is not a demonstration.

use crate::sim::fixed::{Fx, P};
use crate::sim::rand::Rand;
use crate::sim::types::*;

/// Deliberate departures from the shared rule, for one client.
///
/// Each is a mistake somebody has actually shipped: a constant worked out in
/// floating point, an iteration order taken from a hash map, and a timestamp
/// tidied to a round number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quirks {
  pub floats: bool,
  pub target_order: bool,
  pub slow_rounding: bool,
}

impl Quirks {
  pub const NONE: Quirks = Quirks {
    floats: false,
    target_order: false,
    slow_rounding: false,
  };

  pub fn any(&self) -> bool {
    self.floats || self.target_order || self.slow_rounding
  }
}

/// Everything a wave is made of. The unit both sides step and digest.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Field {
  pub tick: u64,
  pub wave: u32,
  pub enemies: Vec<Enemy>,
  pub towers: Vec<Tower>,
  pub gold: i32,
  pub lives: i32,
  pub next_enemy: EnemyId,
  /// The rest of this wave's spawns, as `(tick, kind)`, earliest last so the
  /// due ones pop off the end.
  pub pending: Vec<(u64, EnemyKind)>,
}

impl Default for Field {
  fn default() -> Self {
    Self {
      tick: 0,
      wave: 0,
      enemies: Vec::new(),
      towers: Vec::new(),
      gold: STARTING_GOLD,
      lives: STARTING_LIVES,
      next_enemy: 1,
      pending: Vec::new(),
    }
  }
}

impl Field {
  pub fn now_ms(&self) -> u64 {
    self.tick * SIM_STEP_MS
  }

  pub fn tower_at(&self, cell: Cell) -> Option<&Tower> {
    self.towers.iter().find(|t| t.cell == cell)
  }

  /// Whether the wave is over: nothing left to spawn and nothing left alive.
  pub fn wave_cleared(&self) -> bool {
    self.wave > 0 && self.pending.is_empty() && self.enemies.is_empty()
  }

  /// One number summarising the entire field.
  ///
  /// Built from `plaza_client_utils::SetDigest`, which exists for exactly this
  /// shape of problem: an order-independent fold, so two machines holding the
  /// same set in a different order still agree, and additive, so it is cheap.
  /// Everything that can differ goes in, including the gold and the tick, since
  /// a client that has fallen a tick behind is a client whose next comparison
  /// would be meaningless.
  pub fn digest(&self) -> u64 {
    let mut d = plaza_client_utils::SetDigest::new();
    for enemy in &self.enemies {
      d.insert(enemy.key());
    }
    for tower in &self.towers {
      d.insert(tower.key().wrapping_mul(0x9E37).wrapping_add(1));
    }
    d.insert((self.gold as u32 as u64).wrapping_mul(0xC2B2).wrapping_add(2));
    d.insert((self.lives as u32 as u64).wrapping_mul(0xC2B2).wrapping_add(3));
    d.insert((self.next_enemy as u64).wrapping_mul(0xC2B2).wrapping_add(4));
    d.insert((self.pending.len() as u64).wrapping_mul(0xC2B2).wrapping_add(5));
    d.digest()
  }
}

/// What happened during one step. Derived identically on both sides, so none of
/// it crosses the wire: it is here for the renderer and for the counters.
#[derive(Clone, Debug, Default)]
pub struct StepEvents {
  /// `(from, to)` for every shot fired, for drawing a beam.
  pub shots: Vec<(P, P)>,
  pub kills: Vec<EnemyId>,
  pub leaks: Vec<EnemyId>,
}

/// The spawn schedule for a wave, from the seed and the wave number alone.
///
/// This is the wire format. Everything the players will fight for the next
/// thirty seconds is in these two integers, and a client that computes this
/// list differently is a client playing another game.
pub fn wave_schedule(seed: u64, wave: u32, start_tick: u64) -> Vec<(u64, EnemyKind)> {
  let mut rand = Rand::new(seed.wrapping_add(wave as u64));
  let count = 8 + (wave as i32 * 3).min(34);
  let mut out = Vec::with_capacity(count as usize);
  let mut at = start_tick;
  for i in 0..count {
    // A tank every few enemies once the waves get going, otherwise a mix
    // weighted toward grunts.
    let kind = if wave >= 3 && i % 7 == 6 {
      EnemyKind::Tank
    } else {
      match rand.below(10) {
        0..=5 => EnemyKind::Grunt,
        6..=8 => EnemyKind::Runner,
        _ => EnemyKind::Tank,
      }
    };
    out.push((at, kind));
    let gap_ms = rand.range(280, 620) as u64;
    at += gap_ms / SIM_STEP_MS;
  }
  // Earliest last, so a due spawn pops off the end.
  out.reverse();
  out
}

/// Places or upgrades a tower, charging for it.
///
/// Shared for the same reason the movement is: the client charges what the
/// server charges. If a client thought a tower cost ten gold less, its next
/// affordability check would differ, and by the end of a wave the two would
/// hold different towers.
pub fn apply_build(field: &mut Field, build: Build) -> bool {
  if !in_bounds(build.cell) || on_path(build.cell) {
    return false;
  }
  let existing = field.towers.iter().position(|t| t.cell == build.cell);
  match (build.upgrade, existing) {
    (true, Some(index)) => {
      let tower = field.towers[index];
      if tower.level >= MAX_TOWER_LEVEL {
        return false;
      }
      let price = tower.kind.upgrade_cost(tower.level);
      if field.gold < price {
        return false;
      }
      field.gold -= price;
      field.towers[index].level += 1;
      true
    }
    (false, None) => {
      let price = build.kind.cost();
      if field.gold < price {
        return false;
      }
      field.gold -= price;
      field.towers.push(Tower {
        cell: build.cell,
        kind: build.kind,
        level: 0,
        owner: build.player,
        cooldown_ms: 0,
      });
      true
    }
    _ => false,
  }
}

/// Advances the field by exactly one tick.
pub fn step(field: &mut Field, quirks: Quirks) -> StepEvents {
  field.tick += 1;
  let now = field.now_ms();
  let mut events = StepEvents::default();

  spawn_due(field);
  advance_enemies(field, now, quirks, &mut events);
  fire_towers(field, now, quirks, &mut events);
  collect_dead(field, &mut events);

  events
}

fn spawn_due(field: &mut Field) {
  while field.pending.last().is_some_and(|(at, _)| *at <= field.tick) {
    let (_, kind) = field.pending.pop().expect("just checked");
    let id = field.next_enemy;
    field.next_enemy += 1;
    field.enemies.push(Enemy {
      id,
      kind,
      leg: 0,
      along: Fx::ZERO,
      hp: kind.hp(field.wave),
      slow_until_ms: 0,
    });
  }
}

fn advance_enemies(field: &mut Field, now: u64, quirks: Quirks, events: &mut StepEvents) {
  let mut leaked = Vec::new();
  for enemy in field.enemies.iter_mut() {
    let mut step = step_of(enemy.kind, quirks);
    if enemy.slowed(now) {
      step = Fx(step.0 * SLOW_NUM / SLOW_DEN);
    }

    enemy.along += step;

    while enemy.leg < legs() && enemy.along >= leg_len(enemy.leg) {
      enemy.along = enemy.along - leg_len(enemy.leg);
      enemy.leg += 1;
    }
    if enemy.leg >= legs() {
      leaked.push(enemy.id);
    }
  }

  if !leaked.is_empty() {
    field.enemies.retain(|e| !leaked.contains(&e.id));
    field.lives -= leaked.len() as i32;
    events.leaks.extend(leaked);
  }
}

fn fire_towers(field: &mut Field, now: u64, quirks: Quirks, events: &mut StepEvents) {
  // Placement order. It does not actually matter here, and that is worth
  // knowing rather than assuming: damage is additive and the dead are collected
  // after every tower has fired, so no tower can steal another's kill within a
  // tick. A version of this loop that removed the dead as it went would depend
  // on the order, and would need the same defence the targeting below has.
  for index in 0..field.towers.len() {
    let tower = field.towers[index];
    if tower.cooldown_ms > 0 {
      field.towers[index].cooldown_ms = (tower.cooldown_ms - SIM_STEP_MS as i32).max(0);
      continue;
    }
    let from = tower.cell.centre();
    let range_sq = {
      let r = tower.kind.range(tower.level);
      r.mul(r)
    };

    // The furthest along the path, with the id as an explicit tie-break: a
    // *rule*, evaluated over the set, with no reference to how the set is
    // stored. The quirk takes the first one in range instead, which is the
    // classic version of this bug: it is perfectly deterministic on one machine
    // and it silently encodes the container's iteration order into the game, so
    // the day somebody changes how enemies are held, every client disagrees.
    let in_range = field.enemies.iter().filter(|e| e.pos().dist_sq(from) <= range_sq);
    let target = if quirks.target_order {
      in_range.take(1).map(|e| (e.id, e.pos())).next()
    } else {
      in_range.max_by_key(|e| e.progress()).map(|e| (e.id, e.pos()))
    };

    let Some((target_id, at)) = target else {
      continue;
    };

    field.towers[index].cooldown_ms = tower.kind.cooldown_ms(tower.level);
    events.shots.push((from, at));

    let damage = tower.kind.damage(tower.level);
    let splash = tower.kind.splash();
    let splash_sq = splash.mul(splash);
    for enemy in field.enemies.iter_mut() {
      let hit = enemy.id == target_id || (splash.0 > 0 && enemy.pos().dist_sq(at) <= splash_sq);
      if !hit {
        continue;
      }
      enemy.hp -= damage;
      if tower.kind == TowerKind::Frost {
        let until = now + SLOW_MS;
        enemy.slow_until_ms = if quirks.slow_rounding {
          // The quirk that looks like housekeeping: a timestamp rounded to a
          // tenth of a second. It changes when the slow ends, which changes
          // where the enemy is, which changes what every tower targets after.
          (until + 50) / 100 * 100
        } else {
          until
        };
      }
    }
  }
}

/// An enemy's speed per tick, and the one place the float quirk lives.
///
/// Worth reading, because two earlier versions of this quirk did **not** work
/// and the reasons are the point of the module.
///
/// Making the *movement* use `f32` diverges from nothing: the result is
/// truncated back to 1/256 of a tile every tick, so the float error is
/// quantised away before it can accumulate. Re-quantising every tick is a large
/// part of why fixed point is robust, and it is why "we use floats but round
/// the positions" is not the same protection.
///
/// Making a *range* float diverges too rarely to demonstrate: it changes the
/// radius by 1/256 of a tile, so it only matters in the fraction of a tick an
/// enemy spends crossing that band while a tower happens to be off cooldown.
/// Real, and unobservable in a minute of play.
///
/// A **constant that is multiplied by time** is where a float bites hard. A
/// runner covers 4.2 tiles a second, which is `0.105` of a tile per 25 ms tick,
/// which is `26.88` in 256ths. The integer ratio floors that to 26. Working it
/// out in floating point and rounding gives 27. Four percent, applied every
/// tick, for ever: after ten seconds the two machines' runners are a tile and a
/// half apart, and each is being shot at by a different tower.
fn step_of(kind: EnemyKind, quirks: Quirks) -> Fx {
  if !quirks.floats {
    return kind.step();
  }
  let tiles_per_second: f32 = match kind {
    EnemyKind::Grunt => 2.4,
    EnemyKind::Runner => 4.2,
    EnemyKind::Tank => 1.5,
  };
  let per_tick = tiles_per_second * SIM_STEP_MS as f32 / 1000.0;
  Fx((per_tick * crate::sim::fixed::ONE as f32).round() as i32)
}

fn collect_dead(field: &mut Field, events: &mut StepEvents) {
  let mut gold = 0;
  field.enemies.retain(|enemy| {
    if enemy.hp > 0 {
      return true;
    }
    gold += enemy.kind.bounty();
    events.kills.push(enemy.id);
    false
  });
  field.gold += gold;
}

#[cfg(test)]
mod tests {
  use super::*;

  fn field_with_wave(wave: u32) -> Field {
    let mut field = Field {
      wave,
      ..Field::default()
    };
    field.pending = wave_schedule(0xD3F, wave, 0);
    field
  }

  #[test]
  fn the_same_seed_produces_the_same_wave_to_the_tick() {
    let a = wave_schedule(0xD3F, 4, 100);
    let b = wave_schedule(0xD3F, 4, 100);
    assert_eq!(a, b);
    assert!(a.len() >= 8);
  }

  #[test]
  fn a_different_wave_is_a_different_wave() {
    let a = wave_schedule(0xD3F, 4, 0);
    let b = wave_schedule(0xD3F, 5, 0);
    assert_ne!(a, b, "wave five is wave four shifted, which a seeded stream must not be");
  }

  #[test]
  fn two_fields_stepped_apart_stay_bit_identical() {
    // The claim the whole example rests on, at the smallest scale it can be
    // made: two independent copies, stepped by different callers, agreeing
    // exactly after thousands of ticks of accumulation.
    let mut a = field_with_wave(3);
    let mut b = field_with_wave(3);
    for cell in [Cell::new(4, 4), Cell::new(8, 6), Cell::new(13, 6)] {
      let build = Build {
        player: 0,
        cell,
        kind: TowerKind::Arrow,
        upgrade: false,
      };
      assert!(apply_build(&mut a, build));
      assert!(apply_build(&mut b, build));
    }
    for _ in 0..2000 {
      step(&mut a, Quirks::NONE);
      step(&mut b, Quirks::NONE);
      assert_eq!(a.digest(), b.digest(), "diverged at tick {}", a.tick);
    }
    assert_eq!(a, b);
    assert!(a.tick > 0);
  }

  #[test]
  fn a_tower_kills_things_and_pays_for_it() {
    let mut field = field_with_wave(1);
    let before = field.gold;
    assert!(apply_build(
      &mut field,
      Build {
        player: 0,
        cell: Cell::new(3, 3),
        kind: TowerKind::Arrow,
        upgrade: false,
      }
    ));
    assert_eq!(field.gold, before - TowerKind::Arrow.cost(), "the tower was paid for");

    let mut kills = 0;
    for _ in 0..4000 {
      kills += step(&mut field, Quirks::NONE).kills.len();
    }
    assert!(kills > 0, "a tower on the path's shoulder should kill something");
    assert!(field.gold > before - TowerKind::Arrow.cost(), "and the bounties were paid");
  }

  #[test]
  fn an_enemy_that_walks_the_whole_path_costs_a_life() {
    let mut field = field_with_wave(1);
    let lives = field.lives;
    for _ in 0..6000 {
      step(&mut field, Quirks::NONE);
    }
    assert!(field.lives < lives, "with no towers at all, the wave gets through");
  }

  #[test]
  fn a_build_is_refused_on_the_path_and_when_it_cannot_be_paid_for() {
    let mut field = Field::default();
    let on_the_path = Build {
      player: 0,
      cell: Cell::new(3, 2),
      kind: TowerKind::Arrow,
      upgrade: false,
    };
    assert!(!apply_build(&mut field, on_the_path), "the corridor is not buildable");

    field.gold = 5;
    let too_dear = Build {
      player: 0,
      cell: Cell::new(3, 5),
      kind: TowerKind::Cannon,
      upgrade: false,
    };
    assert!(!apply_build(&mut field, too_dear));
    assert_eq!(field.gold, 5, "and a refusal charges nothing");
  }

  #[test]
  fn each_quirk_actually_diverges() {
    // The three toggles are the demonstration, so each one has to be shown to
    // change the world rather than to change a label. Any that stopped
    // diverging would leave the panel claiming a detection that never fires.
    for quirk in [
      Quirks {
        floats: true,
        ..Quirks::NONE
      },
      Quirks {
        target_order: true,
        ..Quirks::NONE
      },
      Quirks {
        slow_rounding: true,
        ..Quirks::NONE
      },
    ] {
      let mut honest = field_with_wave(4);
      let mut quirked = field_with_wave(4);
      for (i, cell) in [Cell::new(4, 4), Cell::new(7, 6), Cell::new(9, 6), Cell::new(13, 6)].into_iter().enumerate() {
        let build = Build {
          player: 0,
          cell,
          // A frost tower in the mix, so the slow-rounding quirk has something
          // to round.
          kind: if i == 1 { TowerKind::Frost } else { TowerKind::Arrow },
          upgrade: false,
        };
        apply_build(&mut honest, build);
        apply_build(&mut quirked, build);
      }
      assert_eq!(honest.digest(), quirked.digest(), "they start identical");

      let mut diverged_at = None;
      for _ in 0..4000 {
        step(&mut honest, Quirks::NONE);
        step(&mut quirked, quirk);
        if honest.digest() != quirked.digest() {
          diverged_at = Some(honest.tick);
          break;
        }
      }
      assert!(diverged_at.is_some(), "{quirk:?} never actually changed anything");
    }
  }
}
