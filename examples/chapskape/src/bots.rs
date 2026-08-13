//! The world's own, living the loop so a joining player arrives somewhere
//! inhabited.
//!
//! Not decoration. gow_3d proved it by shipping without them: an empty world
//! cannot tell you the difference between a quiet afternoon and a frame that
//! never said anything, and every key looked broken because nothing was there
//! to answer one.
//!
//! They walk the same circle a player does. Chop until the pack is heavy, set
//! light to the logs, catch fish, cook them on the fire, go and fight something,
//! eat when it hurts, start again. Everything they do goes through the same ops
//! a client sends, so nothing downstream knows the difference and nothing here
//! is a special case the netcode would not otherwise have.

use std::collections::HashMap;

use crate::protocol::{Item, Look, Queued, Seat, Tile};
use crate::world::{self, Prop};
use crate::zone::{XorShift, Zone};

/// Where a body is in the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
  Wood,
  Fish,
  Cook,
  Brawl,
}

/// How far one will walk to find work.
const REACH: i16 = 26;

/// Logs before a woodcutter has had enough.
const ENOUGH_LOGS: usize = 6;
/// Fish before a fisher goes looking for a fire.
const ENOUGH_FISH: usize = 4;

#[derive(Default)]
pub struct Bots {
  jobs: HashMap<Seat, Job>,
  rng: XorShift,
}

impl Default for XorShift {
  fn default() -> Self {
    XorShift::new(0xC0FF_EE00_1234_5678)
  }
}

impl Bots {
  pub fn take_seat(&mut self, seat: Seat, index: usize) {
    let job = match index % 4 {
      0 => Job::Wood,
      1 => Job::Fish,
      2 => Job::Brawl,
      _ => Job::Wood,
    };
    self.jobs.insert(seat, job);
  }

  pub fn len(&self) -> usize {
    self.jobs.len()
  }

  pub fn is_empty(&self) -> bool {
    self.jobs.is_empty()
  }

