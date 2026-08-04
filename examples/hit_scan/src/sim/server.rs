//! The authority, and the one decision it makes that has a loser.
//!
//! [`resolve_shot`] is the file's reason to exist. Everything above it exists
//! to put two sets of positions in front of it: where the targets are, and
//! where the shooter last saw them.

use std::collections::VecDeque;

use plaza_server_utils::{HistoricalStateBuffer, InputSchedule, InputWindow};

use crate::sim::protocol::{DeathEvent, Frame, Intent, ShotEvent, Start, Verdict};
use crate::sim::rules::{cast, line_of_sight, move_circle};
use crate::sim::types::{
  Controls, Dir8, HISTORY_SAMPLES, MAX_SEATS, PLAYER_R, PlayerId, PlayerSnap, PlayerState, RESPAWN_MS, RIFLE_COOLDOWN_MS, RIFLE_DAMAGE, RIFLE_RANGE,
  ROCKET_BLAST_R, ROCKET_COOLDOWN_MS, ROCKET_DAMAGE, ROCKET_LIFETIME_MS, ROCKET_R, ROCKET_SPEED, SIM_STEP_MS, V2, Weapon,
};

/// One shot, as the resolver needs it: who, from where, aimed where, and how
/// far back the server has agreed to look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
  pub shooter: PlayerId,
  pub from: V2,
  pub aim: V2,
  pub weapon: Weapon,
  pub fired_tick: u64,
  pub resolved_tick: u64,
  pub rewind_ms: u64,
}

/// Judges one shot in two worlds and reports which one it landed in.
///
/// The rewound world is authoritative: `hit` is what the shooter saw, because
/// refusing them that is the same as telling them their aim does not work. What
/// the present world is for is the *verdict*, and the verdict is the only place
/// the cost shows up. A hit granted by rewind is a hit that was taken off a
/// target who had already moved, and no count of hits alone will ever say so.
///
/// Both worlds are passed in rather than read from anywhere, so the caller
/// decides what "where the shooter saw them" means and this function cannot
/// quietly disagree with the panel about it.
pub fn resolve_shot(shot: Shot, present: &[(PlayerId, PlayerSnap)], past: &[(PlayerId, PlayerSnap)]) -> ShotEvent {
  let then = cast(shot.from, shot.aim, RIFLE_RANGE, past);
  let now = cast(shot.from, shot.aim, RIFLE_RANGE, present);

  let verdict = match (then.target, now.target) {
    (Some(a), Some(b)) if a == b => Verdict::Plain,
    // Two different victims is still the rewind choosing, and choosing against
    // whoever the present would have spared.
    (Some(_), Some(_)) => Verdict::GrantedByRewind,
    (Some(_), None) => Verdict::GrantedByRewind,
    (None, Some(_)) => Verdict::DeniedByRewind,
    (None, None) => Verdict::Miss,
  };

  let target_was = then.target.and_then(|id| past.iter().find(|(pid, _)| *pid == id).map(|(_, snap)| snap.pos));

  ShotEvent {
    shooter: shot.shooter,
    weapon: shot.weapon,
    from: shot.from,
    to: then.point,
    hit: then.target,
    target_was,
    fired_tick: shot.fired_tick,
    resolved_tick: shot.resolved_tick,
    rewind_ms: shot.rewind_ms,
    verdict,
  }
}

/// A short ring of the last measurements, kept so the panel can show a middle
/// rather than a mean.
///
/// A mean of a latency-shaped distribution is decided by its tail, and the tail
/// here is one player on a bad link.
#[derive(Clone, Debug, Default)]
pub struct Recent {
  values: VecDeque<u64>,
}

impl Recent {
  const KEEP: usize = 128;

  pub fn push(&mut self, v: u64) {
    if self.values.len() == Self::KEEP {
      self.values.pop_front();
    }
    self.values.push_back(v);
  }

  pub fn median(&self) -> Option<u64> {
    if self.values.is_empty() {
      return None;
    }
    let mut sorted: Vec<u64> = self.values.iter().copied().collect();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
  }

  pub fn worst(&self) -> Option<u64> {
    self.values.iter().copied().max()
  }

  pub fn len(&self) -> usize {
    self.values.len()
  }

  pub fn is_empty(&self) -> bool {
    self.values.is_empty()
  }
}

/// Everything the panel counts. Both halves of the trade, side by side, which
/// is the whole point.
#[derive(Clone, Debug, Default)]
pub struct Stats {
  pub shots_fired: u64,
  pub hits: u64,
  pub granted_by_rewind: u64,
  pub denied_by_rewind: u64,

  pub deaths: u64,
  pub deaths_behind_cover: u64,
  pub from_the_past: Recent,

  /// Shots whose honest rewind was longer than the cap allowed.
  pub rewind_clamped: u64,
  /// Frames the ghost enforcement held back rather than sent on the tick they
  /// were minted.
  pub frames_withheld: u64,
  pub frames_sent: u64,
}

impl Stats {
  pub fn hit_rate(&self) -> f32 {
    if self.shots_fired == 0 { 0.0 } else { self.hits as f32 / self.shots_fired as f32 }
  }

  pub fn granted_share(&self) -> f32 {
    if self.hits == 0 { 0.0 } else { self.granted_by_rewind as f32 / self.hits as f32 }
  }

  pub fn behind_cover_share(&self) -> f32 {
    if self.deaths == 0 { 0.0 } else { self.deaths_behind_cover as f32 / self.deaths as f32 }
  }
}

