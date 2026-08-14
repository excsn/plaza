//! A zone of characters on generated ground, and what one client is told about
//! it.
//!
//! Everything the server owns lives here: where people are, what they are
//! casting, what is hunting them, and who is close enough to be told. The
//! ground itself is not owned, because it is a rule both ends derive from
//! [`terrain`](crate::terrain) rather than a payload anyone sends.

use plaza_server_utils::relevance::{CellSpace, CellTable, GridQuantizer, SpatialGrid};

use crate::abilities::{ability, Ability, CLAW};
use crate::casting::{Ms, GLOBAL_COOLDOWN_MS};
use crate::controls::Authority;
use crate::movement::{distance, Tracked, Verdict, MAX_AIR};
use crate::protocol::{Kind, Landed, Packed, Seen};
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
  /// How many times this character has been placed somewhere it did not walk
  /// to. Read by the client to know an arrival is not an echo.
  pub spawns: u32,
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
      spawns: 1,
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

/// Everyone in the zone, indexed by the seat rather than hashed on it.
///
/// A seat comes from `Roster`, which hands out the lowest free one, so seats
/// are dense from zero and a slot lookup is an offset. That matters because of
/// where the lookup sits: a frame is built per client and reads every character
/// in that client's audience, so the count is clients times audience, which at
/// four thousand of each is a hundred and eighty thousand lookups a tick.
///
/// The surface is `HashMap`'s on purpose, down to taking `&Seat`, so the
/// seventy-odd call sites that read it did not have to change to say the same
/// thing a different way.
#[derive(Debug, Default, Clone)]
pub struct Bodies {
  slots: Vec<Option<Character>>,
  live: usize,
}

impl Bodies {
  pub fn get(&self, seat: &Seat) -> Option<&Character> {
    self.slots.get(*seat as usize)?.as_ref()
  }

  pub fn get_mut(&mut self, seat: &Seat) -> Option<&mut Character> {
    self.slots.get_mut(*seat as usize)?.as_mut()
  }

  pub fn contains_key(&self, seat: &Seat) -> bool {
    self.get(seat).is_some()
  }

  pub fn insert(&mut self, seat: Seat, character: Character) {
    let index = seat as usize;
    if index >= self.slots.len() {
      self.slots.resize_with(index + 1, || None);
    }
    if self.slots[index].replace(character).is_none() {
      self.live += 1;
    }
  }

  pub fn remove(&mut self, seat: &Seat) -> Option<Character> {
    let taken = self.slots.get_mut(*seat as usize)?.take();
    if taken.is_some() {
      self.live -= 1;
    }
    taken
  }

  pub fn len(&self) -> usize {
    self.live
  }

  pub fn is_empty(&self) -> bool {
    self.live == 0
  }

  pub fn values(&self) -> impl Iterator<Item = &Character> {
    self.slots.iter().flatten()
  }

  pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Character> {
    self.slots.iter_mut().flatten()
  }

  pub fn keys(&self) -> impl Iterator<Item = Seat> + '_ {
    self.values().map(|c| c.seat)
  }
}

impl std::ops::Index<&Seat> for Bodies {
  type Output = Character;

  fn index(&self, seat: &Seat) -> &Character {
    self.get(seat).expect("no character in that seat")
  }
}