  pub fn seats(&self) -> impl Iterator<Item = Seat> + '_ {
    self.jobs.keys().copied()
  }

  pub fn job_of(&self, seat: Seat) -> Option<Job> {
    self.jobs.get(&seat).copied()
  }

  /// Gives anybody who has finished something the next thing to do.
  ///
  /// Only the idle are looked at, which is what keeps a world of these cheap: a
  /// body walking to a tree costs nothing until it arrives.
  pub fn steer(&mut self, zone: &mut Zone) {
    let mut seats: Vec<Seat> = self.jobs.keys().copied().collect();
    seats.sort_unstable();
    for seat in seats {
      let Some(actor) = zone.actors.get(&seat) else {
        self.jobs.remove(&seat);
        continue;
      };
      if !actor.alive() || actor.queued.is_some() || !actor.route.is_empty() {
        continue;
      }
      self.decide(seat, zone);
    }
  }

  fn decide(&mut self, seat: Seat, zone: &mut Zone) {
    let Some(actor) = zone.actors.get(&seat) else {
      return;
    };
    let here = actor.tile;
    let hurt = actor.health * 2 < actor.max_health;
    let logs = actor.pack.count_of(Item::Logs);
    let raw = actor.pack.count_of(Item::RawFish);
    let cooked = actor.pack.count_of(Item::CookedFish);
    let full = actor.pack.is_full();
    let job = self.jobs.get(&seat).copied().unwrap_or(Job::Wood);

    // Eating comes before everything, so a body that is losing a fight does the
    // one thing that would save it rather than the next thing on its list.
    if hurt && cooked > 0 {
      if let Some(slot) = zone.actors[&seat].pack.find(Item::CookedFish) {
        zone.use_slot(seat, slot as u8);
        return;
      }
    }

    match job {
      Job::Wood => {
        if full || logs >= ENOUGH_LOGS {
          self.jobs.insert(seat, Job::Fish);
          return self.spill(seat, zone);
        }
        if !self.work_nearest(seat, here, zone, &[Prop::Tree, Prop::Oak, Prop::Rock]) {
          self.wander(seat, here, zone);
        }
      }
      Job::Fish => {
        if full || raw >= ENOUGH_FISH {
          self.jobs.insert(seat, Job::Cook);
          return;
        }
        if !self.work_nearest(seat, here, zone, &[Prop::Fish]) {
          self.jobs.insert(seat, Job::Wood);
          self.wander(seat, here, zone);
        }
      }
      Job::Cook => {
        if raw == 0 {
          self.jobs.insert(seat, Job::Brawl);
          return;
        }
        let fire = zone
          .fires
          .iter()
          .filter(|(_, (tile, _))| here.steps_to(*tile) <= REACH as i32)
          .min_by_key(|(id, (tile, _))| (here.steps_to(*tile), **id))
          .map(|(id, _)| *id);
        match fire {
          Some(fire) => zone.queue(seat, Queued::Cook { fire }),
          None => match zone.actors[&seat].pack.find(Item::Logs) {
            Some(slot) => zone.use_slot(seat, slot as u8),
            None => {
              self.jobs.insert(seat, Job::Wood);
            }
          },
        }
      }
      Job::Brawl => {
        if hurt {
          self.jobs.insert(seat, Job::Wood);
          return;
        }
        let quarry = zone
          .actors
          .iter()
          .filter(|(_, other)| other.look.is_foe() && other.alive())
          .filter(|(_, other)| here.steps_to(other.tile) <= REACH as i32)
          .min_by_key(|(other, actor)| (here.steps_to(actor.tile), **other))
          .map(|(other, _)| *other);
        match quarry {
          Some(quarry) => zone.queue(seat, Queued::Fight { seat: quarry }),
          None => {
            self.jobs.insert(seat, Job::Wood);
            self.wander(seat, here, zone);
          }
        }
      }
    }
  }

  /// Leaves what it was carrying on the ground.
  ///
  /// Which is also what keeps the ownership timer exercised without a player
  /// having to think of it: there is always something lying about that belongs
  /// to somebody for another half minute.
  fn spill(&mut self, seat: Seat, zone: &mut Zone) {
    let Some(actor) = zone.actors.get(&seat) else {
      return;
    };
    let ore = actor.pack.find(Item::Ore);
    let bones = actor.pack.find(Item::Bones);
    if let Some(slot) = ore.or(bones) {
      zone.drop_slot(seat, slot as u8);
    }
  }

  /// The nearest prop of any of these kinds that is standing, worked at.
  fn work_nearest(&mut self, seat: Seat, here: Tile, zone: &mut Zone, want: &[Prop]) -> bool {
    let mut best: Option<(i32, u32, Prop)> = None;
    for dy in -REACH..=REACH {
      for dx in -REACH..=REACH {
        let tile = Tile::new(here.x + dx, here.y + dy);
        let Some(prop) = world::prop_at(tile) else {
          continue;
        };
        if !want.contains(&prop) {
          continue;
        }
        let id = world::prop_id(tile);
        if !zone.prop_ready(id) {
          continue;
        }
        let (skill, level) = prop.needs();
        if zone.actors.get(&seat).is_none_or(|a| a.level(skill) < level) {
          continue;
        }
        let distance = here.steps_to(tile);
        // Distance then id, so two equally close trees do not swap between
        // ticks and leave a body walking back and forth between them.
        if best.is_none_or(|(d, existing, _)| (distance, id) < (d, existing)) {
          best = Some((distance, id, prop));
        }
      }
    }
    let Some((_, id, prop)) = best else {
      return false;
    };
    zone.queue(seat, match prop {
      Prop::Tree | Prop::Oak => Queued::Chop { object: id },
      Prop::Rock | Prop::Vein => Queued::Mine { object: id },
      Prop::Fish => Queued::Fish { object: id },
    });
    true
  }

  /// Somewhere else to stand, for a body with nothing to do.
  fn wander(&mut self, seat: Seat, here: Tile, zone: &mut Zone) {
    for _ in 0..6 {
      let dx = self.rng.below(2 * REACH as u32 + 1) as i16 - REACH;
      let dy = self.rng.below(2 * REACH as u32 + 1) as i16 - REACH;
      let tile = Tile::new(here.x + dx, here.y + dy);
      if world::walkable(tile) {
        zone.walk_to(seat, tile);
        return;
      }
    }
  }
}

