//! The order rules, shared verbatim by the server and every client.
//!
//! This module is the example's claim made structural: whichever regime runs,
//! who acts next is a pure function of state both ends already hold, so the
//! wire never carries an upcoming-actors list and the client can be *audited*
//! against the server instead of trusted to guess. Everything here is integer
//! arithmetic for that reason; one float would put the two ends a rounding
//! mode apart.

use crate::protocol::{
  class_of, Class, Move, Regime, Unit, UnitId, BASE_SPEEDS, CHARGES, CTB_SCALE, GUARD_SHIELD, INITIATIVE_DIE, MAX_HP,
  SPEED_MAX, SPEED_MIN, SPEED_STEP, TEAM_SIZE, UNITS,
};

/// The shared mixer, so a battle's rolls replay identically on both ends and
/// in the tests.
pub fn rng(seed: u64) -> u64 {
  plaza_client_utils::determinism::mix64(seed)
}

/// A fresh 3v3, gauges seeded for the delay regime whichever regime runs.
pub fn fresh_units() -> Vec<Unit> {
  let mut units = Vec::with_capacity(UNITS);
  for team in 0..2u8 {
    for slot in 0..TEAM_SIZE {
      let speed = BASE_SPEEDS[slot];
      let id = (team as usize * TEAM_SIZE + slot) as UnitId;
      units.push(Unit {
        id,
        team,
        hp: MAX_HP,
        shield: 0,
        speed,
        charges: if class_of(id) == Class::Bruiser { 0 } else { CHARGES },
        alive: true,
        next_at: cost(speed, 100),
      });
    }
  }
  units
}

/// What a move of time-weight `time` costs a unit at `speed`, in gauge units.
pub fn cost(speed: u32, time: u64) -> u64 {
  CTB_SCALE * time / (100 * speed.max(1) as u64)
}

/// The delay regime's next actor: lowest gauge, ties to the lower id.
pub fn ctb_next(units: &[Unit]) -> Option<UnitId> {
  units
    .iter()
    .filter(|u| u.alive)
    .min_by_key(|u| (u.next_at, u.id))
    .map(|u| u.id)
}

/// Re-times the gauges after `actor` acted at `now` (its own gauge value),
/// paying `time` for the move it chose.
///
/// The actor's next wait is charged at whatever its speed is *after* the
/// action, so hasting yourself pays off on your own next turn. Everyone else
/// whose speed changed has the **remaining** wait rescaled by `old / new`,
/// which is what makes a haste land mid-flight instead of one turn late;
/// `before` is the speeds as they stood when the turn opened, indexed by unit
/// id.
pub fn ctb_recharge(units: &mut [Unit], actor: UnitId, now: u64, time: u64, before: &[u32]) {
  for unit in units.iter_mut() {
    if !unit.alive {
      continue;
    }
    if unit.id == actor {
      unit.next_at = now + cost(unit.speed, time);
    } else if before[unit.id as usize] != unit.speed {
      let remaining = unit.next_at.saturating_sub(now);
      unit.next_at = now + remaining * before[unit.id as usize] as u64 / unit.speed.max(1) as u64;
    }
  }
}

/// The delay regime's act list: simulate `n` picks forward, each paying a
/// standard action at its current speed. Speculative past the first entry
/// twice over, since nobody knows the moves to come; exact for the head.
pub fn ctb_project(units: &[Unit], n: usize) -> Vec<UnitId> {
  let mut sim: Vec<Unit> = units.to_vec();
  let mut out = Vec::with_capacity(n);
  for _ in 0..n {
    let Some(id) = ctb_next(&sim) else { break };
    out.push(id);
    let unit = sim.iter_mut().find(|u| u.id == id).expect("ctb_next names a live unit");
    unit.next_at += cost(unit.speed, 100);
  }
  out
}

/// One round's order under the initiative regime: speed plus a d20 rolled from
/// `(seed, round, unit)`, sorted highest first, ties to the lower id. Living
/// units only; the roll reads speed *now*, which is why the boundary is the
/// only honest moment to call this.
pub fn initiative_order(seed: u64, round: u32, units: &[Unit]) -> Vec<UnitId> {
  let mut rolled: Vec<(u32, UnitId)> = units
    .iter()
    .filter(|u| u.alive)
    .map(|u| {
      let die = (rng(seed ^ ((round as u64) << 8) ^ u.id as u64) % INITIATIVE_DIE) as u32 + 1;
      (u.speed + die, u.id)
    })
    .collect();
  rolled.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
  rolled.into_iter().map(|(_, id)| id).collect()
}