/// What one call to [`Server::advance`] produced.
#[derive(Clone, Debug, Default)]
pub struct Tickout {
  /// Plural because ghost enforcement queues frames and a slow wake can release
  /// two at once. Sending only the newest would drop the one in between, which
  /// is a lost timeline rather than a saved byte.
  pub frames: Vec<Frame>,
  pub shots: Vec<ShotEvent>,
  pub deaths: Vec<DeathEvent>,
}

#[derive(Clone, Debug)]
struct Bot {
  target: Option<PlayerId>,
  repick_at_ms: u64,
  strafe: f32,
  /// Where it was when it was last steered, and how many steps it has spent
  /// asking to move without arriving anywhere.
  ///
  /// Without this a bot walks into a wall and stays there for the rest of the
  /// session. The direction it wants is quantised to eight, so a bot pressed
  /// against a vertical face while aiming a few degrees off due west resolves
  /// to due west, which has no vertical component to slide on. It looks like a
  /// bot that has decided to stand still.
  last_pos: V2,
  stuck_steps: u32,
}

#[derive(Clone, Debug)]
pub struct Server {
  pub players: Vec<PlayerState>,
  pub rockets: Vec<crate::sim::types::RocketState>,
  pub stats: Stats,

  clock_ms: u64,
  tick: u64,
  pending_ms: u64,
  last_send_ms: u64,

  /// Held directions. A *level*: the newest input for a tick replaces any
  /// earlier one, because a direction that arrived twice is still one
  /// direction.
  moves: Vec<InputSchedule<Dir8>>,
  /// Shots. *Events*: every one that arrives must fire, because dropping one is
  /// a trigger pull that never happened.
  shots: Vec<InputSchedule<(i16, Weapon)>>,

  history: HistoricalStateBuffer<PlayerId, PlayerSnap, u64>,
  /// Frames minted but not yet old enough to send, when the ghost permission is
  /// being enforced rather than declared.
  withheld: VecDeque<Frame>,

  bots: Vec<Bot>,
  human: Vec<bool>,
  next_rocket: u32,
  rng: u64,
}

impl Server {
  pub fn new(seats: usize, seed: u64) -> Self {
    let seats = seats.clamp(1, MAX_SEATS);
    Self {
      players: (0..seats).map(|i| PlayerState::spawn(i as PlayerId)).collect(),
      rockets: Vec::new(),
      stats: Stats::default(),
      clock_ms: 0,
      tick: 0,
      pending_ms: 0,
      last_send_ms: 0,
      moves: (0..seats).map(|_| InputSchedule::new()).collect(),
      shots: (0..seats).map(|_| InputSchedule::new()).collect(),
      history: HistoricalStateBuffer::new(HISTORY_SAMPLES),
      withheld: VecDeque::new(),
      bots: (0..seats)
        .map(|_| Bot {
          target: None,
          repick_at_ms: 0,
          strafe: 1.0,
          last_pos: V2::ZERO,
          stuck_steps: 0,
        })
        .collect(),
      human: vec![false; seats],
      next_rocket: 1,
      rng: seed | 1,
    }
  }

