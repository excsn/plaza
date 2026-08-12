//! A zone of characters, and what one client is told about them.
//!
//! The point of assembling it is that the four things this example measures
//! separately have to hold together: a character the client owns and the server
//! only checks, an audience built from two channels rather than one, and a cast
//! that is a wait the design already asked for.

use std::collections::HashMap;

use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid};

use crate::casting::{Ms, GLOBAL_COOLDOWN_MS};
use crate::controls::Authority;
use crate::movement::{Tracked, Verdict};
use crate::relevance::{audience, Audience, Parties, Seat};

/// How far a character is told about, in metres.
pub const VIEW: f32 = 30.0;
/// How far an ability reaches.
pub const REACH: f32 = 22.0;
/// What one landing takes off.
pub const HIT: u16 = 12;
/// Grid cell width, a third of the view, which is how the other examples in
/// this tree size theirs.
pub const CELL: f32 = VIEW / 3.0;
/// Metres between floors, which is what makes a zone a building rather than a
/// field.
pub const FLOOR_HEIGHT: f32 = 5.0;

/// What a character is doing, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cast {
  pub ability: u8,
  /// When it will land, on the server's clock.
  pub lands_at: Ms,
}

#[derive(Clone, Copy, Debug)]
pub struct Character {
  pub seat: Seat,
  /// Where the **client** says it is, which the server only sanity-checks.
  pub tracked: Tracked,
  pub health: u16,
  /// What the client says it is holding, used only under server authority.
  pub intent: (f32, i8),
  /// Who this character's next ability is aimed at.
  pub target: Option<Seat>,
  pub casting: Option<Cast>,
  /// When this seat may act again, which is the other half of the design
  /// absorbing latency.
  pub ready_at: Ms,
  pub alive: bool,
}

impl Character {
  pub fn new(seat: Seat, at: (f32, f32, f32), now_ms: Ms) -> Self {
    Self {
      seat,
      tracked: Tracked::new(at, now_ms),
      health: 100,
      intent: (0.0, 0),
      target: None,
      casting: None,
      ready_at: 0,
      alive: true,
    }
  }
}

pub struct Zone {
  pub characters: HashMap<Seat, Character>,
  pub parties: Parties,
  /// The library's flat `(x, z)` grid, rebuilt each tick because everyone
  /// moves.
  ///
  /// Two-dimensional, which is the right default and the wrong one here, and
  /// `tests/tower.rs` is the measurement of how wrong: in a stacked building a
  /// flat cell holds every floor at once, so the height filter below throws
  /// away 72% of what the grid hands back. Kept rather than hidden, because
  /// the whole point of this example is that the trade is visible.
  grid: SpatialGrid<u32>,
  /// Whether the grid still describes where everyone is.
  ///
  /// A rebuilt-on-tick index is one a caller can read before the first tick,
  /// or after a spawn, and get an empty answer that is indistinguishable from
  /// "nobody is nearby". Marking it instead means a stale index cannot be
  /// queried at all: the read rebuilds it.
  stale: bool,
  /// Candidates the grid returned, and how many survived the height test, for
  /// the panel.
  pub examined: u64,
  pub returned: u64,
  pub authority: Authority,
  pub now_ms: Ms,
  /// Claims the validator refused, which is the only signal there is that
  /// somebody is not playing the same game.
  pub refusals: u64,
  /// Casts that landed, for the panel.
  pub landed: u64,
  /// Characters brought back up, for the panel.
  pub revives: u64,
}

impl Default for Zone {
  fn default() -> Self {
    Self {
      characters: HashMap::new(),
      parties: Parties::default(),
      grid: SpatialGrid::new(GridQuantizer::new((0.0, 0.0), CELL)),
      stale: true,
      examined: 0,
      returned: 0,
      authority: Authority::default(),
      now_ms: 0,
      refusals: 0,
      landed: 0,
      revives: 0,
    }
  }
}

impl Zone {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn admit(&mut self, seat: Seat, at: (f32, f32, f32)) {
    self.characters.insert(seat, Character::new(seat, at, self.now_ms));
    self.stale = true;
  }

  pub fn remove(&mut self, seat: Seat) {
    self.characters.remove(&seat);
    self.stale = true;
    // Leaving the zone leaves the party, or a health bar keeps updating for
    // somebody who is not here.
    self.parties.leave(seat);
  }

