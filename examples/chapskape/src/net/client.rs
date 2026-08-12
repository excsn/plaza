//! A client that answers its own clicks.
//!
//! Nothing here is a prediction of a *simulation*. The client runs the same
//! pathfinder over the same derived map, so what it has after a click is not a
//! guess at the server's answer, it is the answer. It starts walking on the
//! frame the mouse went down and the server starts on its next tick, and the
//! two are then a tick and a round trip out of phase rather than in
//! disagreement.
//!
//! Which is why the check here is a **route** check rather than a position
//! check. Asking whether the client and the server are on the same square right
//! now would count the phase offset as an error. Asking whether the server is
//! walking the squares the client drew is the question that has a right answer,
//! and its answer should be yes every time. When it is not, something has
//! diverged and the counter on the panel is the only thing that would say so.
//!
//! The other asymmetry worth naming is in `objects`. Under
//! [`Relevance::EveryTick`] a frame is the whole visible set, so absence means
//! a prop is back. Under [`Relevance::OnChange`] absence means nothing
//! happened, and a prop coming back has to be said out loud. Two modes, two
//! pieces of client code, and that is what the cheaper one costs.

use std::collections::{HashMap, VecDeque};

use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::path::{Goal, Pathfinder};
use crate::protocol::{
  Doing, Fire, Frame, Happened, Item, Look, Lying, Queued, Refusal, Relevance, Seat, SkapeOp, Tile,
  You, PROTOCOL, TICK_MS,
};

const WIRE: MsgPackCodec = MsgPackCodec;
const WINDOW_MS: u64 = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

/// Bytes over a rolling window, in the unit the other examples report.
#[derive(Default)]
pub struct Meter {
  recent: VecDeque<(u64, usize)>,
  pub total_bytes: u64,
  pub frames: u64,
  since_ms: Option<u64>,
}

impl Meter {
  pub fn record(&mut self, now_ms: u64, bytes: usize) {
    self.recent.push_back((now_ms, bytes));
    self.total_bytes += bytes as u64;
    self.frames += 1;
    self.since_ms.get_or_insert(now_ms);
    while let Some((at, _)) = self.recent.front() {
      if now_ms.saturating_sub(*at) > WINDOW_MS {
        self.recent.pop_front();
      } else {
        break;
      }
    }
  }

  pub fn kib_per_sec(&self, now_ms: u64) -> f32 {
    let bytes: usize = self
      .recent
      .iter()
      .filter(|(at, _)| now_ms.saturating_sub(*at) <= WINDOW_MS)
      .map(|(_, b)| b)
      .sum();
    bytes as f32 * 1000.0 / WINDOW_MS as f32 / 1024.0
  }

  pub fn session_kib_per_sec(&self, now_ms: u64) -> f32 {
    let Some(since) = self.since_ms else {
      return 0.0;
    };
    let elapsed = now_ms.saturating_sub(since).max(1) as f32 / 1000.0;
    self.total_bytes as f32 / elapsed / 1024.0
  }
}

/// Somebody else, and the two squares needed to draw them between ticks.
///
/// At a tick this long, interpolation is not a refinement, it is the entire
/// visual experience: without it every body in the world teleports once every
/// six hundred milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Other {
  pub seat: Seat,
  pub tile: Tile,
  pub was: Tile,
  pub look: Look,
  pub doing: Doing,
  pub health: u16,
  pub max_health: u16,
  pub facing: u8,
  pub since_ms: u64,
}

impl Other {
  /// Where to draw them, in squares, eased across the gap between ticks.
  pub fn drawn_at(&self, now_ms: u64, tick_ms: u64) -> (f32, f32) {
    let t = (now_ms.saturating_sub(self.since_ms) as f32 / tick_ms.max(1) as f32).clamp(0.0, 1.0);
    (
      self.was.x as f32 + (self.tile.x - self.was.x) as f32 * t,
      self.was.y as f32 + (self.tile.y - self.was.y) as f32 * t,
    )
  }

