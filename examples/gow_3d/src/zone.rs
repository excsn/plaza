//! A zone of characters on generated ground, and what one client is told about
//! it.
//!
//! Everything the server owns lives here: where people are, what they are
//! casting, what is hunting them, and who is close enough to be told. The
//! ground itself is not owned, because it is a rule both ends derive from
//! [`terrain`](crate::terrain) rather than a payload anyone sends.

use std::collections::HashMap;

use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid};

use crate::abilities::{ability, Ability, CLAW};
use crate::casting::{Ms, GLOBAL_COOLDOWN_MS};
use crate::controls::Authority;
use crate::movement::{distance, Tracked, Verdict, MAX_AIR};
use crate::protocol::Kind;
use crate::relevance::{audience, Audience, Parties, Seat};
use crate::terrain;

/// How far a character is told about, in metres.
pub const VIEW: f32 = 46.0;
/// Grid cell width, a third of the view, which is how the other examples in
/// this tree size theirs.
pub const CELL: f32 = VIEW / 3.0;
/// How long a character stays down before coming back up.
pub const DOWN_MS: Ms = 6000;
/// How long a body stays in everyone's frame after it falls.
///
/// Long enough for a client to play the fall, short enough that the two
/// relevance channels still come apart: past this the body is gone from the
/// people standing next to it and still in the party frame of anyone
/// subscribed, which is the whole distinction this example is about.
pub const CORPSE_MS: Ms = 1600;

pub const MAX_HEALTH: u16 = 100;
pub const MAX_MANA: u16 = 100;
/// Mana returned per second while not casting.
pub const MANA_REGEN: f32 = 7.0;

pub const BEAST_HEALTH: u16 = 60;
/// How close a beast lets somebody get before it takes an interest.
pub const AGGRO: f32 = 16.0;
/// How far from home it will chase before giving up and walking back.
pub const LEASH: f32 = 38.0;
pub const BEAST_SPEED: f32 = 4.6;
/// How often a beast may swing.
pub const CLAW_EVERY_MS: Ms = 1800;

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
  pub kind: Kind,
  /// Where the **client** says it is, which the server only sanity-checks.
  /// For anything the zone drives, it is where the zone put it.
  pub tracked: Tracked,
  pub health: u16,
  pub max_health: u16,
  pub mana: f32,
  /// Which way this character is facing, for drawing a body rather than a box.
  pub yaw: f32,
  /// What the client says it is holding, used only under server authority.
  pub intent: (f32, i8),
  /// Who this character's next ability is aimed at.
  pub target: Option<Seat>,
  pub casting: Option<Cast>,
  /// When this seat may act again, which is the other half of the design
  /// absorbing latency.
  pub ready_at: Ms,
  /// Up, and therefore visible, targetable and able to act.
  pub alive: bool,
  /// When a downed character comes back up.
  pub up_at: Ms,
  /// Where a beast belongs, and returns to.
  pub home: (f32, f32, f32),
  /// When a beast may swing again.
  pub swing_at: Ms,
}

impl Character {
  /// When this character fell. A body is downed for `DOWN_MS`, so the moment
  /// it dropped is the moment it comes back up less that.
  pub fn downed_at(&self) -> Ms {
    self.up_at.saturating_sub(DOWN_MS)
  }

  /// Whether a downed body is still worth sending to whoever is nearby.
  pub fn still_falling(&self, now_ms: Ms) -> bool {
    !self.alive && now_ms.saturating_sub(self.downed_at()) < CORPSE_MS
  }

  pub fn adventurer(seat: Seat, at: (f32, f32, f32), now_ms: Ms) -> Self {
    Self {
      seat,
      kind: Kind::Adventurer,
      tracked: Tracked::new(at, now_ms),
      health: MAX_HEALTH,
      max_health: MAX_HEALTH,
      mana: MAX_MANA as f32,
      yaw: 0.0,
      intent: (0.0, 0),
      target: None,
      casting: None,
      ready_at: 0,
      alive: true,
      up_at: 0,
      home: at,
      swing_at: 0,
    }
  }