  /// Puts a character somewhere, and tells the index about it.
  ///
  /// The only way to move somebody that is not a claim or a driven step.
  /// Writing `tracked.at` directly leaves the spatial index describing where
  /// they used to be, and a query then answers from the old world with no sign
  /// that anything is wrong, which a test doing exactly that is how this
  /// method came to exist.
  pub fn place(&mut self, seat: Seat, at: (f32, f32, f32)) {
    if let Some(character) = self.characters.get_mut(&seat) {
      character.tracked.at = at;
      self.stale = true;
    }
  }

  /// Aims at a seat, or at nobody.
  ///
  /// Checked when the cast lands rather than now, because a target that walks
  /// away mid-cast is the ordinary case and refusing it here would only move
  /// the same decision earlier.
  pub fn aim(&mut self, seat: Seat, at: Option<Seat>) {
    if at.is_some_and(|other| other == seat || !self.characters.contains_key(&other)) {
      return;
    }
    if let Some(character) = self.characters.get_mut(&seat) {
      character.target = at;
    }
  }

  /// Records what a client is holding, for the server to integrate.
  pub fn intend(&mut self, seat: Seat, yaw: f32, forward: i8) {
    if let Some(character) = self.characters.get_mut(&seat) {
      character.intent = (yaw, forward.clamp(-1, 1));
    }
  }

  /// Moves everyone from their held input, which is what the server does when
  /// it is the one deciding.
  ///
  /// The same speed constant the validator polices under the other mode, so
  /// the two arms of the comparison are not quietly running different games.
  fn drive(&mut self, dt_ms: Ms) {
    let step = crate::movement::RUN_SPEED * (dt_ms as f32 / 1000.0);
    for character in self.characters.values_mut() {
      let (yaw, forward) = character.intent;
      if forward == 0 {
        continue;
      }
      let travel = step * forward as f32;
      let at = character.tracked.at;
      character.tracked.at = (at.0 + yaw.sin() * travel, at.1, at.2 + yaw.cos() * travel);
      self.stale = true;
    }
  }

  /// Takes a claimed position, or does not.
  ///
  /// Refused outright under server authority: a position from a client that
  /// does not own one is not a claim to check, it is a packet from a client
  /// that has not noticed the mode changed.
  pub fn claim(&mut self, seat: Seat, to: (f32, f32, f32)) -> Verdict {
    if self.authority == Authority::Server {
      return Verdict::Refused;
    }
    let now = self.now_ms;
    let Some(character) = self.characters.get_mut(&seat) else {
      return Verdict::Refused;
    };
    let verdict = character.tracked.claim(to, now);
    if verdict == Verdict::Refused {
      self.refusals += 1;
    } else {
      self.stale = true;
    }
    verdict
  }

  /// Begins a cast, if this seat is allowed to act.
  ///
  /// Refused while already casting or inside the cooldown, which is not a rate
  /// limit bolted on: it is the wait the whole latency argument rests on.
  pub fn begin_cast(&mut self, seat: Seat, ability: u8, cast_ms: Ms) -> bool {
    let now = self.now_ms;
    let Some(character) = self.characters.get_mut(&seat) else {
      return false;
    };
    if character.casting.is_some() || now < character.ready_at {
      return false;
    }
    character.casting = Some(Cast {
      ability,
      lands_at: now + cast_ms,
    });
    character.ready_at = now + cast_ms.max(GLOBAL_COOLDOWN_MS);
    true
  }

  /// Advances the clock and lands whatever was due.
  ///
  /// Returns the seats whose cast landed, because that is an **event**: it
  /// happens once and no later frame mentions it, which is the same shape
  /// poketo's kills have and the same reason it has to be delivered rather
  /// than described.
  pub fn advance(&mut self, dt_ms: Ms) -> Vec<Seat> {
    self.now_ms += dt_ms;
    if self.authority == Authority::Server {
      self.drive(dt_ms);
    }
    let now = self.now_ms;
    let mut landed = Vec::new();
    for character in self.characters.values_mut() {
      let Some(cast) = character.casting else {
        continue;
      };
      if now >= cast.lands_at {
        character.casting = None;
        landed.push(character.seat);
      }
    }
    self.landed += landed.len() as u64;

    // Resolved after the loop, because a landing reads one character and
    // writes another, and the two can be the same seat's target chain.
    for caster in &landed {
      let Some((from, target)) = self
        .characters
        .get(caster)
        .map(|c| (c.tracked.at, c.target))
      else {
        continue;
      };
      let Some(target) = target else { continue };
      let Some(victim) = self.characters.get_mut(&target) else {
        continue;
      };
      // One range check, on the server, at the instant it lands. That is the
      // whole of hit detection here, and the reason nothing has to be agreed.
      if crate::movement::distance(from, victim.tracked.at) > REACH {
        continue;
      }
      victim.health = victim.health.saturating_sub(HIT);
      if victim.health == 0 {
        // Back to full where they stand, because a corpse is content this
        // example does not have and a zone that empties measures nothing.
        victim.health = 100;
        self.revives += 1;
      }
    }
    landed
  }