  pub fn moving(&self) -> bool {
    self.was != self.tile
  }
}

/// A number that floated off somebody.
#[derive(Clone, Copy, Debug)]
pub struct Splat {
  pub at: Tile,
  pub amount: i32,
  pub since_ms: u64,
  pub mine: bool,
}

impl Splat {
  pub const LIFE_MS: u64 = 1200;

  pub fn age(&self, now_ms: u64) -> f32 {
    (now_ms.saturating_sub(self.since_ms) as f32 / Self::LIFE_MS as f32).clamp(0.0, 1.0)
  }
}

/// A line of text that arrived and will not be said again.
#[derive(Clone, Debug)]
pub struct Notice {
  pub text: String,
  pub since_ms: u64,
  pub loud: bool,
}

impl Notice {
  pub const LIFE_MS: u64 = 2600;
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub seat: Option<Seat>,

  /// Where this client believes it is, having walked there itself.
  pub predicted: Tile,
  /// Where the server last said it was.
  pub confirmed: Tile,
  /// Squares left of the route the client is walking on its own clock.
  pub plan: VecDeque<Tile>,
  /// The same squares again, spent against what the server confirms.
  expect: VecDeque<Tile>,
  /// Whether the current journey is one the client claimed to have worked out.
  ///
  /// A walk and a walk-then-work are: the destination is a square that will
  /// still be there. A chase is not, because the server re-routes toward a body
  /// that keeps moving and the client never claimed to predict where it went.
  /// Counting those as divergence would bury the reading under the one
  /// situation the client was never answering.
  checking: bool,
  /// Where the whole journey ends, for redrawing the route after a surprise.
  goal: Option<Goal>,
  finder: Pathfinder,
  /// When the client next moves itself along its plan.
  next_step_ms: u64,
  /// When the last local step happened, for drawing between squares.
  stepped_ms: u64,
  was: Tile,

  pub seeded: bool,
  pub others: HashMap<Seat, Other>,
  pub you: Option<You>,
  pub pack: Vec<Option<Item>>,
  pub xp: Vec<u32>,
  spawn_seen: u32,

  /// The client's model of the still world: prop id against the tick it
  /// returns on.
  pub objects: HashMap<u32, u32>,
  pub fires: HashMap<u32, Fire>,
  pub ground: Vec<Lying>,

  pub tick: u64,
  pub tick_ms: u64,
  pub mode: Relevance,
  pub meter: Meter,

  /// Frames whose confirmed square was not the next square the client drew.
  ///
  /// Zero is the expected reading, which is exactly what makes it worth
  /// showing: a route both ends derive from one rule has nothing to disagree
  /// about, and a number climbing here means the rule stopped being one rule.
  pub diverged: u64,
  pub confirmations: u64,
  /// Ops this client has sent, for the comparison with a held direction.
  pub ops_sent: u64,
  pub since_first_op_ms: Option<u64>,

  pub splats: Vec<Splat>,
  pub notices: Vec<Notice>,
  /// When each seat last landed a blow, so a swing can be drawn.
  pub swings: HashMap<Seat, u64>,
  pub levels: HashMap<u8, u8>,