  pub fn beast(seat: Seat, at: (f32, f32, f32), now_ms: Ms) -> Self {
    let mut beast = Self::adventurer(seat, at, now_ms);
    beast.kind = Kind::Beast;
    beast.health = BEAST_HEALTH;
    beast.max_health = BEAST_HEALTH;
    beast.mana = 0.0;
    beast
  }

  pub fn is_beast(&self) -> bool {
    self.kind == Kind::Beast
  }

  /// Whether these two may attack each other.
  pub fn hostile_to(&self, other: &Character) -> bool {
    self.kind != other.kind
  }
}

pub struct Zone {
  pub characters: HashMap<Seat, Character>,
  pub parties: Parties,
  /// The library's flat `(x, z)` grid, rebuilt each tick because everyone
  /// moves.
  ///
  /// Two-dimensional, which is the right default and very nearly the right
  /// answer here: a landscape is locally 2.5D, so a flat query plus a height
  /// filter is exact at identical query cost. `tests/tower.rs` is where that
  /// stops holding, on the one structure in the zone that stacks.
  grid: SpatialGrid<u32>,
  /// Whether the grid still describes where everyone is.
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
  /// Beasts killed, which is the closest this has to a score.
  pub slain: u64,
}

impl Default for Zone {
  fn default() -> Self {
    Self {
      characters: HashMap::new(),
      parties: Parties::default(),
      grid: SpatialGrid::new(GridQuantizer::new((-terrain::EDGE, -terrain::EDGE), CELL)),
      stale: true,
      examined: 0,
      returned: 0,
      authority: Authority::default(),
      now_ms: 0,
      refusals: 0,
      landed: 0,
      revives: 0,
      slain: 0,
    }
  }
}

impl Zone {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn admit(&mut self, seat: Seat, at: (f32, f32, f32)) {
    self
      .characters
      .insert(seat, Character::adventurer(seat, at, self.now_ms));
    self.stale = true;
  }