/// The projected act list under either regime, starting after `past` turns of
/// the standing `order` (initiative) or from the gauges as they stand (delay).
///
/// Initiative: the rest of the standing round, then the next round rolled from
/// speeds as they stand now, which is exactly as much as anyone can honestly
/// know: a haste landing before the boundary re-rolls that tail.
pub fn project(regime: Regime, seed: u64, round: u32, order: &[UnitId], past: usize, units: &[Unit], n: usize) -> Vec<UnitId> {
  match regime {
    Regime::Ctb => ctb_project(units, n),
    Regime::Initiative => {
      let alive = |id: &UnitId| units.iter().any(|u| u.id == *id && u.alive);
      let mut out: Vec<UnitId> = order.iter().skip(past).copied().filter(|id| alive(id)).collect();
      let mut next_round = round + 1;
      while out.len() < n && units.iter().any(|u| u.alive) {
        out.extend(initiative_order(seed, next_round, units));
        next_round += 1;
      }
      out.truncate(n);
      out
    }
  }
}

/// Applies `mv` at face value: no crit, plain numbers. What the server's
/// authoritative apply does with the crit already rolled, and what a what-if
/// preview does verbatim.
pub fn nominal_apply(units: &mut [Unit], actor: UnitId, mv: Move, damage: i32) {
  if mv.charged()
    && let Some(me) = units.iter_mut().find(|u| u.id == actor)
  {
    me.charges = me.charges.saturating_sub(1);
  }
  match mv {
    Move::Guard => {
      if let Some(me) = units.iter_mut().find(|u| u.id == actor) {
        me.shield = GUARD_SHIELD;
      }
    }
    _ => {
      let Some(target) = mv.target().and_then(|id| units.iter_mut().find(|u| u.id == id)) else {
        return;
      };
      if damage > 0 {
        let absorbed = target.shield.min(damage);
        target.shield -= absorbed;
        target.hp = (target.hp - (damage - absorbed)).max(0);
        target.alive = target.hp > 0;
      }
      if mv.heal() > 0 {
        target.hp = (target.hp + mv.heal()).min(MAX_HP);
      }
      if matches!(mv, Move::Haste { .. }) {
        target.speed = (target.speed + SPEED_STEP).min(SPEED_MAX);
      }
      if matches!(mv, Move::Slow { .. }) {
        target.speed = target.speed.saturating_sub(SPEED_STEP).max(SPEED_MIN);
      }
    }
  }
}