  now_ms: u64,
  events: Vec<Event>,
  arrivals: Vec<Arrival>,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(
      FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?,
    ))
  }

  pub fn from_socket(socket: Box<dyn plaza_ws::Socket>) -> Self {
    Self::from_pump(FramePump::new(socket, WIRE, PROTOCOL))
  }

  fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      status: Status::Connecting,
      seat: None,
      predicted: Tile::default(),
      confirmed: Tile::default(),
      plan: VecDeque::new(),
      expect: VecDeque::new(),
      checking: false,
      goal: None,
      finder: Pathfinder::new(),
      next_step_ms: 0,
      stepped_ms: 0,
      was: Tile::default(),
      seeded: false,
      others: HashMap::new(),
      you: None,
      pack: vec![None; crate::pack::SLOTS],
      xp: vec![0; crate::skills::SKILLS],
      spawn_seen: 0,
      objects: HashMap::new(),
      fires: HashMap::new(),
      ground: Vec::new(),
      tick: 0,
      tick_ms: TICK_MS,
      mode: Relevance::default(),
      meter: Meter::default(),
      diverged: 0,
      confirmations: 0,
      ops_sent: 0,
      since_first_op_ms: None,
      splats: Vec::new(),
      notices: Vec::new(),
      swings: HashMap::new(),
      levels: HashMap::new(),
      now_ms: 0,
      events: Vec::new(),
      arrivals: Vec::new(),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.now_ms
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  pub fn ready(&self) -> bool {
    self.seat.is_some() && self.seeded
  }

  /// Ops a minute, which is the number this whole example is about.
  pub fn ops_per_minute(&self) -> f32 {
    let Some(since) = self.since_first_op_ms else {
      return 0.0;
    };
    let minutes = self.now_ms.saturating_sub(since).max(1) as f32 / 60_000.0;
    self.ops_sent as f32 / minutes
  }

  pub fn poll(&mut self, now_ms: u64) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Joined;
          }
        }
        Arrival::Ops(frame) => {
          self.meter.record(now_ms, frame.body().len());
          self.on_ops(frame.body());
        }
        Arrival::Mismatch { ours, theirs } => {
          self.status = Status::Gone(mismatch_message(ours, theirs))
        }
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;

    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
    self.walk_myself(now_ms);
    self.forget_what_is_over(now_ms);
  }

  /// Moves the local body along its own plan, on its own clock.
  ///
  /// This is the whole of what prediction means here. There is no simulation to
  /// re-run and no correction to blend: the route was never in doubt, only its
  /// timing was, and the timing is a tick and a round trip that nobody can see
  /// because the walk is seconds long.
  fn walk_myself(&mut self, now_ms: u64) {
    if !self.ready() || self.plan.is_empty() {
      return;
    }
    let steps = if self.you.as_ref().is_some_and(|you| you.running) { 2 } else { 1 };
    while now_ms >= self.next_step_ms && !self.plan.is_empty() {
      self.was = self.predicted;
      for _ in 0..steps {
        if let Some(next) = self.plan.pop_front() {
          self.predicted = next;
        }
      }
      self.stepped_ms = now_ms;
      self.next_step_ms = now_ms + self.tick_ms;
    }
  }

  /// Where the local body is drawn, in squares, between one square and the next.
  pub fn drawn_at(&self, now_ms: u64) -> (f32, f32) {
    let t = (now_ms.saturating_sub(self.stepped_ms) as f32 / self.tick_ms.max(1) as f32)
      .clamp(0.0, 1.0);
    (
      self.was.x as f32 + (self.predicted.x - self.was.x) as f32 * t,
      self.was.y as f32 + (self.predicted.y - self.was.y) as f32 * t,
    )
  }

  pub fn walking(&self) -> bool {
    !self.plan.is_empty() || self.was != self.predicted
  }

  pub fn facing(&self) -> u8 {
    if self.was == self.predicted {
      return self.you.as_ref().map(|you| you.facing).unwrap_or(4);
    }
    crate::zone::facing_between(self.was, self.predicted)
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<SkapeOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        SkapeOp::Seated { seat, tile } => {
          self.seat = Some(seat);
          self.jump_to(tile);
          self.seeded = true;
        }
        SkapeOp::World(frame) => self.on_frame(*frame),
        SkapeOp::WalkTo { .. }
        | SkapeOp::Interact { .. }
        | SkapeOp::Attack { .. }
        | SkapeOp::Take { .. }
        | SkapeOp::Drop { .. }
        | SkapeOp::Use { .. }
        | SkapeOp::Run { .. }
        | SkapeOp::Cancel => {}
      }
    }
  }

  fn jump_to(&mut self, tile: Tile) {
    self.predicted = tile;
    self.confirmed = tile;
    self.was = tile;
    self.plan.clear();
    self.expect.clear();
    self.goal = None;
    self.stepped_ms = self.now_ms;
    self.next_step_ms = self.now_ms + self.tick_ms;
  }

  fn on_frame(&mut self, frame: Frame) {
    self.tick = frame.tick;
    self.tick_ms = frame.tick_ms.max(1) as u64;
    self.mode = frame.mode;
    let now = self.now_ms;

    self.apply_objects(&frame);
    self.fires = frame.fires.into_iter().map(|fire| (fire.id, fire)).collect();
    self.ground = frame.ground;

    for seen in &frame.actors {
      self
        .others
        .entry(seen.seat)
        .and_modify(|other| {
          if other.tile != seen.tile {
            other.was = other.tile;
            other.since_ms = now;
          }
          other.tile = seen.tile;
          other.look = seen.look;
          other.doing = seen.doing;
          other.health = seen.health;
          other.max_health = seen.max_health;
          other.facing = seen.facing;
        })
        .or_insert(Other {
          seat: seen.seat,
          tile: seen.tile,
          was: seen.tile,
          look: seen.look,
          doing: seen.doing,
          health: seen.health,
          max_health: seen.max_health,
          facing: seen.facing,
          since_ms: now,
        });
    }
    // A frame is the whole audience, not a delta, so absence means out of it.
    let present: std::collections::HashSet<Seat> = frame.actors.iter().map(|a| a.seat).collect();
    self.others.retain(|seat, _| present.contains(seat));

    for happened in &frame.events {
      self.on_happened(*happened);
    }

    if let Some(you) = frame.you {
      self.on_you(*you);
    }
  }

  fn apply_objects(&mut self, frame: &Frame) {
    match frame.mode {
      // The whole visible set, so what is not in it is standing again.
      Relevance::EveryTick => {
        self.objects.clear();
        for state in &frame.objects {
          self.objects.insert(state.id, state.ready_at);
        }
      }
      // A difference, so silence means nothing happened and a zero is the only
      // thing that can say a prop came back.
      Relevance::OnChange => {
        for state in &frame.objects {
          if state.ready_at == 0 {
            self.objects.remove(&state.id);
          } else {
            self.objects.insert(state.id, state.ready_at);
          }
        }
        self.objects.retain(|_, ready_at| *ready_at as u64 > frame.tick);
      }
    }
  }

  fn on_you(&mut self, you: You) {
    let now = self.now_ms;
    if you.spawn != self.spawn_seen {
      // The one square that arrives rather than departs. A body that was put
      // somewhere it did not walk to has to be told, and told once.
      self.spawn_seen = you.spawn;
      self.jump_to(you.tile);
      self.you = Some(you);
      return;
    }

    if let Some(before) = self.you.as_ref() {
      if before.health > you.health {
        self.splats.push(Splat {
          at: you.tile,
          amount: (before.health - you.health) as i32,
          since_ms: now,
          mine: true,
        });
      } else if you.health > before.health {
        self.splats.push(Splat {
          at: you.tile,
          amount: -((you.health - before.health) as i32),
          since_ms: now,
          mine: true,
        });
      }
    }

    if let Some(refusal) = you.refused {
      self.notices.push(Notice {
        text: say_refusal(refusal),
        since_ms: now,
        loud: false,
      });
      if refusal == Refusal::NoRoute {
        self.plan.clear();
        self.expect.clear();
        self.checking = false;
        self.goal = None;
        self.predicted = you.tile;
        self.was = you.tile;
      }
    }

    self.confirm(you.tile);
    if let Some(private) = you.private.clone() {
      self.pack = private.pack;
      self.xp = private.xp;
    }
    self.you = Some(you);
  }

  /// Checks the server against the route this client drew.
  ///
  /// Not against the client's current square: the two are a tick out of phase
  /// by design, and counting that as an error would bury the thing this is for.
  fn confirm(&mut self, tile: Tile) {
    if tile == self.confirmed {
      return;
    }
    if !self.checking {
      return self.follow(tile);
    }
    self.confirmations += 1;
    // Two, because running covers two squares in a tick and the first of them
    // is a square nothing ever reports.
    for _ in 0..2 {
      match self.expect.pop_front() {
        Some(next) if next == tile => {
          self.confirmed = tile;
          return;
        }
        Some(_) => continue,
        None => {
          // The route this client drew is spent and the body is still moving,
          // which means the server is steering it rather than walking a
          // journey that was asked for. Follow it; there is nothing left to
          // check against.
          self.checking = false;
          return self.follow(tile);
        }
      }
    }
    self.diverged += 1;
    self.notices.push(Notice {
      text: "the world walked a different way".to_owned(),
      since_ms: self.now_ms,
      loud: true,
    });
    self.checking = false;
    self.confirmed = tile;
    self.jump_to(tile);
  }

  /// Takes a square the server moved the body to, and eases into it.
  ///
  /// Not a correction and not a snap. It is the ordinary case for anything the
  /// server steers, and drawing it as one step from the last square is what
  /// keeps a chase from reading as a teleport once a tick.
  fn follow(&mut self, tile: Tile) {
    self.was = self.predicted;
    self.predicted = tile;
    self.confirmed = tile;
    self.plan.clear();
    self.expect.clear();
    self.stepped_ms = self.now_ms;
    self.next_step_ms = self.now_ms + self.tick_ms;
  }

  fn on_happened(&mut self, happened: Happened) {
    let now = self.now_ms;
    match happened {
      Happened::Hit { by, on, damage } => {
        self.swings.insert(by, now);
        let at = self
          .others
          .get(&on)
          .map(|other| other.tile)
          .unwrap_or(self.predicted);
        self.splats.push(Splat {
          at,
          amount: damage as i32,
          since_ms: now,
          mine: Some(on) == self.seat,
        });
      }
      Happened::Fell { .. } => {}
      Happened::Gathered { seat, item } => {
        if Some(seat) == self.seat {
          self.notices.push(Notice {
            text: format!("you get some {}", item.name()),
            since_ms: now,
            loud: false,
          });
        }
      }
      Happened::Earned { skill, amount } => {
        if let Some(name) = crate::skills::Skill::from_index(skill as usize) {
          self.notices.push(Notice {
            text: format!("+{amount} {}", name.name()),
            since_ms: now,
            loud: false,
          });
        }
      }
      Happened::Levelled { skill, level } => {
        self.levels.insert(skill, level);
        if let Some(name) = crate::skills::Skill::from_index(skill as usize) {
          self.notices.push(Notice {
            text: format!("{} level {level}", name.name()),
            since_ms: now,
            loud: true,
          });
        }
      }
    }
  }

  fn forget_what_is_over(&mut self, now_ms: u64) {
    self.splats.retain(|s| now_ms.saturating_sub(s.since_ms) < Splat::LIFE_MS);
    self
      .notices
      .retain(|n| now_ms.saturating_sub(n.since_ms) < Notice::LIFE_MS);
    self.swings.retain(|_, at| now_ms.saturating_sub(*at) < 700);
    if self.notices.len() > 6 {
      let extra = self.notices.len() - 6;
      self.notices.drain(0..extra);
    }
  }

  /// Draws the route locally and then asks for it.
  ///
  /// In that order, and the order is the point: the body is already walking
  /// before the op has left the machine.
  fn set_out(&mut self, goal: Goal, checking: bool) {
    let route = self.finder.route(self.predicted, goal);
    self.goal = Some(goal);
    self.checking = checking;
    self.plan = route.iter().copied().collect();
    self.expect = route.into_iter().collect();
    self.next_step_ms = self.now_ms;
    self.stepped_ms = self.now_ms;
    self.was = self.predicted;
  }

  fn count_op(&mut self) {
    self.ops_sent += 1;
    self.since_first_op_ms.get_or_insert(self.now_ms);
  }

  pub fn walk_to(&mut self, tile: Tile) {
    if !crate::world::walkable(tile) {
      self.notices.push(Notice {
        text: "you cannot reach that".to_owned(),
        since_ms: self.now_ms,
        loud: false,
      });
      return;
    }
    self.set_out(Goal::On(tile), true);
    self.count_op();
    self.pump.send_op(&SkapeOp::WalkTo { tile });
  }

  pub fn interact(&mut self, object: u32) {
    let tile = if object >= crate::world::FIRE_BASE {
      self.fires.get(&object).map(|fire| fire.tile)
    } else {
      Some(crate::world::prop_tile(object))
    };
    if let Some(tile) = tile {
      self.set_out(Goal::Beside(tile), true);
    }
    self.count_op();
    self.pump.send_op(&SkapeOp::Interact { object });
  }

  pub fn attack(&mut self, seat: Seat) {
    if let Some(other) = self.others.get(&seat) {
      let tile = other.tile;
      self.set_out(Goal::Beside(tile), false);
    }
    self.count_op();
    self.pump.send_op(&SkapeOp::Attack { seat });
  }

  pub fn take(&mut self, ground: u32) {
    if let Some(lying) = self.ground.iter().find(|l| l.id == ground) {
      let tile = lying.tile;
      self.set_out(Goal::On(tile), true);
    }
    self.count_op();
    self.pump.send_op(&SkapeOp::Take { ground });
  }

  pub fn drop_slot(&mut self, slot: u8) {
    self.count_op();
    self.pump.send_op(&SkapeOp::Drop { slot });
  }

  pub fn use_slot(&mut self, slot: u8) {
    self.count_op();
    self.pump.send_op(&SkapeOp::Use { slot });
  }

  pub fn set_running(&mut self, on: bool) {
    self.count_op();
    self.pump.send_op(&SkapeOp::Run { on });
  }

  pub fn cancel(&mut self) {
    self.plan.clear();
    self.expect.clear();
    self.checking = false;
    self.goal = None;
    self.count_op();
    self.pump.send_op(&SkapeOp::Cancel);
  }

  /// Whether a prop is standing, as far as this client has been told.
  pub fn prop_standing(&self, id: u32) -> bool {
    !self.objects.contains_key(&id)
  }

  pub fn queued(&self) -> Option<Queued> {
    self.you.as_ref().and_then(|you| you.queued)
  }

  pub fn is_down(&self) -> bool {
    self.you.as_ref().is_some_and(|you| you.up_in.is_some())
  }

  pub fn level_of(&self, skill: crate::skills::Skill) -> u8 {
    crate::skills::level_for(self.xp.get(skill.index()).copied().unwrap_or(0))
  }

  pub fn swinging(&self, seat: Seat, now_ms: u64) -> f32 {
    self
      .swings
      .get(&seat)
      .map(|at| (now_ms.saturating_sub(*at) as f32 / 500.0).clamp(0.0, 1.0))
      .filter(|share| *share < 1.0)
      .unwrap_or(0.0)
  }
}

pub fn say_refusal(refusal: Refusal) -> String {
  match refusal {
    Refusal::NoRoute => "you cannot get there".to_owned(),
    Refusal::PackFull => "your pack is full".to_owned(),
    Refusal::PackEmpty => "there is nothing there".to_owned(),
    Refusal::NeedsLevel { skill, level } => match crate::skills::Skill::from_index(skill as usize) {
      Some(skill) => format!("you need {} level {level}", skill.name()),
      None => format!("you need level {level}"),
    },
    Refusal::NotThere => "it is not there any more".to_owned(),
    Refusal::NotYours => "that is not yours yet".to_owned(),
    Refusal::Busy => "you are busy".to_owned(),
    Refusal::NothingToCook => "you have nothing to cook".to_owned(),
    Refusal::Dead => "you are not up yet".to_owned(),
  }
}