  pub fn seats(&self) -> usize {
    self.players.len()
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  pub fn tick(&self) -> u64 {
    self.tick
  }

  pub fn take_seat(&mut self, seat: usize) {
    if let Some(p) = self.players.get_mut(seat) {
      p.bot = false;
    }
    if let Some(h) = self.human.get_mut(seat) {
      *h = true;
    }
    if let Some(s) = self.moves.get_mut(seat) {
      s.clear();
    }
    if let Some(s) = self.shots.get_mut(seat) {
      s.clear();
    }
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(p) = self.players.get_mut(seat) {
      p.bot = true;
      p.dir = Dir8::Still;
    }
    if let Some(h) = self.human.get_mut(seat) {
      *h = false;
    }
    if let Some(s) = self.moves.get_mut(seat) {
      s.clear();
    }
    if let Some(s) = self.shots.get_mut(seat) {
      s.clear();
    }
  }

  pub fn start(&self) -> Start {
    Start {
      server_time_ms: self.clock_ms,
      tick: self.tick,
      players: self.players.clone(),
    }
  }

  /// Positions as they are, which is what the present half of a verdict reads.
  pub fn snaps_now(&self) -> Vec<(PlayerId, PlayerSnap)> {
    self.players.iter().map(|p| (p.id, PlayerSnap { pos: p.pos, alive: p.alive })).collect()
  }

  /// Positions as they were, which is what a shooter saw.
  pub fn snaps_at(&self, at_ms: u64) -> Vec<(PlayerId, PlayerSnap)> {
    self
      .players
      .iter()
      .map(|p| {
        let snap = self
          .history
          .get_state_at_or_before(&p.id, at_ms)
          .unwrap_or(PlayerSnap { pos: p.pos, alive: p.alive });
        (p.id, snap)
      })
      .collect()
  }

  /// Accepts one input for a named tick, or refuses it.
  ///
  /// Two schedules rather than one because the kinds have different loss
  /// semantics: a dropped direction is corrected by the next one, and a dropped
  /// shot is gone.
  pub fn submit(&mut self, seat: usize, tick: u64, intent: Intent, controls: &Controls) -> bool {
    let window = InputWindow {
      max_late: controls.input_max_late_ticks,
      max_early: controls.input_max_early_ticks,
    };
    // Derived from the clock at the call site, never a counter this owns: a
    // schedule that kept its own would survive a reset the clock did not, and
    // silently refuse every input from then on.
    let current = self.clock_ms / SIM_STEP_MS;
    match intent {
      Intent::Walk(dir) => match self.moves.get_mut(seat) {
        Some(schedule) => schedule.submit(tick, dir, current, window).accepted(),
        None => false,
      },
      Intent::Shoot { aim_deg, weapon } => match self.shots.get_mut(seat) {
        Some(schedule) => schedule.submit(tick, (aim_deg, weapon), current, window).accepted(),
        None => false,
      },
    }
  }

  fn rand(&mut self) -> u64 {
    self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    self.rng >> 33
  }

  fn rand_f32(&mut self) -> f32 {
    (self.rand() % 10_000) as f32 / 10_000.0
  }

  /// Steps the world by whole quanta and reports what left the arena.
  ///
  /// Accumulates rather than trusting `delta_time` to be one step, so the
  /// simulation's rate is a property of the game and not of the host's
  /// scheduler.
  pub fn advance(&mut self, delta_ms: u64, controls: &Controls) -> Tickout {
    let mut out = Tickout::default();
    self.pending_ms += delta_ms;
    while self.pending_ms >= SIM_STEP_MS {
      self.pending_ms -= SIM_STEP_MS;
      self.step(controls, &mut out);
    }
    self.publish_frames(controls, &mut out);
    out
  }

  fn step(&mut self, controls: &Controls, out: &mut Tickout) {
    self.clock_ms += SIM_STEP_MS;
    self.tick += 1;
    let dt = SIM_STEP_MS as f32 / 1000.0;

    self.respawn_due();
    self.steer_bots(controls);
    self.apply_moves();

    for i in 0..self.players.len() {
      if !self.players[i].alive {
        continue;
      }
      let delta = self.players[i].velocity().scale(dt);
      self.players[i].pos = move_circle(self.players[i].pos, delta, PLAYER_R);
    }

    // Recorded after moving, at the clock the move landed on, so a rewind to a
    // tick finds the world as that tick ended.
    for p in &self.players {
      self.history.record_state(p.id, self.clock_ms, PlayerSnap { pos: p.pos, alive: p.alive });
    }

    self.fire_due(controls, out);
    self.fire_bots(controls, out);
    self.advance_rockets(controls, out);
    self.mint_frame(controls);
  }

  fn respawn_due(&mut self) {
    let now = self.clock_ms;
    for p in &mut self.players {
      if !p.alive && now >= p.respawn_at_ms {
        let id = p.id;
        let kills = p.kills;
        let deaths = p.deaths;
        let bot = p.bot;
        *p = PlayerState::spawn(id);
        p.kills = kills;
        p.deaths = deaths;
        p.bot = bot;
      }
    }
  }

  fn apply_moves(&mut self) {
    let current = self.clock_ms / SIM_STEP_MS;
    for seat in 0..self.players.len() {
      if !self.human.get(seat).copied().unwrap_or(false) {
        continue;
      }
      // `execute_due`, not `drain_due`: a level input only needs its newest
      // value for this tick, and running three stale directions in a row would
      // walk a path nobody asked for.
      if let Some(dir) = self.moves[seat].execute_due(current) {
        self.players[seat].dir = dir;
      }
    }
  }

  fn fire_due(&mut self, controls: &Controls, out: &mut Tickout) {
    let current = self.clock_ms / SIM_STEP_MS;
    for seat in 0..self.players.len() {
      if !self.human.get(seat).copied().unwrap_or(false) {
        continue;
      }
      let due: Vec<(i16, Weapon)> = self.shots[seat].drain_due(current).collect();
      for (aim_deg, weapon) in due {
        self.fire(seat as PlayerId, aim_deg, weapon, current, controls, true, out);
      }
    }
  }

  /// Puts one trigger pull through the rules.
  ///
  /// `compensate` is false for a shooter with no link to compensate for, which
  /// is what a bot is. Passing it rather than reading a seat's kind keeps the
  /// decision at the call site, where the reason for it is visible.
  fn fire(&mut self, shooter: PlayerId, aim_deg: i16, weapon: Weapon, fired_tick: u64, controls: &Controls, compensate: bool, out: &mut Tickout) {
    let Some(p) = self.players.get(shooter as usize) else { return };
    if !p.alive {
      return;
    }
    let ready = match weapon {
      Weapon::Rifle => p.rifle_ready_at_ms,
      Weapon::Rocket => p.rocket_ready_at_ms,
    };
    if self.clock_ms < ready {
      return;
    }
    let from = p.pos;
    let aim = V2::from_degrees(aim_deg);

    match weapon {
      Weapon::Rifle => {
        self.players[shooter as usize].rifle_ready_at_ms = self.clock_ms + RIFLE_COOLDOWN_MS;
        self.stats.shots_fired += 1;

        // What the shooter's screen showed when they pulled the trigger: their
        // input names a tick a playout depth ahead, and they were watching a
        // world a render delay behind. Both, because both are real.
        let honest = if compensate { controls.playout_delay_ms + controls.render_delay_ms } else { 0 };
        let budget = controls.rewind_budget_ms();
        let rewind_ms = honest.min(budget);
        if honest > budget {
          self.stats.rewind_clamped += 1;
        }
        let fired_ms = fired_tick * SIM_STEP_MS;
        let target_ms = fired_ms.saturating_sub(rewind_ms);

        // The shooter is in neither list. A ray starting at the centre of a
        // body hits that body at zero distance, so leaving themselves in makes
        // every shot a suicide. Their own position is the muzzle and is read
        // from the present, never rewound: that is where they are standing.
        let present: Vec<_> = self.snaps_now().into_iter().filter(|(id, _)| *id != shooter).collect();
        let past: Vec<_> = if rewind_ms == 0 {
          present.clone()
        } else {
          self.snaps_at(target_ms).into_iter().filter(|(id, _)| *id != shooter).collect()
        };

        let event = resolve_shot(
          Shot {
            shooter,
            from,
            aim,
            weapon,
            fired_tick,
            resolved_tick: self.tick,
            rewind_ms,
          },
          &present,
          &past,
        );

        match event.verdict {
          Verdict::GrantedByRewind => self.stats.granted_by_rewind += 1,
          Verdict::DeniedByRewind => self.stats.denied_by_rewind += 1,
          _ => {}
        }
        if event.verdict.landed() {
          self.stats.hits += 1;
        }
        if let Some(victim) = event.hit.filter(|_| event.verdict.landed()) {
          self.damage(victim, Some(shooter), weapon, RIFLE_DAMAGE, rewind_ms, controls, out);
        }
        out.shots.push(event);
      }

      Weapon::Rocket => {
        self.players[shooter as usize].rocket_ready_at_ms = self.clock_ms + ROCKET_COOLDOWN_MS;
        self.stats.shots_fired += 1;
        let id = self.next_rocket;
        self.next_rocket += 1;
        self.rockets.push(crate::sim::types::RocketState {
          id,
          owner: shooter,
          pos: from.add(aim.normalized().scale(PLAYER_R + ROCKET_R + 1.0)),
          vel: aim.normalized().scale(ROCKET_SPEED),
          dies_at_ms: self.clock_ms + ROCKET_LIFETIME_MS,
        });
        // No verdict: a rocket is not resolved at all yet. Its fairness is a
        // client's patience rather than a server's policy, which is the
        // comparison this weapon is here to make.
        out.shots.push(ShotEvent {
          shooter,
          weapon,
          from,
          to: from,
          hit: None,
          target_was: None,
          fired_tick,
          resolved_tick: self.tick,
          rewind_ms: 0,
          verdict: Verdict::Miss,
        });
      }
    }
  }

  fn advance_rockets(&mut self, controls: &Controls, out: &mut Tickout) {
    let dt = SIM_STEP_MS as f32 / 1000.0;
    let now = self.clock_ms;
    let mut detonations: Vec<(V2, PlayerId)> = Vec::new();

    // Split into two disjoint field borrows so the bodies can be read while the
    // rockets are being walked.
    let players = &self.players;
    let rockets = &mut self.rockets;
    rockets.retain_mut(|r| {
      let step = r.vel.scale(dt);
      let wanted = step.len();
      let next = move_circle(r.pos, step, ROCKET_R);
      // `move_circle` slides along cover, so a rocket that covered less ground
      // than it asked for is a rocket that ran into something.
      let travelled = next.dist(r.pos);
      r.pos = next;

      let struck = players
        .iter()
        .any(|p| p.alive && p.id != r.owner && p.pos.dist(r.pos) <= PLAYER_R + ROCKET_R);

      if struck || travelled + 0.01 < wanted || now >= r.dies_at_ms {
        detonations.push((r.pos, r.owner));
        return false;
      }
      true
    });

    for (at, owner) in detonations {
      let caught: Vec<(PlayerId, f32)> = self
        .players
        .iter()
        .filter(|p| p.alive)
        .map(|p| (p.id, p.pos.dist(at)))
        .filter(|(_, d)| *d <= ROCKET_BLAST_R + PLAYER_R)
        .collect();
      for (victim, dist) in caught {
        // Falls off with distance, and cover does not stop a blast: the two
        // weapons would otherwise be the same weapon at different speeds.
        let falloff = 1.0 - (dist / (ROCKET_BLAST_R + PLAYER_R)).clamp(0.0, 1.0);
        let damage = (ROCKET_DAMAGE as f32 * (0.4 + 0.6 * falloff)).round() as i32;
        let killer = if victim == owner { None } else { Some(owner) };
        self.damage(victim, killer, Weapon::Rocket, damage, 0, controls, out);
      }
    }
  }

  fn damage(&mut self, victim: PlayerId, killer: Option<PlayerId>, weapon: Weapon, amount: i32, rewind_ms: u64, controls: &Controls, out: &mut Tickout) {
    let Some(v) = self.players.get_mut(victim as usize) else { return };
    if !v.alive {
      return;
    }
    v.health -= amount;
    if v.health > 0 {
      return;
    }
    v.alive = false;
    v.health = 0;
    v.dir = Dir8::Still;
    v.deaths += 1;
    v.respawn_at_ms = self.clock_ms + RESPAWN_MS;
    let respawn_at_ms = v.respawn_at_ms;
    let victim_pos = v.pos;

    if let Some(k) = killer.and_then(|k| self.players.get_mut(k as usize)) {
      k.kills += 1;
    }

    // The number this example exists to print. Asked of the *present*: could
    // the victim, standing where they stand now, be seen from where the shooter
    // stands now? If not, they reached cover and were shot there anyway.
    let behind_cover = match killer.and_then(|k| self.players.get(k as usize)) {
      Some(shooter) => !line_of_sight(victim_pos, shooter.pos),
      None => false,
    };

    // How far behind the victim's own present the fatal decision was made. The
    // shooter's rewind plus the delay the victim renders at: peeker's advantage
    // with both terms visible.
    let from_the_past_ms = rewind_ms + controls.render_delay_ms;

    self.stats.deaths += 1;
    if behind_cover {
      self.stats.deaths_behind_cover += 1;
    }
    self.stats.from_the_past.push(from_the_past_ms);

    out.deaths.push(DeathEvent {
      victim,
      killer,
      weapon,
      at_ms: self.clock_ms,
      respawn_at_ms,
      behind_cover,
      from_the_past_ms,
    });
  }

  /// Puts a frame on the send grid. Minting is per step, so a slow wake that
  /// covers three intervals produces three frames rather than one.
  fn mint_frame(&mut self, controls: &Controls) {
    if self.clock_ms.saturating_sub(self.last_send_ms) < controls.sync_interval_ms() {
      return;
    }
    self.last_send_ms = self.clock_ms;
    self.withheld.push_back(Frame {
      server_time_ms: self.clock_ms,
      tick: self.tick,
      players: self.players.clone(),
      rockets: self.rockets.clone(),
    });
  }

  /// Decides which minted frames may leave yet.
  fn publish_frames(&mut self, controls: &Controls, out: &mut Tickout) {
    if controls.allow_ghost {
      while let Some(frame) = self.withheld.pop_front() {
        self.stats.frames_sent += 1;
        out.frames.push(frame);
      }
      self.stats.frames_withheld = 0;
      return;
    }

    // Enforcement, and the only formulation that works: withhold against the
    // *declared timeline* rather than against the wire. Delaying the send alone
    // changes nothing, because the client's playout clock is derived from the
    // stream and shifts with it, leaving the buffer depth identical.
    let horizon = self.clock_ms.saturating_sub(controls.render_delay_ms);
    while self.withheld.front().is_some_and(|f| f.server_time_ms <= horizon) {
      let frame = self.withheld.pop_front().expect("just checked");
      self.stats.frames_sent += 1;
      out.frames.push(frame);
    }
    self.stats.frames_withheld = self.withheld.len() as u64;
  }

  fn steer_bots(&mut self, controls: &Controls) {
    if !controls.bots {
      for seat in 0..self.players.len() {
        if !self.human.get(seat).copied().unwrap_or(false) {
          self.players[seat].dir = Dir8::Still;
        }
      }
      return;
    }
    let now = self.clock_ms;
    let seats = self.players.len();
    for seat in 0..seats {
      if self.human.get(seat).copied().unwrap_or(false) || !self.players[seat].alive {
        continue;
      }
      let me = self.players[seat].pos;

      let moved = me.dist(self.bots[seat].last_pos);
      self.bots[seat].last_pos = me;
      if moved < 0.4 && self.players[seat].dir != Dir8::Still {
        self.bots[seat].stuck_steps += 1;
      } else {
        self.bots[seat].stuck_steps = 0;
      }

      if now >= self.bots[seat].repick_at_ms {
        let pick = (0..seats)
          .filter(|i| *i != seat && self.players[*i].alive)
          .min_by(|a, b| me.dist(self.players[*a].pos).total_cmp(&me.dist(self.players[*b].pos)))
          .map(|i| self.players[i].id);
        self.bots[seat].target = pick;
        self.bots[seat].repick_at_ms = now + 700 + self.rand() % 900;
        self.bots[seat].strafe = if self.rand() % 2 == 0 { 1.0 } else { -1.0 };
      }

      let Some(target_id) = self.bots[seat].target else {
        self.players[seat].dir = Dir8::Still;
        continue;
      };
      let Some(target) = self.players.get(target_id as usize).filter(|t| t.alive) else {
        self.bots[seat].repick_at_ms = 0;
        continue;
      };
      let to_target = target.pos.sub(me);
      let dist = to_target.len();
      let seen = line_of_sight(me, target.pos);

      // Closes when it cannot see, strafes when it can. Enough to keep bodies
      // crossing sight lines, which is the traffic the rewind numbers need, and
      // deliberately no more: a bot good enough to be interesting would make
      // every number a fact about the bot.
      let mut want = if !seen || dist > 220.0 {
        to_target.normalized()
      } else {
        V2::new(-to_target.y, to_target.x).normalized().scale(self.bots[seat].strafe)
      };

      // Wedged against a face it cannot slide along: turn ninety degrees and
      // walk out along it. Reversing instead would produce a bot that paces
      // the same two cells, which reads as working and is not.
      if self.bots[seat].stuck_steps > 8 {
        want = V2::new(-want.y, want.x).scale(self.bots[seat].strafe);
      }
      if self.bots[seat].stuck_steps > 40 {
        self.bots[seat].strafe = -self.bots[seat].strafe;
        self.bots[seat].stuck_steps = 0;
        self.bots[seat].repick_at_ms = now;
      }

      self.players[seat].dir = Dir8::from_axes(quantise(want.x), quantise(want.y));
    }
  }

  /// Bots pull their triggers where a connected player's inputs are drained,
  /// after everything has moved, so both kinds of shooter read the same
  /// geometry. Steering happens before the move and shooting after it, which is
  /// the same split a client makes.
  fn fire_bots(&mut self, controls: &Controls, out: &mut Tickout) {
    if !controls.bots {
      return;
    }
    let now = self.clock_ms;
    let tick = now / SIM_STEP_MS;
    for seat in 0..self.players.len() {
      if self.human.get(seat).copied().unwrap_or(false) || !self.players[seat].alive {
        continue;
      }
      if now < self.players[seat].rifle_ready_at_ms {
        continue;
      }
      let Some(target_id) = self.bots[seat].target else { continue };
      let Some(target_pos) = self.players.get(target_id as usize).filter(|t| t.alive).map(|t| t.pos) else { continue };
      let me = self.players[seat].pos;
      if !line_of_sight(me, target_pos) {
        continue;
      }
      if self.rand() % 3 != 0 {
        continue;
      }
      let spread = (self.rand_f32() - 0.5) * 14.0;
      let aim = (target_pos.sub(me).to_degrees_i16() as f32 + spread).round() as i32;
      // A bot's shot is resolved against the present, because a bot has no link
      // to compensate for. It reaches the same `fire` as everybody else, so a
      // bot kill counts in exactly the same places.
      self.fire(seat as PlayerId, aim.rem_euclid(360) as i16, Weapon::Rifle, tick, controls, false, out);
    }
  }

  /// Verdict counters per seat, for the movement schedule and the shot
  /// schedule: `(accepted, late, closed, ahead, last margin)`.
  pub fn input_verdicts(&self) -> Vec<(u64, u64, u64, u64, Option<i64>)> {
    self
      .moves
      .iter()
      .zip(self.shots.iter())
      .map(|(m, s)| {
        let (mc, ma) = m.rejected_split();
        let (sc, sa) = s.rejected_split();
        (
          m.accepted() + s.accepted(),
          m.late() + s.late(),
          mc + sc,
          ma + sa,
          s.last_reject_margin().or_else(|| m.last_reject_margin()),
        )
      })
      .collect()
  }
}

fn quantise(v: f32) -> i32 {
  if v > 0.38 {
    1
  } else if v < -0.38 {
    -1
  } else {
    0
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::Rewind;

  const SEED: u64 = 0x51_6E_7A_11;

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      bots: false,
      players: 2,
      ..Controls::default()
    }
  }