  pub fn admit_beast(&mut self, seat: Seat, at: (f32, f32, f32)) {
    self.characters.insert(seat, Character::beast(seat, at, self.now_ms));
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
  /// that anything is wrong.
  pub fn place(&mut self, seat: Seat, at: (f32, f32, f32)) {
    if let Some(character) = self.characters.get_mut(&seat) {
      character.tracked.at = at;
      self.stale = true;
    }
  }

  pub fn face(&mut self, seat: Seat, yaw: f32) {
    if let Some(character) = self.characters.get_mut(&seat) {
      character.yaw = yaw;
    }
  }

  /// Aims at a seat, or at nobody.
  ///
  /// Checked when the cast lands rather than now, because a target that walks
  /// away mid-cast is the ordinary case.
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
      character.yaw = yaw;
    }
  }

  /// Moves everyone from their held input, which is what the server does when
  /// it is the one deciding.
  fn drive(&mut self, dt_ms: Ms) {
    let step = crate::movement::RUN_SPEED * (dt_ms as f32 / 1000.0);
    for character in self.characters.values_mut() {
      if character.is_beast() || !character.alive {
        continue;
      }
      let (yaw, forward) = character.intent;
      if forward == 0 {
        continue;
      }
      let travel = step * forward as f32;
      let at = character.tracked.at;
      let (x, z) = (at.0 + yaw.sin() * travel, at.2 + yaw.cos() * travel);
      let (x, z) = (
        x.clamp(-terrain::EDGE + 2.0, terrain::EDGE - 2.0),
        z.clamp(-terrain::EDGE + 2.0, terrain::EDGE - 2.0),
      );
      character.tracked.at = (x, terrain::ground_at(x, z), z);
      self.stale = true;
    }
  }

  /// Takes a claimed position, or does not.
  ///
  /// Two rules, and the second is only possible because the ground is a shared
  /// rule: a speed budget cannot see a client hovering, because hovering costs
  /// no horizontal distance at all.
  pub fn claim(&mut self, seat: Seat, to: (f32, f32, f32)) -> Verdict {
    if self.authority == Authority::Server {
      return Verdict::Refused;
    }
    if to.0.abs() > terrain::EDGE || to.2.abs() > terrain::EDGE {
      self.refusals += 1;
      return Verdict::Refused;
    }
    if to.1 - terrain::ground_at(to.0, to.2) > MAX_AIR {
      self.refusals += 1;
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

  /// Begins an ability, if this seat can pay for it and is allowed to act.
  pub fn begin_cast(&mut self, seat: Seat, index: u8, _cast_ms: Ms) -> bool {
    let Some(spell) = ability(index) else { return false };
    let now = self.now_ms;
    let Some(character) = self.characters.get_mut(&seat) else {
      return false;
    };
    if !character.alive || character.casting.is_some() || now < character.ready_at {
      return false;
    }
    if character.mana < spell.mana as f32 {
      return false;
    }
    character.mana -= spell.mana as f32;
    character.casting = Some(Cast {
      ability: index,
      lands_at: now + spell.cast_ms,
    });
    character.ready_at = now + spell.cast_ms.max(GLOBAL_COOLDOWN_MS);
    true
  }

  /// Advances the clock and resolves everything due.
  ///
  /// Returns the seats whose cast landed, because that is an **event**: it
  /// happens once and no later frame mentions it.
  pub fn advance(&mut self, dt_ms: Ms) -> Vec<Seat> {
    self.now_ms += dt_ms;
    if self.authority == Authority::Server {
      self.drive(dt_ms);
    }
    self.hunt(dt_ms);
    let now = self.now_ms;
    let regen = MANA_REGEN * (dt_ms as f32 / 1000.0);

    for character in self.characters.values_mut() {
      if !character.alive && now >= character.up_at {
        let at = if character.is_beast() {
          character.home
        } else {
          terrain::footing_near(character.home.0, character.home.2)
        };
        character.alive = true;
        character.health = character.max_health;
        character.mana = MAX_MANA as f32;
        character.tracked = Tracked::new(at, now);
        character.target = None;
        self.revives += 1;
        self.stale = true;
      }
      if character.alive && character.casting.is_none() {
        character.mana = (character.mana + regen).min(MAX_MANA as f32);
      }
    }

    let mut landed = Vec::new();
    for character in self.characters.values_mut() {
      let Some(cast) = character.casting else {
        continue;
      };
      if now >= cast.lands_at {
        character.casting = None;
        landed.push((character.seat, cast.ability));
      }
    }
    self.landed += landed.len() as u64;

    // Resolved after the loop, because a landing reads one character and
    // writes another, and the two can be the same seat's target chain.
    for (caster, index) in &landed {
      let Some(spell) = ability(*index) else { continue };
      self.resolve(*caster, spell);
    }
    // The clock moved, so the index describes a world that no longer exists:
    // a body may have finished falling out of it without anybody touching a
    // position. Marking it here rather than at each of the places that might
    // have caused it is what keeps that from being a hunt for the one caller
    // that forgot.
    self.stale = true;
    landed.into_iter().map(|(seat, _)| seat).collect()
  }

  /// Applies one landed ability. The whole of hit detection, on the server, at
  /// one instant, against a named target.
  fn resolve(&mut self, caster: Seat, spell: Ability) {
    let Some((from, target, kind)) = self
      .characters
      .get(&caster)
      .map(|c| (c.tracked.at, c.target, c.kind))
    else {
      return;
    };
    let Some(target) = target else { return };
    let Some(victim) = self.characters.get_mut(&target).filter(|v| v.alive) else {
      return;
    };
    if distance(from, victim.tracked.at) > spell.range {
      return;
    }
    let friendly = victim.kind == kind;
    if spell.hostile == friendly {
      return;
    }

    if spell.heal > 0 {
      victim.health = (victim.health + spell.heal).min(victim.max_health);
      return;
    }

    victim.health = victim.health.saturating_sub(spell.damage);
    if victim.health == 0 {
      // Down rather than gone. It is the third reason somebody leaves your
      // frame, after walking away and disconnecting, and the one that shows
      // the two relevance channels apart: a downed **party member** is still
      // subscribed, so they stay in the party frame at zero health while
      // their body leaves the world.
      victim.alive = false;
      victim.up_at = self.now_ms + DOWN_MS;
      victim.casting = None;
      victim.target = None;
      if victim.is_beast() {
        self.slain += 1;
      }
      self.stale = true;
    }
  }

  /// What the beasts do, which is the only simulation the server runs.
  fn hunt(&mut self, dt_ms: Ms) {
    let now = self.now_ms;
    let step = BEAST_SPEED * (dt_ms as f32 / 1000.0);
    let seats: Vec<Seat> = self
      .characters
      .values()
      .filter(|c| c.is_beast() && c.alive)
      .map(|c| c.seat)
      .collect();

    for seat in seats {
      let me = self.characters[&seat];
      let far_from_home = distance(me.tracked.at, me.home);

      // A prey worth chasing: an adventurer, alive, inside the aggro radius,
      // and not so far that the beast would abandon its ground.
      let prey = if far_from_home > LEASH {
        None
      } else {
        self
          .characters
          .values()
          .filter(|c| c.alive && c.hostile_to(&me))
          .filter(|c| distance(me.tracked.at, c.tracked.at) <= AGGRO)
          .min_by(|a, b| {
            distance(me.tracked.at, a.tracked.at).total_cmp(&distance(me.tracked.at, b.tracked.at))
          })
          .map(|c| (c.seat, c.tracked.at))
      };

      let goal = match prey {
        Some((_, at)) => at,
        None => me.home,
      };
      let gap = distance(me.tracked.at, goal);
      let want_close = prey.map(|_| CLAW.range * 0.8).unwrap_or(0.6);

      if gap > want_close {
        let (dx, dz) = (goal.0 - me.tracked.at.0, goal.2 - me.tracked.at.2);
        let len = (dx * dx + dz * dz).sqrt().max(f32::EPSILON);
        let (x, z) = (
          me.tracked.at.0 + dx / len * step,
          me.tracked.at.2 + dz / len * step,
        );
        let yaw = dx.atan2(dz);
        self.place(seat, (x, terrain::ground_at(x, z), z));
        self.face(seat, yaw);
      }

      if let Some((victim, at)) = prey
        && distance(self.characters[&seat].tracked.at, at) <= CLAW.range
        && now >= me.swing_at
      {
        if let Some(beast) = self.characters.get_mut(&seat) {
          beast.swing_at = now + CLAW_EVERY_MS;
          beast.target = Some(victim);
        }
        self.resolve(seat, CLAW);
      }
    }
  }

  /// Rebuilds the spatial index. Called once a tick, before anyone queries it.
  fn reindex(&mut self) {
    self.stale = false;
    self.grid.clear();
    let now = self.now_ms;
    // A body still going over is still in the world. Excluding it here is what
    // made a beast disappear the instant it died: it left the index, so it
    // left every audience, so no client was ever told it had fallen.
    for character in self
      .characters
      .values()
      .filter(|c| c.alive || c.still_falling(now))
    {
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
    // A downed player still watches the zone while they wait, so the viewer's
    // own liveness does not gate the query.
    let Some(from) = self.characters.get(&seat).map(|c| c.tracked.at) else {
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
      if distance(from, character.tracked.at) <= VIEW {
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
  use crate::abilities::{BOLT, MEND, STRIKE};

  fn zone() -> Zone {
    let mut zone = Zone::new();
    zone.admit(1, (0.0, terrain::ground_at(0.0, 0.0), 0.0));
    zone.admit(2, (5.0, terrain::ground_at(5.0, 0.0), 0.0));
    zone.admit(3, (200.0, 0.0, 200.0));
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
    assert!(zone.begin_cast(1, 1, 0), "the first one goes");
    assert!(!zone.begin_cast(1, 1, 0), "not while already casting");

    let landed = zone.advance(BOLT.cast_ms);
    assert_eq!(landed, vec![1], "and it lands once");
  }

  #[test]
  fn an_ability_costs_mana_and_runs_out() {
    // The resource that makes a bar a choice rather than a key to hold.
    let mut zone = zone();
    let mut casts = 0;
    for _ in 0..12 {
      if zone.begin_cast(1, 1, 0) {
        casts += 1;
      }
      zone.advance(GLOBAL_COOLDOWN_MS);
    }
    assert!(casts > 0, "nothing was castable at all");
    assert!(
      zone.characters[&1].mana < MAX_MANA as f32,
      "mana never moved"
    );
  }

  #[test]
  fn mana_comes_back_when_you_stop() {
    let mut zone = zone();
    zone.begin_cast(1, 1, 0);
    zone.advance(BOLT.cast_ms);
    let spent = zone.characters[&1].mana;
    zone.advance(4000);
    assert!(zone.characters[&1].mana > spent);
  }

  #[test]
  fn the_cooldown_runs_during_a_cast_rather_than_after_it() {
    let mut long = zone();
    long.begin_cast(1, 2, 0);
    long.advance(MEND.cast_ms);
    assert!(long.begin_cast(1, 0, 0), "a full-length cast is ready when it lands");

    let mut short = zone();
    short.begin_cast(1, 0, 0);
    short.advance(1);
    assert!(!short.begin_cast(1, 0, 0), "an instant still owes the cooldown");
    short.advance(GLOBAL_COOLDOWN_MS);
    assert!(short.begin_cast(1, 0, 0));
  }

  #[test]
  fn a_landing_is_reported_once_and_never_again() {
    let mut zone = zone();
    zone.begin_cast(1, 1, 0);
    assert!(zone.advance(BOLT.cast_ms - 100).is_empty(), "not yet");
    assert_eq!(zone.advance(200), vec![1], "now");
    assert!(zone.advance(1000).is_empty(), "and not a second time");
  }

  #[test]
  fn an_ability_only_reaches_what_it_is_aimed_at_and_only_in_range() {
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit_beast(2, (4.0, 0.0, 0.0));
    zone.aim(1, Some(2));
    zone.begin_cast(1, 0, 0);
    zone.advance(1);
    assert_eq!(zone.characters[&2].health, BEAST_HEALTH - STRIKE.damage);

    // Out of reach when it lands, which is the ordinary case.
    zone.place(2, (STRIKE.range * 4.0, 0.0, 0.0));
    zone.advance(GLOBAL_COOLDOWN_MS);
    assert!(zone.begin_cast(1, 0, 0));
    zone.advance(1);
    assert_eq!(
      zone.characters[&2].health,
      BEAST_HEALTH - STRIKE.damage,
      "a landing out of range does nothing"
    );
  }

  #[test]
  fn you_cannot_strike_your_own_side_or_heal_a_beast() {
    // Without this the bots farm each other and a healer is a weapon.
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit(2, (2.0, 0.0, 0.0));
    // Well outside its aggro radius, or the beast mauls the ally this test is
    // about and the assertion fails for a reason it is not testing.
    zone.admit_beast(3, (AGGRO * 6.0, 0.0, 0.0));

    zone.aim(1, Some(2));
    zone.begin_cast(1, 0, 0);
    zone.advance(1);
    assert_eq!(zone.characters[&2].health, MAX_HEALTH, "struck an ally");

    zone.characters.get_mut(&3).unwrap().health = 10;
    zone.place(3, (2.5, 0.0, 0.0));
    zone.advance(GLOBAL_COOLDOWN_MS);
    zone.aim(1, Some(3));
    assert!(zone.begin_cast(1, 2, 0));
    zone.advance(MEND.cast_ms);
    assert_eq!(zone.characters[&3].health, 10, "healed a beast");
  }

  #[test]
  fn mend_puts_health_back_and_never_past_full() {
    let mut zone = zone();
    zone.characters.get_mut(&2).unwrap().health = 20;
    zone.aim(1, Some(2));
    assert!(zone.begin_cast(1, 2, 0));
    zone.advance(MEND.cast_ms);
    assert_eq!(zone.characters[&2].health, 20 + MEND.heal);

    zone.characters.get_mut(&2).unwrap().health = MAX_HEALTH - 2;
    zone.advance(GLOBAL_COOLDOWN_MS);
    zone.characters.get_mut(&1).unwrap().mana = MAX_MANA as f32;
    assert!(zone.begin_cast(1, 2, 0));
    zone.advance(MEND.cast_ms);
    assert_eq!(zone.characters[&2].health, MAX_HEALTH);
  }

  #[test]
  fn a_downed_character_leaves_the_world_and_comes_back_up() {
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit_beast(2, (3.0, 0.0, 0.0));
    zone.aim(1, Some(2));
    let mut casts = 0;
    while zone.characters[&2].alive {
      zone.begin_cast(1, 0, 0);
      zone.advance(GLOBAL_COOLDOWN_MS);
      casts += 1;
      assert!(casts < 30, "a beast should not take this long");
    }
    assert_eq!(zone.slain, 1);

    // Still there while it goes over, and gone once it has.
    let mut scratch = Vec::new();
    zone.near(1, &mut scratch);
    assert!(scratch.contains(&2), "a body has to be visible long enough to fall");
    zone.advance(CORPSE_MS + 100);
    zone.near(1, &mut scratch);
    assert!(!scratch.contains(&2), "a fallen character is not in view");

    zone.advance(DOWN_MS);
    assert!(zone.characters[&2].alive);
    assert_eq!(zone.characters[&2].health, BEAST_HEALTH);
    assert!(zone.revives > 0);
  }

  #[test]
  fn a_beast_chases_what_comes_close_and_goes_home_after() {
    let mut zone = Zone::new();
    let home = (0.0, terrain::ground_at(0.0, 0.0), 0.0);
    zone.admit_beast(1, home);
    zone.admit(2, (AGGRO * 0.5, terrain::ground_at(AGGRO * 0.5, 0.0), 0.0));

    for _ in 0..90 {
      zone.advance(33);
    }
    let chased = distance(zone.characters[&1].tracked.at, home);
    assert!(chased > 1.0, "the beast never left home to chase");
    assert!(
      zone.characters[&2].health < MAX_HEALTH,
      "the beast caught nobody"
    );

    zone.remove(2);
    for _ in 0..300 {
      zone.advance(33);
    }
    assert!(
      distance(zone.characters[&1].tracked.at, home) < 1.5,
      "the beast never went home"
    );
  }

  #[test]
  fn a_beast_gives_up_past_its_leash() {
    // Otherwise one player drags the whole zone across the map behind them.
    let mut zone = Zone::new();
    let home = (0.0, 0.0, 0.0);
    zone.admit_beast(1, home);
    zone.admit(2, (0.0, 0.0, 0.0));

    // Standing at the end of the leash, which is inside the aggro radius of
    // where the beast now is but outside what it will tolerate.
    zone.place(1, (LEASH + 2.0, 0.0, 0.0));
    zone.place(2, (LEASH + 4.0, 0.0, 0.0));
    let before = zone.characters[&2].health;
    for _ in 0..60 {
      zone.advance(33);
    }
    assert_eq!(zone.characters[&2].health, before, "it kept fighting past the leash");
    assert!(
      distance(zone.characters[&1].tracked.at, home) < LEASH + 2.0,
      "it did not turn back"
    );
  }

  #[test]
  fn a_claim_above_the_ground_is_refused() {
    // The check a speed budget cannot make: flying costs no distance.
    let mut zone = zone();
    let at = zone.characters[&1].tracked.at;
    assert_eq!(
      zone.claim(1, (at.0, at.1 + MAX_AIR + 5.0, at.2)),
      Verdict::Refused
    );
    assert_eq!(zone.refusals, 1);
  }

  #[test]
  fn a_jump_is_not_refused() {
    // The same rule must leave the honest case alone, or jumping is a ban.
    let mut zone = zone();
    let at = zone.characters[&1].tracked.at;
    zone.advance(500);
    assert_eq!(
      zone.claim(1, (at.0, at.1 + crate::movement::JUMP_SPEED * 0.2, at.2)),
      Verdict::Accepted
    );
  }

  #[test]
  fn a_claim_outside_the_world_is_refused() {
    let mut zone = zone();
    zone.advance(1000);
    assert_eq!(zone.claim(1, (terrain::EDGE + 40.0, 0.0, 0.0)), Verdict::Refused);
  }

  #[test]
  fn a_refused_claim_is_counted_because_it_is_the_only_signal() {
    let mut zone = zone();
    let at = zone.characters[&1].tracked.at;
    assert_eq!(zone.claim(1, (at.0 + 40.0, at.1, at.2)), Verdict::Refused);
    assert_eq!(zone.refusals, 1);

    zone.advance(1000);
    assert_eq!(zone.claim(1, (at.0 + 1.0, at.1, at.2)), Verdict::Accepted);
    assert_eq!(zone.refusals, 1);
  }
}