  /// Rebuilds the spatial index. Called once a tick, before anyone queries it.
  ///
  /// Rebuilt rather than updated, which is what every example in this tree
  /// does and for the same reason: in a zone where everyone moves, tracking
  /// which cell each character left costs more than filling an index whose
  /// buckets already have their capacity.
  fn reindex(&mut self) {
    self.stale = false;
    self.grid.clear();
    for character in self.characters.values().filter(|c| c.alive) {
      self
        .grid
        .insert(character.seat as u32, character.tracked.at.0, character.tracked.at.2);
    }
  }

  /// Who is within the view radius of a seat, itself included.
  ///
  /// The grid answers in two axes and the height test finishes the job. It is
  /// exact either way: a flat query returns the **column**, a superset of the
  /// sphere, so nobody is ever missed and the filter only removes false
  /// positives.
  pub fn near(&mut self, seat: Seat, out: &mut Vec<Seat>) {
    out.clear();
    if self.stale {
      self.reindex();
    }
    let Some(from) = self.characters.get(&seat).filter(|c| c.alive).map(|c| c.tracked.at) else {
      return;
    };
    let mut candidates = Vec::new();
    self.grid.query_radius(from.0, from.2, VIEW, &mut candidates);
    self.examined += candidates.len() as u64;
    for id in candidates {
      let other = id as Seat;
      let Some(character) = self.characters.get(&other) else {
        continue;
      };
      if crate::movement::distance(from, character.tracked.at) <= VIEW {
        out.push(other);
      }
    }
    self.returned += out.len() as u64;
    out.sort_unstable();
  }

