//! The world, and everything that happens in it on a tick.
//!
//! Two halves that behave nothing alike, which is most of what this example is
//! for.
//!
//! **The moving half** is a few dozen actors, every one of which is somewhere
//! different from last tick. That is the ordinary relevance problem, and every
//! example in this tree already solves it.
//!
//! **The still half** is a couple of thousand props, of which perhaps one
//! changes a tick. Their positions are derived rather than stored, so the
//! world's contents cost nothing to hold and nothing to join; what is stored is
//! the small set that is currently out, and what it is stored as is the tick it
//! comes back on rather than how long it has left. An absolute tick is the same
//! answer every tick, and a state that does not change is a state you can send
//! once.
//!
//! And a third thing that is neither: an item on the ground, whose **audience
//! is a rule**. It belongs to whoever dropped it until a timer runs out and to
//! everybody afterwards, and no distance query or subscription set expresses
//! that.

use std::collections::{HashMap, VecDeque};

use crate::pack::Pack;
use crate::path::{Goal, Pathfinder};
use crate::protocol::{Doing, Fire, Happened, Item, Look, Lying, ObjectState, Queued, Refusal, Seat, Tile};
use crate::skills::{self, Skill, SKILLS};
use crate::world::{self, Prop};

/// How far a client is told about, in squares.
pub const VIEW: i32 = 24;

/// How many actors the world holds.
pub const MAX_ACTORS: usize = 240;

pub const PERSON_HEALTH: u16 = 30;
pub const HEN_HEALTH: u16 = 6;
pub const BRUTE_HEALTH: u16 = 22;

/// Ticks a person spends down before coming back at the green.
pub const DOWN_TICKS: u64 = 8;
/// Ticks a fallen hen or brute lies there before it is back at its patch.
pub const CARCASS_TICKS: u64 = 20;

/// Ticks a dropped item belongs to the one who dropped it, and nobody else.
///
/// The number that makes an audience a rule. Long enough that walking away and
/// coming back is a decision, short enough that it happens while somebody is
/// watching.
pub const OWNER_TICKS: u64 = 50;
/// Ticks before a dropped item is gone for good.
pub const GROUND_TICKS: u64 = 220;
/// Ticks a fire burns.
pub const FIRE_TICKS: u64 = 150;

/// Ticks between blows.
pub const SWING_TICKS: u64 = 2;
/// Squares a brute notices somebody from.
pub const AGGRO: i32 = 5;
/// Squares from its patch a brute will chase before it turns back.
pub const LEASH: i32 = 14;
/// Ticks a cooked fish is worth.
pub const HEAL: u16 = 9;

/// What one of the eight facings means, clockwise from north.
pub const FACINGS: [(i16, i16); 8] = [
  (0, -1),
  (1, -1),
  (1, 0),
  (1, 1),
  (0, 1),
  (-1, 1),
  (-1, 0),
  (-1, -1),
];

pub fn facing_between(from: Tile, to: Tile) -> u8 {
  let (dx, dy) = ((to.x - from.x).signum(), (to.y - from.y).signum());
  FACINGS
    .iter()
    .position(|step| *step == (dx, dy))
    .unwrap_or(4) as u8
}

/// An item lying where somebody left it.
#[derive(Clone, Copy, Debug)]
pub struct Dropped {
  pub tile: Tile,
  pub item: Item,
  /// Who may take it until the timer runs out. `None` is everybody's already.
  pub owner: Option<Seat>,
  pub public_at: u64,
  pub gone_at: u64,
}

/// Anything with a body: a player, one of the world's own, a hen, a brute.
#[derive(Clone, Debug)]
pub struct Actor {
  pub tile: Tile,
  pub look: Look,
  pub health: u16,
  pub max_health: u16,
  pub facing: u8,
  pub route: VecDeque<Tile>,
  pub queued: Option<Queued>,
  pub doing: Doing,
  /// Ticks of work put into the current action.
  pub effort: u16,
  pub pack: Pack,
  pub xp: [u32; SKILLS],
  pub running: bool,
  /// The tick a fallen body gets back up on.
  pub up_at: u64,
  /// How many times this body has been put somewhere it did not walk to.
  pub spawns: u32,
  /// Where one of the world's own belongs, and where the dead come back.
  pub home: Tile,
  pub swing_at: u64,
  pub refused: Option<Refusal>,
  /// Whether the pack or the skill sheet moved since the last frame.
  pub private_moved: bool,
}

impl Actor {
  pub fn new(tile: Tile, look: Look) -> Self {
    let max_health = match look {
      Look::Person => PERSON_HEALTH,
      Look::Hen => HEN_HEALTH,
      Look::Brute => BRUTE_HEALTH,
    };
    Self {
      tile,
      look,
      health: max_health,
      max_health,
      facing: 4,
      route: VecDeque::new(),
      queued: None,
      doing: Doing::Idle,
      effort: 0,
      pack: Pack::new(),
      xp: [0; SKILLS],
      running: false,
      up_at: 0,
      spawns: 0,
      home: tile,
      swing_at: 0,
      refused: None,
      private_moved: true,
    }
  }

  pub fn alive(&self) -> bool {
    self.health > 0
  }

  pub fn level(&self, skill: Skill) -> u8 {
    skills::level_for(self.xp[skill.index()])
  }

  pub fn is_person(&self) -> bool {
    self.look == Look::Person
  }