pub struct Zone {
  pub characters: Bodies,
  pub parties: Parties,
  /// The library's flat `(x, z)` grid, rebuilt each tick because everyone
  /// moves.
  ///
  /// Two-dimensional, which is the right default and very nearly the right
  /// answer here: a landscape is locally 2.5D, so a flat query plus a height
  /// filter is exact at identical query cost. `tests/tower.rs` is where that
  /// stops holding, on the one structure in the zone that stacks.
  grid: SpatialGrid<u32>,
  /// The same cells as `grid`, addressed densely, because publishing keys three
  /// tables by cell and every one of them is read `viewers x cells-per-view`
  /// times a tick. `publish_costs` prices the hash those lookups would
  /// otherwise pay at 1.39x of a whole tick, and 8x on the fan-out path.
  space: CellSpace,
  /// Whether the grid still describes where everyone is.
  stale: bool,
  /// What the grid last handed back, kept so a query per client per tick is
  /// not also an allocation per client per tick.
  candidates: Vec<u32>,
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
      characters: Bodies::default(),
      parties: Parties::default(),
      grid: SpatialGrid::new(GridQuantizer::new((-terrain::EDGE, -terrain::EDGE), CELL)),
      space: CellSpace::new(
        GridQuantizer::new((-terrain::EDGE, -terrain::EDGE), CELL),
        terrain::EDGE * 2.0,
      ),
      stale: true,
      candidates: Vec::new(),
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