  /// Everything a seat is told about this tick: near, plus subscribed.
  pub fn audience_for(&mut self, seat: Seat, scratch: &mut Vec<Seat>) -> Audience {
    self.near(seat, scratch);
    audience(scratch, &self.parties, seat)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn zone() -> Zone {
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit(2, (5.0, 0.0, 0.0));
    // Three floors up and across the zone: near nobody.
    zone.admit(3, (200.0, FLOOR_HEIGHT * 3.0, 200.0));
    zone
  }

  #[test]
  fn an_audience_is_the_two_channels_unioned() {
    let mut zone = zone();
    let mut scratch = Vec::new();

    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.seats, vec![1, 2], "distance alone");

    zone.parties.join(1, 3);
    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.seats, vec![1, 2, 3], "and a party member across the zone");
    assert_eq!(a.subscribed, 1);
  }

  #[test]
  fn leaving_the_zone_leaves_the_party() {
    // Or a health bar keeps updating for somebody who is not here, which is a
    // subscription outliving the thing it is about.
    let mut zone = zone();
    zone.parties.join(1, 3);
    zone.remove(3);
    let mut scratch = Vec::new();
    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.subscribed, 0);
    assert_eq!(a.seats, vec![1, 2]);
  }

  #[test]
  fn a_cast_cannot_be_started_twice() {
    let mut zone = zone();
    assert!(zone.begin_cast(1, 0, 1500), "the first one goes");
    assert!(!zone.begin_cast(1, 0, 1500), "not while already casting");

    let landed = zone.advance(1500);
    assert_eq!(landed, vec![1], "and it lands once");
  }

  #[test]
  fn the_cooldown_runs_during_a_cast_rather_than_after_it() {
    // Which is why a caster is not waiting three seconds between long
    // abilities: the two waits overlap, and the longer of them is the whole of
    // it. A short cast is the case where the cooldown is still running when
    // the ability has already gone off.
    let mut long = zone();
    long.begin_cast(1, 0, GLOBAL_COOLDOWN_MS);
    long.advance(GLOBAL_COOLDOWN_MS);
    assert!(long.begin_cast(1, 0, 0), "a full-length cast is ready the moment it lands");

    let mut short = zone();
    short.begin_cast(1, 0, 400);
    short.advance(400);
    assert!(!short.begin_cast(1, 0, 400), "a short one still owes the cooldown");
    short.advance(GLOBAL_COOLDOWN_MS - 400);
    assert!(short.begin_cast(1, 0, 400));
  }

  #[test]
  fn an_instant_ability_still_waits_for_the_cooldown() {
    // Which is why an instant is not an exception to the design absorbing
    // latency: the next input was never going to be frame-tight.
    let mut zone = zone();
    assert!(zone.begin_cast(1, 1, 0));
    zone.advance(1);
    assert!(!zone.begin_cast(1, 1, 0), "the cooldown is the wait now");
    zone.advance(GLOBAL_COOLDOWN_MS);
    assert!(zone.begin_cast(1, 1, 0));
  }

  #[test]
  fn a_landing_is_reported_once_and_never_again() {
    // An event rather than a state, the same shape poketo's kills have: a
    // client that misses it is not told again by anything.
    let mut zone = zone();
    zone.begin_cast(1, 0, 500);
    assert!(zone.advance(400).is_empty(), "not yet");
    assert_eq!(zone.advance(200), vec![1], "now");
    assert!(zone.advance(1000).is_empty(), "and not a second time");
  }

  #[test]
  fn a_named_target_is_checked_when_the_cast_lands_not_when_it_starts() {
    // The whole of hit detection in this genre, and the reason nothing has to
    // be agreed: no projectile exists, so there is no moving thing for two
    // machines to disagree about. One range check, on the server, at one
    // instant.
    let mut zone = zone();
    zone.aim(1, Some(2));
    zone.begin_cast(1, 0, 500);

    // The target walks out of reach while the bar is running, which is the
    // ordinary case rather than an edge one.
    zone.advance(250);
    zone.place(2, (REACH * 3.0, 0.0, 0.0));
    zone.advance(250);
    assert_eq!(zone.characters[&2].health, 100, "out of reach when it landed");

    // And back in reach, where the same cast connects. Waiting out the
    // cooldown first, and asserting the cast actually began: a refused one
    // leaves the health unchanged for a reason that has nothing to do with
    // range, which would pass this test for the wrong reason.
    zone.place(2, (2.0, 0.0, 0.0));
    zone.advance(GLOBAL_COOLDOWN_MS);
    assert!(zone.begin_cast(1, 0, 0), "the second cast is off cooldown");
    zone.advance(1);
    assert_eq!(zone.characters[&2].health, 100 - HIT);
  }

  #[test]
  fn aiming_at_yourself_or_at_nobody_is_refused_rather_than_stored() {
    // A target that is not there is a target every landing has to re-check,
    // and a target that is yourself is a rule nobody wants to discover later.
    let mut zone = zone();
    zone.aim(1, Some(1));
    assert_eq!(zone.characters[&1].target, None);
    zone.aim(1, Some(77));
    assert_eq!(zone.characters[&1].target, None);
    zone.aim(1, Some(2));
    assert_eq!(zone.characters[&1].target, Some(2));
    zone.aim(1, None);
    assert_eq!(zone.characters[&1].target, None, "and dropping a target is allowed");
  }

  #[test]
  fn a_character_brought_to_nothing_comes_back_up_where_it_stands() {
    // A zone that empties measures nothing, and a corpse is content this
    // example does not have.
    let mut zone = zone();
    zone.aim(1, Some(2));
    let mut casts = 0;
    while zone.revives == 0 {
      zone.begin_cast(1, 0, 0);
      zone.advance(GLOBAL_COOLDOWN_MS);
      casts += 1;
      assert!(casts < 20, "a hundred health at {HIT} a landing should not take this long");
    }
    assert_eq!(zone.characters[&2].health, 100);
    assert_eq!(zone.revives, 1);
  }

  #[test]
  fn a_refused_claim_is_counted_because_it_is_the_only_signal() {
    let mut zone = zone();
    assert_eq!(zone.claim(1, (1.0, 0.0, 0.0)), Verdict::Refused, "no time has passed");
    assert_eq!(zone.refusals, 1);

    zone.advance(1000);
    assert_eq!(zone.claim(1, (1.0, 0.0, 0.0)), Verdict::Accepted, "a second is plenty");
    assert_eq!(zone.refusals, 1);
  }
}