  fn earn(&mut self, skill: Skill, amount: u32, events: &mut Vec<(Tile, Happened)>, seat: Seat) {
    let before = self.level(skill);
    self.xp[skill.index()] += amount;
    self.private_moved = true;
    let _ = seat;
    events.push((self.tile, Happened::Earned {
      skill: skill.index() as u8,
      amount: amount.min(u16::MAX as u32) as u16,
    }));
    let after = self.level(skill);
    if after > before {
      events.push((self.tile, Happened::Levelled {
        skill: skill.index() as u8,
        level: after,
      }));
    }
  }
}

/// A deterministic stream, so a tick replayed is a tick repeated.
#[derive(Clone, Copy, Debug)]
pub struct XorShift(u64);

impl XorShift {
  pub fn new(seed: u64) -> Self {
    Self(seed | 1)
  }

  pub fn next(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }

  pub fn below(&mut self, bound: u32) -> u32 {
    if bound == 0 {
      return 0;
    }
    (self.next() % bound as u64) as u32
  }
}

pub struct Zone {
  pub tick: u64,
  pub actors: HashMap<Seat, Actor>,
  /// Props that are out, by id, against the tick they come back on.
  ///
  /// The whole of the still world's mutable state: a couple of thousand props
  /// exist and this holds only the handful currently missing.
  pub depleted: HashMap<u32, u64>,
  pub fires: HashMap<u32, (Tile, u64)>,
  pub ground: HashMap<u32, Dropped>,
  /// What happened this tick, with where, so a viewer is told only about the
  /// things they could have watched.
  pub events: Vec<(Tile, Happened)>,
  pub finder: Pathfinder,
  rng: XorShift,
  next_ground: u32,
  next_fire: u32,

  pub routes_found: u64,
  pub squares_searched: u64,
  pub gathered: u64,
  pub blows: u64,
  pub falls: u64,
  /// Props used up over the session, which is not the same question as how
  /// many are out right now: a world where everything respawned before anyone
  /// looked would read as a world where nothing ever happened.
  pub depletions: u64,
}

impl Default for Zone {
  fn default() -> Self {
    Self::new()
  }
}

impl std::fmt::Debug for Zone {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Zone")
      .field("tick", &self.tick)
      .field("actors", &self.actors.len())
      .field("depleted", &self.depleted.len())
      .finish_non_exhaustive()
  }
}

impl Zone {
  pub fn new() -> Self {
    Self {
      tick: 0,
      actors: HashMap::new(),
      depleted: HashMap::new(),
      fires: HashMap::new(),
      ground: HashMap::new(),
      events: Vec::new(),
      finder: Pathfinder::new(),
      rng: XorShift::new(0x5EA1_1CE5_0DDB_A11),
      next_ground: 1,
      next_fire: world::FIRE_BASE,
      routes_found: 0,
      squares_searched: 0,
      gathered: 0,
      blows: 0,
      falls: 0,
      depletions: 0,
    }
  }

  /// Everybody, in an order two runs agree about.
  fn seats(&self) -> Vec<Seat> {
    let mut seats: Vec<Seat> = self.actors.keys().copied().collect();
    seats.sort_unstable();
    seats
  }

  pub fn admit(&mut self, seat: Seat, tile: Tile, look: Look) {
    let mut actor = Actor::new(tile, look);
    actor.home = if look == Look::Person { world::the_green() } else { tile };
    self.actors.insert(seat, actor);
  }

  pub fn remove(&mut self, seat: Seat) {
    self.actors.remove(&seat);
  }

  /// Whether a prop is standing right now.
  pub fn prop_ready(&self, id: u32) -> bool {
    !self.depleted.contains_key(&id)
  }

  /// The prop an id names, if it is one and it is there.
  pub fn prop_of(&self, id: u32) -> Option<Prop> {
    if id >= world::FIRE_BASE {
      return None;
    }
    world::prop_at(world::prop_tile(id))
  }