  /// A zone whose spatial index reaches `extent` units from the origin in each
  /// direction.
  ///
  /// [`GridQuantizer`] clamps anything outside its origin into the boundary
  /// cells, and [`publish`](Self::publish) ships whole cells, so an index
  /// smaller than the world does not merely waste query effort the way a
  /// per-client distance test did: it puts bodies nobody can see on the wire.
  /// Anything spreading a population past [`terrain::EDGE`] must say so here.
  pub fn spanning(extent: f32) -> Self {
    let quantizer = GridQuantizer::new((-extent, -extent), CELL);
    Self {
      grid: SpatialGrid::new(quantizer),
      space: CellSpace::new(quantizer, extent * 2.0),
      ..Self::default()
    }
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
  pub fn advance(&mut self, dt_ms: Ms) -> Vec<Landed> {
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
        // The client owns its position, so it has to be told it was moved or
        // it stands where it died and every claim it sends is refused.
        character.spawns += 1;
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
    let mut out = Vec::with_capacity(landed.len());
    for (caster, index) in &landed {
      let Some(spell) = ability(*index) else { continue };
      let victim = self.resolve(*caster, spell);
      out.push(Landed {
        seat: *caster,
        ability: *index,
        victim,
      });
    }
    // The clock moved, so the index describes a world that no longer exists:
    // a body may have finished falling out of it without anybody touching a
    // position. Marking it here rather than at each of the places that might
    // have caused it is what keeps that from being a hunt for the one caller
    // that forgot.
    self.stale = true;
    out
  }

  /// Applies one landed ability. The whole of hit detection, on the server, at
  /// one instant, against a named target.
  fn resolve(&mut self, caster: Seat, spell: Ability) -> Option<Seat> {
    let (from, target, kind) = self
      .characters
      .get(&caster)
      .map(|c| (c.tracked.at, c.target, c.kind))?;
    let target = target?;
    let victim = self.characters.get_mut(&target).filter(|v| v.alive)?;
    if distance(from, victim.tracked.at) > spell.range {
      return None;
    }
    let friendly = victim.kind == kind;
    if spell.hostile == friendly {
      return None;
    }

    if spell.heal > 0 {
      victim.health = (victim.health + spell.heal).min(victim.max_health);
      return Some(target);
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
    Some(target)
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
    // `query_radius` extends rather than clears, so a reused buffer has to be
    // emptied here or every query inherits the last one's answer.
    let mut candidates = std::mem::take(&mut self.candidates);
    candidates.clear();
    self.grid.query_radius(from.0, from.2, VIEW, &mut candidates);
    self.examined += candidates.len() as u64;
    for id in &candidates {
      let other = *id as Seat;
      let Some(character) = self.characters.get(&other) else {
        continue;
      };
      if distance(from, character.tracked.at) <= VIEW {
        out.push(other);
      }
    }
    self.candidates = candidates;
    self.returned += out.len() as u64;
    out.sort_unstable();
  }

  /// Everything a seat is told about this tick: near, plus subscribed.
  pub fn audience_for(&mut self, seat: Seat, scratch: &mut Vec<Seat>) -> Audience {
    self.near(seat, scratch);
    audience(scratch, &self.parties, seat)
  }

  /// Packs the spatial channel once for the whole zone: one payload per
  /// occupied grid cell, each shared by every client whose view touches the
  /// cell. This is what stops the build cost tracking the client count.
  pub fn publish_at(&mut self, into: &mut Publication, precision: crate::protocol::Precision) {
    if self.stale {
      self.reindex();
    }
    let now = self.now_ms;
    into.cells.clear_each();
    into.coarse.clear_each();
    into.occupied = 0;
    for (key, ids) in self.grid.occupied() {
      let (cx, cz) = plaza_server_utils::relevance::morton::decode_2d(key);
      let at = self.space.index_at(cx, cz);
      let live = ids.iter().filter(|id| self.characters.contains_key(&(**id as Seat))).count();
      let mut w = plaza_wire::bits::BitWriter::with_capacity(ids.len() * 16);
      match precision {
        crate::protocol::Precision::Absolute => {
          crate::pack::open(&mut w, live);
          for id in ids {
            let Some(character) = self.characters.get(&(*id as Seat)) else { continue };
            crate::pack::write(&mut w, &seen_of(character, now));
          }
        }
        crate::protocol::Precision::CellRelative => {
          crate::pack::open_cell(&mut w, at, live);
          let corner = self.space.corner(at);
          for id in ids {
            let Some(character) = self.characters.get(&(*id as Seat)) else { continue };
            crate::pack::write_in_cell(&mut w, &seen_of(character, now), corner);
          }
        }
        crate::protocol::Precision::Graded => {
          let corner = self.space.corner(at);
          crate::pack::open_graded(&mut w, at, live, false);
          let mut c = plaza_wire::bits::BitWriter::with_capacity(ids.len() * 14);
          crate::pack::open_graded(&mut c, at, live, true);
          for id in ids {
            let Some(character) = self.characters.get(&(*id as Seat)) else { continue };
            let body = seen_of(character, now);
            crate::pack::write_in_cell_at(&mut w, &body, corner, crate::pack::REL_BITS);
            crate::pack::write_in_cell_at(&mut c, &body, corner, crate::pack::GRADED_COARSE_BITS);
          }
          if let Some(slot) = into.coarse.get_mut(at) {
            *slot = Some(Packed::new(c.finish()));
          }
        }
      }
      if let Some(slot) = into.cells.get_mut(at) {
        *slot = Some(Packed::new(w.finish()));
        into.occupied += 1;
      }
    }
  }

  /// A publication sized to this zone, for a caller that reuses one across
  /// ticks rather than allocating per tick.
  pub fn publication(&self) -> Publication {
    Publication {
      cells: CellTable::new(self.space),
      coarse: CellTable::new(self.space),
      occupied: 0,
    }
  }

  /// The dense indices of the cells a view standing at `(x, z)` touches.
  ///
  /// Cell-granular like the grid itself: a superset of the view disc, which is
  /// the precision the spatial channel trades for being shared.
  pub fn cells_touching(&self, x: f32, z: f32) -> impl Iterator<Item = usize> + use<> {
    self.space.indices_in_radius(x, z, VIEW)
  }

  /// The dense index of the cell a point falls in.
  pub fn cell_index(&self, x: f32, z: f32) -> usize {
    self.space.index_of(x, z)
  }

  pub fn space(&self) -> &CellSpace {
    &self.space
  }

  /// The half-extent this zone's index covers, which is what a client needs to
  /// rebuild the same [`CellSpace`].
  pub fn extent(&self) -> f32 {
    -self.space.quantizer().corner(0, 0).0
  }
}

/// The `CellSpace` a zone of half-extent `extent` uses.
///
/// Both ends derive it from one number rather than the server describing every
/// cell, which is the same rule the terrain follows.
pub fn space_for(extent: f32) -> CellSpace {
  CellSpace::new(GridQuantizer::new((-extent, -extent), CELL), extent * 2.0)
}

/// One tick's spatial channel: a packed payload per occupied cell, keyed by
/// [`CellSpace`] so a viewer's lookups are array indexes rather than hashes.
///
/// Under [`Precision::Graded`](crate::protocol::Precision::Graded) each cell is
/// published twice, at both widths, because the width a viewer wants depends on
/// its distance to the cell and a shared payload cannot answer per viewer.
pub struct Publication {
  cells: CellTable<Option<Packed>>,
  coarse: CellTable<Option<Packed>>,
  occupied: usize,
}

/// How far a cell's centre may be before a viewer takes its coarse payload.
pub const COARSE_BEYOND: f32 = VIEW / 2.0;

/// Whether a cell `(dx, dz)` away is far enough to take the coarse width.
///
/// A function of the **offset alone**, because cell centres sit on a regular
/// lattice: every viewer in one cell reads every cell in its window the same
/// way. That is what turns a per-listener distance into a fixed 7x7 mask, and
/// it is the reason the graded audience split need not touch a viewer at all.
pub const fn offset_is_coarse(dx: i32, dz: i32) -> bool {
  let (x, z) = (dx as f32 * CELL, dz as f32 * CELL);
  x * x + z * z > COARSE_BEYOND * COARSE_BEYOND
}

impl Publication {
  pub fn cell(&self, index: usize) -> Option<&Packed> {
    self.cells.get(index).and_then(|slot| slot.as_ref())
  }

  /// The payload a viewer `distance` from this cell's centre should be sent.
  pub fn cell_for(&self, index: usize, distance: f32) -> Option<&Packed> {
    if distance > COARSE_BEYOND
      && let Some(Some(payload)) = self.coarse.get(index)
    {
      return Some(payload);
    }
    self.cell(index)
  }

  /// Every occupied cell and its payload, for a caller addressing per cell
  /// rather than assembling per viewer.
  pub fn occupied(&self) -> impl Iterator<Item = (usize, &Packed)> {
    self.cells.occupied().filter_map(|(at, slot)| slot.as_ref().map(|p| (at, p)))
  }

  pub fn cells(&self) -> usize {
    self.occupied
  }
}

/// One character as the wire describes them. `because` is not on the wire:
/// the reader stamps it from which channel the entry arrived on.
pub fn seen_of(character: &Character, now: Ms) -> Seen {
  Seen {
    seat: character.seat,
    at: character.tracked.at,
    health: character.health,
    max_health: character.max_health,
    yaw: character.yaw,
    kind: character.kind,
    because: crate::protocol::Because::Near,
    casting_ms: character.casting.map(|cast| cast.lands_at.saturating_sub(now) as u32),
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
    assert_eq!(landed.len(), 1, "and it lands once");
    assert_eq!(landed[0].seat, 1);
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
  fn a_landing_says_what_it_was_and_what_it_reached() {
    // Both halves are needed to draw it and neither is derivable later: no
    // frame mentions a landing twice, and by the time one arrives the
    // victim's health has already moved.
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit_beast(2, (3.0, 0.0, 0.0));
    zone.aim(1, Some(2));
    zone.begin_cast(1, 0, 0);
    let landed = zone.advance(1);
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].ability, 0);
    assert_eq!(landed[0].victim, Some(2));
  }

  #[test]
  fn a_swing_through_empty_air_is_still_an_event() {
    // Otherwise a press that misses is indistinguishable from a press that did
    // nothing, which is the whole complaint the feedback work started from.
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.begin_cast(1, 0, 0);
    let landed = zone.advance(1);
    assert_eq!(landed.len(), 1, "the cast has to be announced");
    assert_eq!(landed[0].victim, None, "and say that it reached nobody");
  }

  #[test]
  fn a_landing_is_reported_once_and_never_again() {
    let mut zone = zone();
    zone.begin_cast(1, 1, 0);
    assert!(zone.advance(BOLT.cast_ms - 100).is_empty(), "not yet");
    let now = zone.advance(200);
    assert_eq!(now.len(), 1, "now");
    assert_eq!(now[0].seat, 1);
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