/// The what-if: the act list as it would stand after `actor` played `mv`, crit
/// unrolled because a preview promising a crit would be lying half the time.
pub fn preview(
  regime: Regime,
  seed: u64,
  round: u32,
  order: &[UnitId],
  past: usize,
  units: &[Unit],
  actor: UnitId,
  mv: Move,
  n: usize,
) -> Vec<UnitId> {
  let mut sim: Vec<Unit> = units.to_vec();
  let before: Vec<u32> = {
    let mut speeds = vec![0u32; UNITS];
    for unit in &sim {
      speeds[unit.id as usize] = unit.speed;
    }
    speeds
  };
  nominal_apply(&mut sim, actor, mv, mv.damage());
  if regime == Regime::Ctb
    && let Some(now) = sim.iter().find(|u| u.id == actor).map(|u| u.next_at)
  {
    ctb_recharge(&mut sim, actor, now, mv.time(), &before);
  }
  project(regime, seed, round, order, past, &sim, n)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_haste_rescales_the_remaining_wait_and_can_take_the_turn() {
    let mut units = fresh_units();
    // Unit 3, red's bruiser, is slow and far from acting. Doubling its speed
    // mid-flight halves what is left of its wait, not its next full cost.
    let waiting = units.iter().find(|u| u.id == 3).unwrap().next_at;
    let before: Vec<u32> = units.iter().map(|u| u.speed).collect();
    units.iter_mut().find(|u| u.id == 3).unwrap().speed *= 2;
    ctb_recharge(&mut units, 0, 0, 100, &before);
    let after = units.iter().find(|u| u.id == 3).unwrap().next_at;
    assert_eq!(after, waiting / 2, "half the speed's wait remains");
  }

  #[test]
  fn the_projection_head_is_the_next_actor() {
    let mut units = fresh_units();
    for _ in 0..20 {
      let next = ctb_next(&units).unwrap();
      assert_eq!(ctb_project(&units, 1), vec![next]);
      let unit = units.iter_mut().find(|u| u.id == next).unwrap();
      unit.next_at += cost(unit.speed, 100);
    }
  }

  #[test]
  fn a_heavy_move_costs_its_weight_in_time() {
    let mut smashed = fresh_units();
    let mut jabbed = smashed.clone();
    let before: Vec<u32> = smashed.iter().map(|u| u.speed).collect();
    let actor = ctb_next(&smashed).unwrap();
    let now = smashed.iter().find(|u| u.id == actor).unwrap().next_at;

    ctb_recharge(&mut smashed, actor, now, Move::Smash { target: 0 }.time(), &before);
    ctb_recharge(&mut jabbed, actor, now, Move::Jab { target: 0 }.time(), &before);

    let at = |units: &[Unit]| units.iter().find(|u| u.id == actor).unwrap().next_at;
    assert!(at(&smashed) > at(&jabbed), "the smash buys damage with its own future turns");
  }

  #[test]
  fn the_boundary_reads_speed_as_it_stands() {
    let units = fresh_units();
    let rolled = initiative_order(7, 3, &units);

    let mut hasted = units.clone();
    for unit in hasted.iter_mut().filter(|u| u.id == 3) {
      unit.speed += SPEED_STEP * 3;
    }
    let rerolled = initiative_order(7, 3, &hasted);
    assert_ne!(rolled, rerolled, "the same round's roll moves with the speeds it reads");
    let place = |order: &[UnitId]| order.iter().position(|id| *id == 3).unwrap();
    assert!(place(&rerolled) < place(&rolled), "the hasted unit climbs");
  }

  #[test]
  fn a_round_never_seats_the_dead() {
    let mut units = fresh_units();
    units.iter_mut().find(|u| u.id == 2).unwrap().alive = false;
    let order = initiative_order(1, 1, &units);
    assert_eq!(order.len(), 5);
    assert!(!order.contains(&2));
  }

  #[test]
  fn a_preview_removes_a_kill_and_moves_a_slow() {
    let mut units = fresh_units();
    // Wound red's medic to within a jab, then preview the jab: the queue it
    // promises has no unit 4 in it.
    units.iter_mut().find(|u| u.id == 4).unwrap().hp = 10;
    let killing = preview(Regime::Ctb, 1, 0, &[], 0, &units, 2, Move::Jab { target: 4 }, 8);
    assert!(!killing.contains(&4), "the preview buries the dead");

    // Two previews by the same actor, so the baselines match: against a jab
    // elsewhere, slowing red's trickster mid-wait pushes it later.
    units.iter_mut().find(|u| u.id == 5).unwrap().next_at += 1200;
    let jabbed = preview(Regime::Ctb, 1, 0, &[], 0, &units, 2, Move::Jab { target: 4 }, 8);
    let slowed = preview(Regime::Ctb, 1, 0, &[], 0, &units, 2, Move::Slow { target: 5 }, 8);
    let place = |order: &[UnitId]| order.iter().position(|id| *id == 5).unwrap_or(usize::MAX);
    assert!(place(&slowed) > place(&jabbed), "the slow shows up before it is committed");
  }

  #[test]
  fn a_shield_absorbs_before_hp() {
    let mut units = fresh_units();
    nominal_apply(&mut units, 0, Move::Guard, 0);
    let hp = units[0].hp;
    nominal_apply(&mut units, 3, Move::Jab { target: 0 }, 14);
    assert_eq!(units[0].hp, hp, "the guard ate the jab whole");
    assert_eq!(units[0].shield, GUARD_SHIELD - 14);
  }
}