/// Seats the hens and brutes that make the countryside worth crossing.
pub fn stock(zone: &mut Zone, seats: impl Iterator<Item = Seat>, hens: usize) {
  for (index, seat) in seats.enumerate() {
    let angle = index as f32 * 2.399_963_2;
    let radius = 10.0 + (index as f32).sqrt() * 6.0;
    let hint = Tile::new(
      (world::SIZE / 2) as i16 + (angle.cos() * radius) as i16,
      (world::SIZE / 2) as i16 + (angle.sin() * radius) as i16,
    );
    let look = if index < hens { Look::Hen } else { Look::Brute };
    zone.admit(seat, world::footing_near(hint), look);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::skills::Skill;

  fn a_world(bots: usize) -> (Zone, Bots) {
    let mut zone = Zone::new();
    let mut crew = Bots::default();
    for index in 0..bots {
      let seat = index as Seat;
      let hint = Tile::new(
        world::SIZE / 2 + (index as i16 % 9) * 2,
        world::SIZE / 2 + (index as i16 / 9) * 2,
      );
      zone.admit(seat, world::footing_near(hint), Look::Person);
      crew.take_seat(seat, index);
    }
    (zone, crew)
  }

  #[test]
  fn an_idle_body_is_given_something_to_do() {
    let (mut zone, mut crew) = a_world(4);
    crew.steer(&mut zone);
    let busy = zone
      .actors
      .values()
      .filter(|a| a.queued.is_some() || !a.route.is_empty())
      .count();
    assert!(busy >= 3, "only {busy} of four found anything to do");
  }

  #[test]
  fn the_world_gets_on_with_it() {
    // The measurement that says the place is inhabited rather than populated.
    // Nothing here is asserted about who did what, only that the loop turns.
    let (mut zone, mut crew) = a_world(16);
    let foes: Vec<Seat> = (100..116).collect();
    stock(&mut zone, foes.into_iter(), 10);

    for _ in 0..400 {
      crew.steer(&mut zone);
      zone.advance();
    }

    let xp: u32 = zone
      .actors
      .values()
      .filter(|a| a.is_person())
      .map(|a| a.xp.iter().sum::<u32>())
      .sum();
    let levels: usize = zone
      .actors
      .values()
      .filter(|a| a.is_person())
      .map(|a| a.level(Skill::Woodcutting) as usize - 1)
      .sum();
    println!(
      "\n  after 400 ticks: {} gathered, {} blows, {} felled, {} experience, {} levels, {} props used up, {} on the ground\n",
      zone.gathered,
      zone.blows,
      zone.falls,
      xp,
      levels,
      zone.depletions,
      zone.ground.len()
    );
    assert!(zone.gathered > 20, "only {} things were gathered", zone.gathered);
    assert!(zone.depletions > 5, "nothing was ever used up");
    assert!(xp > 200, "the world earned {xp} experience, which is nothing");
  }

  #[test]
  fn a_hungry_body_eats_before_it_does_anything_else() {
    let (mut zone, mut crew) = a_world(1);
    {
      let actor = zone.actors.get_mut(&0).unwrap();
      actor.health = 4;
      actor.pack.add(Item::CookedFish);
    }
    crew.steer(&mut zone);
    assert!(zone.actors[&0].health > 4, "it went to work instead of eating");
  }

  #[test]
  fn the_loop_moves_a_body_along_it() {
    // Wood, then fish, then cook: a body that stayed on one job for ever would
    // populate the world without inhabiting it.
    let (mut zone, mut crew) = a_world(1);
    for _ in 0..12 {
      zone.actors.get_mut(&0).unwrap().pack.add(Item::Logs);
    }
    crew.steer(&mut zone);
    assert_eq!(crew.job_of(0), Some(Job::Fish), "a full woodcutter kept chopping");
  }
}