  /// The one horizontal line that crosses the map with nothing on it. Both
  /// pillars span y 60..150, so the obvious y 100 is not the open lane it
  /// looks like.
  const OPEN_LANE_Y: f32 = 162.0;

  /// Two players in a straight open lane, the target having just stepped out of
  /// it. The whole example in one setup.
  fn duel(controls: &Controls) -> Server {
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.take_seat(1);
    s.players[0].pos = V2::new(20.0, OPEN_LANE_Y);
    s.players[1].pos = V2::new(200.0, OPEN_LANE_Y);
    // Long enough that the history covers the whole rewind budget.
    s.advance(400, controls);
    s.players[0].pos = V2::new(20.0, OPEN_LANE_Y);
    s.players[1].pos = V2::new(200.0, OPEN_LANE_Y - 60.0);
    s.advance(SIM_STEP_MS, controls);
    s
  }

  fn shoot_east(s: &mut Server, controls: &Controls) -> ShotEvent {
    let tick = s.now_ms() / SIM_STEP_MS;
    assert!(s.submit(0, tick, Intent::Shoot { aim_deg: 0, weapon: Weapon::Rifle }, controls));
    let out = s.advance(SIM_STEP_MS, controls);
    out.shots.into_iter().find(|e| e.weapon == Weapon::Rifle).expect("the trigger was pulled")
  }