  /// A destination, expanded into a route by the same rule the client used.
  ///
  /// The server does not send the route back. It does not need to: the client
  /// already has it, having run this function on the same map before the op
  /// left the machine.
  pub fn set_route(&mut self, seat: Seat, goal: Goal) -> bool {
    let Some(from) = self.actors.get(&seat).map(|a| a.tile) else {
      return false;
    };
    let route = self.finder.route(from, goal);
    self.squares_searched += self.finder.visited as u64;
    self.routes_found += 1;
    let arrived = goal.reached(from);
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.route = route.into_iter().collect();
      if !actor.route.is_empty() {
        actor.doing = Doing::Walking;
      }
    }
    arrived || self.actors.get(&seat).is_some_and(|a| !a.route.is_empty())
  }

  pub fn walk_to(&mut self, seat: Seat, tile: Tile) {
    if !self.can_act(seat) {
      return;
    }
    if !world::walkable(tile) {
      self.refuse(seat, Refusal::NoRoute);
      return;
    }
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.queued = None;
      actor.effort = 0;
      actor.doing = Doing::Idle;
      actor.refused = None;
    }
    if !self.set_route(seat, Goal::On(tile)) {
      self.refuse(seat, Refusal::NoRoute);
    }
  }

  /// Walk there, then do that. Two things in one op, and the walk is what makes
  /// the round trip free.
  pub fn queue(&mut self, seat: Seat, what: Queued) {
    if !self.can_act(seat) {
      return;
    }
    let target = match self.target_of(what) {
      Some(tile) => tile,
      None => {
        self.refuse(seat, Refusal::NotThere);
        return;
      }
    };
    if let Some(refusal) = self.why_not(seat, what) {
      self.refuse(seat, refusal);
      return;
    }
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.queued = Some(what);
      actor.effort = 0;
      actor.refused = None;
      actor.doing = Doing::Idle;
    }
    let goal = match what {
      Queued::Take { .. } => Goal::On(target),
      _ => Goal::Beside(target),
    };
    self.set_route(seat, goal);
  }

  fn can_act(&mut self, seat: Seat) -> bool {
    match self.actors.get(&seat) {
      Some(actor) if actor.alive() => true,
      Some(_) => {
        self.refuse(seat, Refusal::Dead);
        false
      }
      None => false,
    }
  }

  fn refuse(&mut self, seat: Seat, why: Refusal) {
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.refused = Some(why);
    }
  }

  /// Where the thing a queued action names is standing.
  pub fn target_of(&self, what: Queued) -> Option<Tile> {
    match what {
      Queued::Chop { object } | Queued::Mine { object } | Queued::Fish { object } => {
        let tile = world::prop_tile(object);
        (self.prop_of(object).is_some() && self.prop_ready(object)).then_some(tile)
      }
      Queued::Cook { fire } => self.fires.get(&fire).map(|(tile, _)| *tile),
      Queued::Take { ground } => self.ground.get(&ground).map(|lying| lying.tile),
      Queued::Fight { seat } => self.actors.get(&seat).filter(|a| a.alive()).map(|a| a.tile),
    }
  }

  /// Whether an actor is allowed to do this at all, which is the level gate and
  /// the pack limit stated where a player can be told.
  fn why_not(&self, seat: Seat, what: Queued) -> Option<Refusal> {
    let actor = self.actors.get(&seat)?;
    match what {
      Queued::Chop { object } | Queued::Mine { object } | Queued::Fish { object } => {
        let prop = self.prop_of(object)?;
        let (skill, level) = prop.needs();
        if actor.level(skill) < level {
          return Some(Refusal::NeedsLevel {
            skill: skill.index() as u8,
            level,
          });
        }
        actor.pack.is_full().then_some(Refusal::PackFull)
      }
      Queued::Cook { .. } => actor
        .pack
        .find(Item::RawFish)
        .is_none()
        .then_some(Refusal::NothingToCook),
      Queued::Take { ground } => {
        let lying = self.ground.get(&ground)?;
        if lying.owner.is_some_and(|owner| owner != seat) && self.tick < lying.public_at {
          return Some(Refusal::NotYours);
        }
        actor.pack.is_full().then_some(Refusal::PackFull)
      }
      Queued::Fight { seat: other } => {
        let them = self.actors.get(&other)?;
        (!them.look.is_foe()).then_some(Refusal::NotThere)
      }
    }
  }

  pub fn drop_slot(&mut self, seat: Seat, slot: u8) {
    if !self.can_act(seat) {
      return;
    }
    let Some(actor) = self.actors.get_mut(&seat) else {
      return;
    };
    let Some(item) = actor.pack.take(slot as usize) else {
      self.refuse(seat, Refusal::PackEmpty);
      return;
    };
    actor.private_moved = true;
    let tile = actor.tile;
    self.lay_down(tile, item, Some(seat));
  }

  /// Puts an item on the ground, owned for a while and everybody's afterwards.
  pub fn lay_down(&mut self, tile: Tile, item: Item, owner: Option<Seat>) -> u32 {
    let id = self.next_ground;
    self.next_ground += 1;
    self.ground.insert(id, Dropped {
      tile,
      item,
      owner,
      public_at: self.tick + if owner.is_some() { OWNER_TICKS } else { 0 },
      gone_at: self.tick + GROUND_TICKS,
    });
    id
  }

  /// Eats a cooked fish, or sets light to logs.
  pub fn use_slot(&mut self, seat: Seat, slot: u8) {
    if !self.can_act(seat) {
      return;
    }
    let Some(actor) = self.actors.get(&seat) else {
      return;
    };
    let Some(item) = actor.pack.get(slot as usize) else {
      self.refuse(seat, Refusal::PackEmpty);
      return;
    };
    let tile = actor.tile;
    match item {
      Item::CookedFish => {
        let Some(actor) = self.actors.get_mut(&seat) else {
          return;
        };
        actor.pack.take(slot as usize);
        actor.health = (actor.health + HEAL).min(actor.max_health);
        actor.private_moved = true;
      }
      Item::Logs => {
        if self.fires.values().any(|(at, _)| *at == tile) {
          self.refuse(seat, Refusal::Busy);
          return;
        }
        let Some(actor) = self.actors.get_mut(&seat) else {
          return;
        };
        actor.pack.take(slot as usize);
        actor.private_moved = true;
        let id = self.next_fire;
        self.next_fire += 1;
        self.fires.insert(id, (tile, self.tick + FIRE_TICKS));
        // Straight into cooking, because a player who lit a fire holding fish
        // meant to cook them and being made to click again is friction with
        // nothing on the other side of it.
        if self.actors.get(&seat).is_some_and(|a| a.pack.find(Item::RawFish).is_some()) {
          if let Some(actor) = self.actors.get_mut(&seat) {
            actor.queued = Some(Queued::Cook { fire: id });
            actor.effort = 0;
          }
        }
      }
      _ => self.refuse(seat, Refusal::NotThere),
    }
  }

  pub fn cancel(&mut self, seat: Seat) {
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.route.clear();
      actor.queued = None;
      actor.effort = 0;
      actor.doing = Doing::Idle;
    }
  }

  pub fn set_running(&mut self, seat: Seat, on: bool) {
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.running = on;
    }
  }

  /// One game tick.
  pub fn advance(&mut self) {
    self.tick += 1;
    self.events.clear();

    self.depleted.retain(|_, ready_at| *ready_at > self.tick);
    self.fires.retain(|_, (_, out_at)| *out_at > self.tick);
    let now = self.tick;
    self.ground.retain(|_, lying| lying.gone_at > now);

    self.revive();
    self.think();
    self.step_everyone();
    self.work();
  }

  fn revive(&mut self) {
    let now = self.tick;
    let green = world::the_green();
    let mut moved: Vec<(Seat, Tile)> = Vec::new();
    for (seat, actor) in self.actors.iter_mut() {
      if actor.alive() || actor.up_at > now {
        continue;
      }
      actor.health = actor.max_health;
      actor.doing = Doing::Idle;
      actor.route.clear();
      actor.queued = None;
      actor.effort = 0;
      let back = if actor.is_person() { green } else { actor.home };
      actor.tile = back;
      actor.spawns += 1;
      moved.push((*seat, back));
    }
    let _ = moved;
  }

  /// Hens wander and brutes look for somebody.
  fn think(&mut self) {
    let now = self.tick;
    let mut people: Vec<(Seat, Tile)> = self
      .actors
      .iter()
      .filter(|(_, a)| a.is_person() && a.alive())
      .map(|(seat, a)| (*seat, a.tile))
      .collect();
    people.sort_unstable();

    // Sorted, and this is not tidiness. Every body below draws from one random
    // stream, so an order that came out of a hash map is an order that decides
    // who wanders where, and the same tick run twice stops being the same tick.
    let mut wandering: Vec<Seat> = self
      .actors
      .iter()
      .filter(|(_, a)| a.look.is_foe() && a.alive() && a.route.is_empty())
      .map(|(seat, _)| *seat)
      .collect();
    wandering.sort_unstable();

    for seat in wandering {
      let Some(actor) = self.actors.get(&seat) else {
        continue;
      };
      let (here, home, brute) = (actor.tile, actor.home, actor.look == Look::Brute);
      // A brute takes the nearest person inside its notice, and gives up on
      // one that has pulled it too far from its patch.
      if brute {
        let quarry = people
          .iter()
          .filter(|(_, tile)| here.steps_to(*tile) <= AGGRO && home.steps_to(*tile) <= LEASH)
          .min_by_key(|(other, tile)| (here.steps_to(*tile), *other))
          .copied();
        if let Some((quarry, _)) = quarry {
          if let Some(actor) = self.actors.get_mut(&seat) {
            actor.queued = Some(Queued::Fight { seat: quarry });
          }
          if let Some(tile) = self.target_of(Queued::Fight { seat: quarry }) {
            self.set_route(seat, Goal::Beside(tile));
          }
          continue;
        }
      }
      if self.actors.get(&seat).is_some_and(|a| a.queued.is_some()) {
        continue;
      }
      // Wandering is cheap on purpose: one square at a time, occasionally, so
      // a hundred idle bodies do not each cost a search.
      if self.rng.below(5) != 0 {
        continue;
      }
      let step = FACINGS[self.rng.below(8) as usize];
      let next = Tile::new(here.x + step.0, here.y + step.1);
      if world::walkable(next) && home.steps_to(next) <= LEASH {
        if let Some(actor) = self.actors.get_mut(&seat) {
          actor.route.push_back(next);
        }
      }
    }
    let _ = now;
  }

  fn step_everyone(&mut self) {
    let seats = self.seats();
    for seat in seats {
      let Some(actor) = self.actors.get_mut(&seat) else {
        continue;
      };
      if !actor.alive() {
        actor.doing = Doing::Dead;
        continue;
      }
      let steps = if actor.running { 2 } else { 1 };
      let mut moved = false;
      for _ in 0..steps {
        let Some(next) = actor.route.pop_front() else {
          break;
        };
        actor.facing = facing_between(actor.tile, next);
        actor.tile = next;
        moved = true;
      }
      if moved {
        actor.doing = Doing::Walking;
      } else if actor.doing == Doing::Walking {
        actor.doing = Doing::Idle;
      }
    }
  }

  /// Everybody who has arrived somewhere gets on with what they came for.
  fn work(&mut self) {
    let seats = self.seats();
    for seat in seats {
      let Some(actor) = self.actors.get(&seat) else {
        continue;
      };
      if !actor.alive() {
        continue;
      }
      let Some(what) = actor.queued else {
        continue;
      };
      if !actor.route.is_empty() {
        continue;
      }

      let Some(target) = self.target_of(what) else {
        self.give_up(seat, Refusal::NotThere);
        continue;
      };
      let here = self.actors[&seat].tile;
      let arrived = match what {
        Queued::Take { .. } => here == target,
        _ => here.is_beside(target),
      };
      if !arrived {
        // The thing moved, or the walk stopped short. One more route rather
        // than a refusal: a brute that stepped away is not a mistake.
        if !self.set_route(seat, match what {
          Queued::Take { .. } => Goal::On(target),
          _ => Goal::Beside(target),
        }) {
          self.give_up(seat, Refusal::NoRoute);
        }
        continue;
      }

      if let Some(actor) = self.actors.get_mut(&seat) {
        actor.facing = facing_between(here, target);
      }
      match what {
        Queued::Chop { object } | Queued::Mine { object } | Queued::Fish { object } => {
          self.gather(seat, object)
        }
        Queued::Cook { fire } => self.cook(seat, fire),
        Queued::Take { ground } => self.take(seat, ground),
        Queued::Fight { seat: other } => self.fight(seat, other),
      }
    }
  }

  fn give_up(&mut self, seat: Seat, why: Refusal) {
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.queued = None;
      actor.effort = 0;
      actor.doing = Doing::Idle;
      actor.refused = Some(why);
    }
  }

  fn gather(&mut self, seat: Seat, object: u32) {
    let Some(prop) = self.prop_of(object) else {
      self.give_up(seat, Refusal::NotThere);
      return;
    };
    let Some(actor) = self.actors.get_mut(&seat) else {
      return;
    };
    actor.doing = prop.doing();
    actor.effort += 1;
    if actor.effort < prop.effort() {
      return;
    }
    actor.effort = 0;
    let item = prop.yields();
    if !actor.pack.add(item) {
      self.give_up(seat, Refusal::PackFull);
      return;
    }
    actor.private_moved = true;
    let tile = actor.tile;
    let (skill, _) = prop.needs();
    let xp = prop.xp();
    let mut events = std::mem::take(&mut self.events);
    events.push((tile, Happened::Gathered { seat, item }));
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.earn(skill, xp, &mut events, seat);
    }
    self.events = events;
    self.gathered += 1;

    // A prop gives up more than once before it goes, so working one is a
    // stretch of time rather than a click. It going is what sends everybody
    // walking somewhere else, which is the only reason the still world moves.
    if self.rng.below(3) == 0 {
      self.depleted.insert(object, self.tick + prop.respawn() as u64);
      self.depletions += 1;
      self.give_up(seat, Refusal::NotThere);
      if let Some(actor) = self.actors.get_mut(&seat) {
        actor.refused = None;
        actor.doing = Doing::Idle;
      }
    } else if self.actors.get(&seat).is_some_and(|a| a.pack.is_full()) {
      self.give_up(seat, Refusal::PackFull);
    }
  }

  fn cook(&mut self, seat: Seat, fire: u32) {
    if !self.fires.contains_key(&fire) {
      self.give_up(seat, Refusal::NotThere);
      return;
    }
    let Some(actor) = self.actors.get_mut(&seat) else {
      return;
    };
    actor.doing = Doing::Cooking;
    actor.effort += 1;
    if actor.effort < 2 {
      return;
    }
    actor.effort = 0;
    let Some(slot) = actor.pack.find(Item::RawFish) else {
      self.give_up(seat, Refusal::NothingToCook);
      return;
    };
    actor.pack.replace(slot, Item::CookedFish);
    actor.private_moved = true;
    let tile = actor.tile;
    let mut events = std::mem::take(&mut self.events);
    events.push((tile, Happened::Gathered { seat, item: Item::CookedFish }));
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.earn(Skill::Cooking, skills::XP_COOKING, &mut events, seat);
    }
    self.events = events;
  }

  fn take(&mut self, seat: Seat, ground: u32) {
    let Some(lying) = self.ground.get(&ground).copied() else {
      self.give_up(seat, Refusal::NotThere);
      return;
    };
    if lying.owner.is_some_and(|owner| owner != seat) && self.tick < lying.public_at {
      self.give_up(seat, Refusal::NotYours);
      return;
    }
    let Some(actor) = self.actors.get_mut(&seat) else {
      return;
    };
    if !actor.pack.add(lying.item) {
      self.give_up(seat, Refusal::PackFull);
      return;
    }
    actor.private_moved = true;
    self.ground.remove(&ground);
    self.give_up(seat, Refusal::NotThere);
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.refused = None;
    }
  }

  fn fight(&mut self, seat: Seat, other: Seat) {
    let Some(them) = self.actors.get(&other) else {
      self.give_up(seat, Refusal::NotThere);
      return;
    };
    if !them.alive() {
      self.give_up(seat, Refusal::NotThere);
      if let Some(actor) = self.actors.get_mut(&seat) {
        actor.refused = None;
      }
      return;
    }
    let now = self.tick;
    let Some(actor) = self.actors.get_mut(&seat) else {
      return;
    };
    actor.doing = Doing::Fighting;
    if actor.swing_at > now {
      return;
    }
    actor.swing_at = now + SWING_TICKS;
    let level = actor.level(Skill::Combat) as u32;
    let tile = actor.tile;
    let damage = (1 + self.rng.below(3) + level / 6).min(u16::MAX as u32) as u16;

    let Some(them) = self.actors.get_mut(&other) else {
      return;
    };
    let dealt = damage.min(them.health);
    them.health -= dealt;
    let fell = !them.alive();
    let carcass = them.tile;
    if fell {
      them.doing = Doing::Dead;
      them.route.clear();
      them.queued = None;
      them.up_at = now + if them.is_person() { DOWN_TICKS } else { CARCASS_TICKS };
    }

    self.blows += 1;
    let mut events = std::mem::take(&mut self.events);
    events.push((tile, Happened::Hit { by: seat, on: other, damage: dealt }));
    if fell {
      events.push((carcass, Happened::Fell { seat: other }));
    }
    if let Some(actor) = self.actors.get_mut(&seat) {
      actor.earn(Skill::Combat, dealt as u32 * skills::XP_PER_DAMAGE, &mut events, seat);
    }
    self.events = events;

    if fell {
      self.falls += 1;
      if self.actors.get(&other).is_some_and(|a| a.look.is_foe()) {
        self.lay_down(carcass, Item::Bones, Some(seat));
      }
      self.give_up(seat, Refusal::NotThere);
      if let Some(actor) = self.actors.get_mut(&seat) {
        actor.refused = None;
      }
    }
  }

  /// Props in view that are out, as the wire says them.
  pub fn depleted_in_view(&self, middle: Tile, into: &mut Vec<ObjectState>) {
    into.clear();
    for (id, ready_at) in &self.depleted {
      let tile = world::prop_tile(*id);
      if middle.steps_to(tile) <= VIEW {
        into.push(ObjectState {
          id: *id,
          ready_at: *ready_at as u32,
        });
      }
    }
    // Sorted, because a frame whose order came from a hash map is a frame that
    // differs from itself for no reason a reader could name.
    into.sort_unstable_by_key(|state| state.id);
  }

  pub fn fires_in_view(&self, middle: Tile) -> Vec<Fire> {
    let mut fires: Vec<Fire> = self
      .fires
      .iter()
      .filter(|(_, (tile, _))| middle.steps_to(*tile) <= VIEW)
      .map(|(id, (tile, out_at))| Fire {
        id: *id,
        tile: *tile,
        out_at: *out_at as u32,
      })
      .collect();
    fires.sort_unstable_by_key(|fire| fire.id);
    fires
  }

  /// Items on the ground this viewer may be told about.
  ///
  /// The audience is a rule rather than a distance: within sight **and** either
  /// yours or nobody's yet. A viewer who cannot take it is not told it is
  /// there, which is the whole of what makes the ownership timer a thing that
  /// happens rather than a label.
  pub fn ground_in_view(&self, middle: Tile, viewer: Seat) -> Vec<Lying> {
    let mut lying: Vec<Lying> = self
      .ground
      .iter()
      .filter(|(_, item)| middle.steps_to(item.tile) <= VIEW)
      .filter(|(_, item)| item.owner.is_none_or(|owner| owner == viewer) || self.tick >= item.public_at)
      .map(|(id, item)| Lying {
        id: *id,
        tile: item.tile,
        item: item.item,
        yours: item.owner == Some(viewer) && self.tick < item.public_at,
        public_in: item.public_at.saturating_sub(self.tick).min(u16::MAX as u64) as u16,
      })
      .collect();
    lying.sort_unstable_by_key(|item| item.id);
    lying
  }

  /// How many props are standing in the world at all, for the panel that has to
  /// say what the still half costs against what it could have cost.
  pub fn props_in_view(&self, middle: Tile) -> usize {
    let mut count = 0;
    for y in (middle.y as i32 - VIEW)..=(middle.y as i32 + VIEW) {
      for x in (middle.x as i32 - VIEW)..=(middle.x as i32 + VIEW) {
        if world::prop_at(Tile::new(x as i16, y as i16)).is_some() {
          count += 1;
        }
      }
    }
    count
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn a_person(zone: &mut Zone, seat: Seat) -> Tile {
    let tile = world::footing_near(Tile::new(40 + seat as i16 * 3, 40));
    zone.admit(seat, tile, Look::Person);
    tile
  }

  fn nearest_prop(from: Tile, want: Prop) -> Tile {
    let mut best: Option<(i32, Tile)> = None;
    for y in 0..world::SIZE {
      for x in 0..world::SIZE {
        let tile = Tile::new(x, y);
        if world::prop_at(tile) != Some(want) {
          continue;
        }
        let distance = from.steps_to(tile);
        if best.is_none_or(|(d, _)| distance < d) {
          best = Some((distance, tile));
        }
      }
    }
    best.expect("no prop of that kind in the world").1
  }

  /// Runs the world until a condition holds, or gives up loudly.
  fn until(zone: &mut Zone, ticks: u64, mut done: impl FnMut(&Zone) -> bool) -> u64 {
    for elapsed in 1..=ticks {
      zone.advance();
      if done(zone) {
        return elapsed;
      }
    }
    panic!("nothing happened in {ticks} ticks");
  }

  #[test]
  fn a_destination_is_walked_to_one_square_at_a_time() {
    let mut zone = Zone::new();
    let from = a_person(&mut zone, 1);
    let to = world::footing_near(Tile::new(from.x + 9, from.y + 4));
    zone.walk_to(1, to);
    assert!(!zone.actors[&1].route.is_empty(), "a click produced no route");

    let took = until(&mut zone, 60, |z| z.actors[&1].tile == to);
    assert!(took >= 5, "arrived in {took} ticks, which is not walking");
    zone.advance();
    assert_eq!(zone.actors[&1].doing, Doing::Idle, "still walking a tick after arriving");
  }

  #[test]
  fn running_covers_two_squares_a_tick() {
    let mut zone = Zone::new();
    let from = a_person(&mut zone, 1);
    let to = world::footing_near(Tile::new(from.x + 14, from.y));
    zone.set_running(1, true);
    zone.walk_to(1, to);
    let running = until(&mut zone, 80, |z| z.actors[&1].tile == to);

    let mut walking_zone = Zone::new();
    let from = a_person(&mut walking_zone, 1);
    let to = world::footing_near(Tile::new(from.x + 14, from.y));
    walking_zone.walk_to(1, to);
    let walking = until(&mut walking_zone, 80, |z| z.actors[&1].tile == to);

    assert!(running < walking, "running took {running} against walking {walking}");
  }

  #[test]
  fn a_queued_action_walks_there_first_and_then_starts() {
    // The claim about latency, as a rule rather than as prose: the interaction
    // does not begin until the walk is over, so the round trip had seconds to
    // arrive in.
    let mut zone = Zone::new();
    let from = a_person(&mut zone, 1);
    let tree = nearest_prop(from, Prop::Tree);
    zone.queue(1, Queued::Chop { object: world::prop_id(tree) });
    assert_ne!(zone.actors[&1].doing, Doing::Chopping, "began before walking there");

    let walking = until(&mut zone, 200, |z| z.actors[&1].doing == Doing::Chopping);
    assert!(walking > 1, "chopping began on the tick it was asked for");
    assert!(zone.actors[&1].tile.is_beside(tree), "chopping from the wrong square");
  }

  #[test]
  fn chopping_fills_a_pack_and_pays_experience() {
    let mut zone = Zone::new();
    let from = a_person(&mut zone, 1);
    let tree = nearest_prop(from, Prop::Tree);
    zone.actors.get_mut(&1).unwrap().tile = world::footing_near(Tile::new(tree.x + 1, tree.y));
    zone.queue(1, Queued::Chop { object: world::prop_id(tree) });

    until(&mut zone, 200, |z| z.actors[&1].pack.count_of(Item::Logs) > 0);
    assert!(zone.actors[&1].xp[Skill::Woodcutting.index()] > 0, "no experience");
    assert!(
      zone.events.iter().any(|(_, e)| matches!(e, Happened::Gathered { .. })),
      "nothing said it had happened"
    );
  }

  #[test]
  fn a_level_gate_refuses_and_says_which_one() {
    // Guide 40's argument in the game's own vocabulary. A refusal a player
    // cannot read is indistinguishable from a broken key.
    let mut zone = Zone::new();
    let from = a_person(&mut zone, 1);
    let oak = nearest_prop(from, Prop::Oak);
    zone.queue(1, Queued::Chop { object: world::prop_id(oak) });
    assert_eq!(
      zone.actors[&1].refused,
      Some(Refusal::NeedsLevel {
        skill: Skill::Woodcutting.index() as u8,
        level: 8
      })
    );
    assert!(zone.actors[&1].queued.is_none(), "the refusal did not stop the walk");

    zone.actors.get_mut(&1).unwrap().xp[Skill::Woodcutting.index()] = skills::xp_for(8);
    zone.queue(1, Queued::Chop { object: world::prop_id(oak) });
    assert_eq!(zone.actors[&1].refused, None);
    assert!(zone.actors[&1].queued.is_some());
  }

  #[test]
  fn a_depleted_prop_comes_back_on_the_tick_it_said_it_would() {
    // Absolute rather than counted down, which is the whole reason a viewer
    // can be told once instead of every tick.
    let mut zone = Zone::new();
    let tile = nearest_prop(world::the_green(), Prop::Tree);
    let id = world::prop_id(tile);
    zone.depleted.insert(id, 10);
    assert!(!zone.prop_ready(id));
    until(&mut zone, 20, |z| z.prop_ready(id));
    assert_eq!(zone.tick, 10, "it came back on tick {}", zone.tick);
  }

  #[test]
  fn a_dropped_item_is_yours_and_then_everybodys() {
    // The audience decided by a rule rather than by a distance, which is the
    // thing neither relevance nor subscription expresses.
    let mut zone = Zone::new();
    let mine = a_person(&mut zone, 1);
    zone.admit(2, Tile::new(mine.x + 1, mine.y), Look::Person);
    zone.actors.get_mut(&1).unwrap().pack.add(Item::Logs);
    zone.drop_slot(1, 0);

    assert_eq!(zone.ground_in_view(mine, 1).len(), 1, "the owner cannot see their own drop");
    assert!(zone.ground_in_view(mine, 1)[0].yours);
    assert!(
      zone.ground_in_view(mine, 2).is_empty(),
      "somebody else was told about an item they may not take"
    );

    for _ in 0..OWNER_TICKS {
      zone.advance();
    }
    assert_eq!(zone.ground_in_view(mine, 2).len(), 1, "it never became everybody's");
    assert!(!zone.ground_in_view(mine, 1)[0].yours);
  }

  #[test]
  fn taking_something_that_is_not_yours_is_refused_by_name() {
    let mut zone = Zone::new();
    let mine = a_person(&mut zone, 1);
    zone.admit(2, Tile::new(mine.x + 1, mine.y), Look::Person);
    zone.actors.get_mut(&1).unwrap().pack.add(Item::Ore);
    zone.drop_slot(1, 0);
    let id = *zone.ground.keys().next().unwrap();

    zone.queue(2, Queued::Take { ground: id });
    assert_eq!(zone.actors[&2].refused, Some(Refusal::NotYours));
    assert_eq!(zone.actors[&2].queued, None);
  }

  #[test]
  fn the_loop_closes() {
    // Chop, light, catch, cook, eat. Every piece of content in this world sits
    // on that circle, which is the discipline that keeps it from growing.
    let mut zone = Zone::new();
    let tile = a_person(&mut zone, 1);
    {
      let actor = zone.actors.get_mut(&1).unwrap();
      actor.pack.add(Item::Logs);
      actor.pack.add(Item::RawFish);
      actor.health = 10;
    }
    zone.use_slot(1, 0);
    assert_eq!(zone.fires.len(), 1, "the logs did not light");
    assert!(matches!(zone.actors[&1].queued, Some(Queued::Cook { .. })), "did not start cooking");

    until(&mut zone, 20, |z| z.actors[&1].pack.count_of(Item::CookedFish) > 0);
    assert!(zone.actors[&1].xp[Skill::Cooking.index()] > 0);

    let slot = zone.actors[&1].pack.find(Item::CookedFish).unwrap();
    zone.use_slot(1, slot as u8);
    assert_eq!(zone.actors[&1].health, 10 + HEAL, "eating did not heal");
    let _ = tile;
  }

  #[test]
  fn a_brute_notices_you_and_gives_up_when_you_leave() {
    let mut zone = Zone::new();
    let tile = a_person(&mut zone, 1);
    let patch = world::footing_near(Tile::new(tile.x + 2, tile.y + 1));
    zone.admit(9, patch, Look::Brute);

    until(&mut zone, 40, |z| z.actors[&1].health < PERSON_HEALTH);
    assert!(
      zone.events.iter().any(|(_, e)| matches!(e, Happened::Hit { .. })) || zone.blows > 0,
      "nothing was said about being hit"
    );
  }

  #[test]
  fn a_fallen_person_comes_back_at_the_green_and_is_told_they_moved() {
    // The client owns nothing about its position here, but it does draw one,
    // and a respawn is the one time a square arrives rather than departs.
    let mut zone = Zone::new();
    a_person(&mut zone, 1);
    let before = zone.actors[&1].spawns;
    {
      let actor = zone.actors.get_mut(&1).unwrap();
      actor.health = 0;
      actor.up_at = 3;
    }
    until(&mut zone, 20, |z| z.actors[&1].alive());
    assert_eq!(zone.actors[&1].tile, world::the_green());
    assert!(zone.actors[&1].spawns > before, "the client is never told it moved");
  }

  #[test]
  fn a_fallen_brute_leaves_bones_for_whoever_felled_it() {
    let mut zone = Zone::new();
    let tile = a_person(&mut zone, 1);
    let patch = world::footing_near(Tile::new(tile.x + 1, tile.y));
    zone.admit(9, patch, Look::Brute);
    zone.actors.get_mut(&9).unwrap().health = 1;
    zone.queue(1, Queued::Fight { seat: 9 });

    until(&mut zone, 60, |z| !z.actors[&9].alive());
    let bones: Vec<_> = zone.ground.values().filter(|d| d.item == Item::Bones).collect();
    assert_eq!(bones.len(), 1, "no bones");
    assert_eq!(bones[0].owner, Some(1), "somebody else's kill");
  }

  #[test]
  fn what_the_still_half_of_the_world_costs() {
    // The number the whole relevance argument turns on: how many props a
    // viewer can see against how many of them are ever out at once.
    let mut zone = Zone::new();
    let middle = world::the_green();
    let in_view = zone.props_in_view(middle);

    let mut ids: Vec<u32> = Vec::new();
    for y in (middle.y as i32 - VIEW)..=(middle.y as i32 + VIEW) {
      for x in (middle.x as i32 - VIEW)..=(middle.x as i32 + VIEW) {
        let tile = Tile::new(x as i16, y as i16);
        if world::prop_at(tile).is_some() {
          ids.push(world::prop_id(tile));
        }
      }
    }
    for id in ids.iter().take(6) {
      zone.depleted.insert(*id, 500);
    }
    let mut out = Vec::new();
    zone.depleted_in_view(middle, &mut out);

    println!("\n  {in_view} props inside a {VIEW}-square view, {} of them out\n", out.len());
    assert!(in_view > 100, "only {in_view} props in view, which is not a still world");
    assert_eq!(out.len(), 6);
    assert!(
      out.len() * 20 < in_view,
      "if most of the world is always out, sending all of it is not the wrong answer"
    );
  }

  #[test]
  fn a_tick_is_the_same_tick_when_it_is_run_again() {
    // Nothing here is replayed over the wire, but a world whose own randomness
    // came from a hash map's order is a world nobody can reason about.
    let run = |ticks: u64| {
      let mut zone = Zone::new();
      let tile = a_person(&mut zone, 1);
      for seat in 0..6u16 {
        let patch = world::footing_near(Tile::new(tile.x + 4 + seat as i16, tile.y + 3));
        zone.admit(20 + seat, patch, Look::Hen);
      }
      for _ in 0..ticks {
        zone.advance();
      }
      let mut where_they_are: Vec<(Seat, Tile)> =
        zone.actors.iter().map(|(seat, a)| (*seat, a.tile)).collect();
      where_they_are.sort_unstable();
      where_they_are
    };
    assert_eq!(run(40), run(40));
  }
}