  #[test]
  fn a_rewind_grants_a_hit_the_present_would_have_missed() {
    // The headline. The target is out of the lane *now* and was in it when the
    // shooter's screen was drawn, and the server sides with the shooter.
    let controls = Controls { rewind: Rewind::Capped, rewind_cap_ms: 250, ..quiet() };
    let mut s = duel(&controls);
    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.hit, Some(1), "the shooter's aim worked");
    assert_eq!(event.verdict, Verdict::GrantedByRewind);
    assert_eq!(s.stats.granted_by_rewind, 1);
  }

  #[test]
  fn the_same_shot_misses_when_the_server_refuses_to_look_back() {
    // The other half of the trade, and the reason the panel needs a switch
    // rather than a paragraph: nothing about the shooter changed.
    let controls = Controls { rewind: Rewind::Off, ..quiet() };
    let mut s = duel(&controls);
    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.hit, None);
    assert_eq!(event.verdict, Verdict::Miss);
    assert_eq!(s.stats.granted_by_rewind, 0, "with no rewind there is nothing to grant");
  }

  #[test]
  fn a_rewind_shorter_than_the_lag_it_compensates_is_counted_as_clamped() {
    // A cap is not free: past it the shooter is being asked to lead their
    // target again, and the panel should say how often that happened rather
    // than letting the cap look costless.
    let controls = Controls { rewind: Rewind::Capped, rewind_cap_ms: 20, playout_delay_ms: 100, render_delay_ms: 100, ..quiet() };
    let mut s = duel(&controls);
    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.rewind_ms, 20, "the cap, not the honest 200");
    assert_eq!(s.stats.rewind_clamped, 1);
  }

  #[test]
  fn a_shot_that_lands_in_both_worlds_takes_nothing_from_anybody() {
    let controls = Controls { rewind: Rewind::Capped, rewind_cap_ms: 250, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.take_seat(1);
    s.players[0].pos = V2::new(20.0, OPEN_LANE_Y);
    s.players[1].pos = V2::new(200.0, OPEN_LANE_Y);
    s.advance(400, &controls);
    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.verdict, Verdict::Plain);
    assert_eq!(s.stats.granted_by_rewind, 0);
    assert_eq!(s.stats.denied_by_rewind, 0);
  }

  #[test]
  fn a_bots_shot_is_not_compensated_because_a_bot_has_no_link() {
    // Compensating a shooter with no latency would invent unfairness rather
    // than correct any, and every bot kill would land in the granted column.
    // Both seats are bots, facing each other down the open lane, so the claim
    // is tested without depending on either of them finding the other.
    let controls = Controls { bots: true, players: 2, rewind: Rewind::Capped, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.players[0].pos = V2::new(120.0, OPEN_LANE_Y);
    s.players[1].pos = V2::new(420.0, OPEN_LANE_Y);
    let mut granted = 0;
    for _ in 0..900 {
      let out = s.advance(SIM_STEP_MS, &controls);
      granted += out.shots.iter().filter(|e| e.verdict == Verdict::GrantedByRewind).count();
    }
    assert!(s.stats.shots_fired > 0, "the bots did shoot");
    assert_eq!(granted, 0, "and none of it was charged to anybody's latency");
    assert_eq!(s.stats.rewind_clamped, 0, "nor was any of it clamped");
  }

  #[test]
  fn a_bot_walked_into_a_wall_gets_out_of_it_again() {
    // pellet_maze's lesson, paid for again here: a bot that looks broken makes
    // the example look broken. Steering is quantised to eight directions, so a
    // bot pressed against a vertical face while wanting to go a few degrees off
    // due west resolves to due west and stands there for ever. It is invisible
    // without a number, because a stationary bot is a plausible bot.
    let controls = Controls { bots: true, players: 2, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    // Wedged against the right face of the lower right pillar, aiming past it.
    s.players[0].pos = V2::new(60.0, 60.0);
    s.players[1].pos = V2::new(512.0, 245.0);

    let start = s.players[1].pos;
    let mut furthest = 0.0f32;
    for _ in 0..600 {
      s.advance(SIM_STEP_MS, &controls);
      furthest = furthest.max(start.dist(s.players[1].pos));
    }
    assert!(furthest > 120.0, "the bot only ever got {furthest:.0} units from the wall");
  }

  #[test]
  fn a_direction_and_a_shot_do_not_share_a_queue() {
    // Two schedules, because the kinds have different loss semantics. Mixed
    // into one queue, `execute_due` keeping only the newest would silently eat
    // every shot fired on a tick that also carried a direction.
    let controls = quiet();
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    let tick = s.now_ms() / SIM_STEP_MS + 1;
    assert!(s.submit(0, tick, Intent::Walk(Dir8::E), &controls));
    assert!(s.submit(0, tick, Intent::Shoot { aim_deg: 0, weapon: Weapon::Rifle }, &controls));
    assert!(s.submit(0, tick, Intent::Shoot { aim_deg: 90, weapon: Weapon::Rifle }, &controls));

    let out = s.advance(SIM_STEP_MS * 2, &controls);
    assert_eq!(s.players[0].dir, Dir8::E, "the direction landed");
    // The second shot is refused by the cooldown rather than by the queue,
    // which is a rule and not a dropped input: the schedule handed both over.
    assert_eq!(out.shots.len(), 1);
    assert_eq!(s.stats.shots_fired, 1);
  }

  #[test]
  fn an_input_naming_a_tick_that_has_already_run_is_refused_rather_than_shifted() {
    // Correcting a backdated tick still executes the input, so a lag switch
    // loses the lie and keeps the steering. Dropping it makes backdating cost
    // the input.
    let controls = Controls { input_max_late_ticks: 2, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.advance(1000, &controls);
    let current = s.now_ms() / SIM_STEP_MS;
    let accepted = s.submit(0, current.saturating_sub(30), Intent::Shoot { aim_deg: 0, weapon: Weapon::Rifle }, &controls);
    assert!(!accepted, "thirty ticks late, against a window of two");

    let out = s.advance(SIM_STEP_MS * 4, &controls);
    assert!(out.shots.is_empty(), "and it did not fire late either");
  }

  #[test]
  fn enforcing_the_ghost_permission_withholds_a_frame_until_its_instant_has_passed() {
    // Not a delayed send: withheld against the *declared* timeline. A client's
    // playout clock is derived from the stream and shifts with it, so merely
    // sending later leaves the unresolved window exactly as it was.
    let controls = Controls { allow_ghost: false, render_delay_ms: 100, sync_hz: 20, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);

    let mut sent = Vec::new();
    for _ in 0..40 {
      let out = s.advance(SIM_STEP_MS, &controls);
      let now = s.now_ms();
      for frame in out.frames {
        assert!(
          frame.server_time_ms + controls.render_delay_ms <= now,
          "frame stamped {} left at {now}, inside the client's own render instant",
          frame.server_time_ms
        );
        sent.push(frame.server_time_ms);
      }
    }
    assert!(!sent.is_empty(), "frames still flow, just later");
  }

  #[test]
  fn allowing_the_ghost_sends_a_frame_on_the_tick_it_was_minted() {
    let controls = Controls { allow_ghost: true, render_delay_ms: 100, sync_hz: 20, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    let mut unresolved = 0;
    for _ in 0..40 {
      let out = s.advance(SIM_STEP_MS, &controls);
      let now = s.now_ms();
      for frame in out.frames {
        if frame.server_time_ms + controls.render_delay_ms > now {
          unresolved += 1;
        }
      }
    }
    assert!(unresolved > 0, "the slack a ghost overlay reads is exactly this");
  }

  #[test]
  fn a_rewind_never_reaches_past_the_history_it_can_read() {
    // `HistoricalStateBuffer` clamps to its oldest sample rather than refusing,
    // so an unbounded budget would resolve shots against a position the server
    // no longer knows and report it as fact.
    let controls = Controls { rewind: Rewind::Uncapped, playout_delay_ms: 4000, render_delay_ms: 4000, ..quiet() };
    let mut s = duel(&controls);
    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.rewind_ms, crate::sim::types::HISTORY_MS);
    assert!(s.stats.rewind_clamped > 0);
  }

  #[test]
  fn the_muzzle_is_where_the_shooter_stands_now_rather_than_where_they_stood() {
    // Rewinding the shooter along with their targets puts the ray's origin
    // somewhere nobody is, and on this map that means firing out of the far
    // side of a wall they have already walked past.
    let controls = Controls { rewind: Rewind::Capped, rewind_cap_ms: 250, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.take_seat(1);
    s.players[0].pos = V2::new(20.0, OPEN_LANE_Y);
    s.players[1].pos = V2::new(200.0, OPEN_LANE_Y);
    s.advance(400, &controls);
    // The shooter moves a long way immediately before firing.
    s.players[0].pos = V2::new(240.0, OPEN_LANE_Y);
    s.advance(SIM_STEP_MS, &controls);

    let event = shoot_east(&mut s, &controls);
    assert_eq!(event.from, V2::new(240.0, OPEN_LANE_Y), "not the rewound position");
  }

  #[test]
  fn a_shooter_cannot_hit_themselves() {
    // A ray starting at the centre of a body hits that body at zero distance,
    // so leaving the shooter in the cast makes every trigger pull a suicide.
    // It reads as a wildly effective weapon until somebody checks who died.
    let controls = Controls { rewind: Rewind::Capped, ..quiet() };
    let mut s = duel(&controls);
    let event = shoot_east(&mut s, &controls);
    assert_ne!(event.hit, Some(0));
    assert!(s.players[0].alive);
  }

  #[test]
  fn a_death_behind_cover_is_counted_as_one() {
    // The victim reaches cover and is shot there anyway, which is what
    // granting the shooter their own view costs. Constructed rather than waited
    // for: the count is the claim, so it must be reachable on purpose.
    let controls = Controls { rewind: Rewind::Capped, rewind_cap_ms: 400, playout_delay_ms: 100, render_delay_ms: 100, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.take_seat(1);

    // The shooter sits left of the central block, which spans x 270..370,
    // y 176..224. The victim spends its history up and to the right, on a
    // sight line that passes over the block.
    let shooter = V2::new(200.0, 190.0);
    let seen_at = V2::new(400.0, 130.0);
    let cover_at = V2::new(400.0, 190.0);
    assert!(line_of_sight(shooter, seen_at), "the past position was visible");
    assert!(!line_of_sight(shooter, cover_at), "and the present one is not");

    s.players[0].pos = shooter;
    s.players[1].pos = seen_at;
    s.advance(400, &controls);
    s.players[0].pos = shooter;
    s.players[1].pos = cover_at;
    s.players[1].health = 1;
    s.advance(SIM_STEP_MS, &controls);

    let aim = seen_at.sub(shooter).to_degrees_i16();
    let tick = s.now_ms() / SIM_STEP_MS;
    assert!(s.submit(0, tick, Intent::Shoot { aim_deg: aim, weapon: Weapon::Rifle }, &controls));
    let out = s.advance(SIM_STEP_MS, &controls);

    let death = out.deaths.first().expect("the rewind found them where they used to be");
    assert_eq!(death.victim, 1);
    assert!(death.behind_cover, "and killed them where the shooter cannot see");
    assert_eq!(s.stats.deaths_behind_cover, 1);
    assert!(death.from_the_past_ms >= controls.render_delay_ms);
  }

  #[test]
  fn a_dead_player_comes_back_with_their_score_and_not_their_wounds() {
    let controls = quiet();
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    s.players[0].kills = 7;
    s.players[0].deaths = 3;
    s.players[0].alive = false;
    s.players[0].health = 0;
    s.players[0].respawn_at_ms = s.now_ms() + 100;
    s.advance(200, &controls);
    assert!(s.players[0].alive);
    assert_eq!(s.players[0].health, crate::sim::types::MAX_HEALTH);
    assert_eq!(s.players[0].kills, 7, "a score survives a death");
    assert_eq!(s.players[0].deaths, 3);
  }

  #[test]
  fn a_slow_wake_produces_every_frame_rather_than_the_newest_one() {
    // Minting per step rather than per wake. Skipping the frames in between is
    // a lost timeline on the client, not a saved byte.
    let controls = Controls { sync_hz: 60, ..quiet() };
    let mut s = Server::new(2, SEED);
    s.take_seat(0);
    let out = s.advance(SIM_STEP_MS * 10, &controls);
    assert!(out.frames.len() >= 9, "got {}", out.frames.len());
    let stamps: Vec<u64> = out.frames.iter().map(|f| f.server_time_ms).collect();
    assert!(stamps.windows(2).all(|w| w[0] < w[1]), "and in order: {stamps:?}");
  }

  #[test]
  fn the_median_of_a_recent_ring_is_not_dragged_by_one_bad_link() {
    let mut r = Recent::default();
    for _ in 0..20 {
      r.push(100);
    }
    r.push(5000);
    assert_eq!(r.median(), Some(100));
    assert_eq!(r.worst(), Some(5000));
  }
}
