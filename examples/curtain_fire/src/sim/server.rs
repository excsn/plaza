//! The authority, and the question it answers three different ways.
//!
//! [`Server::judge_deaths`] is the file's reason to exist. Everything above it
//! keeps two things true: the curtain is never stored, and every ship's recent
//! position is, because a declaration names a tick in the past.

use plaza_server_utils::{HistoricalStateBuffer, InputSchedule, InputWindow};

use crate::sim::curtain::{Bullet, Downed, Wave, contact, curtain_at, make_wave};
use crate::sim::protocol::{DeathEvent, DeathVerdict, Frame, Intent, ServerPolicy, Start};
use crate::sim::types::{
  Controls, DeathRule, Dir8, FIELD_H, FIELD_W, INVULN_MS, MAX_SEATS, PLAYER_BULLET_R, PLAYER_BULLET_SPEED, PLAYER_FIRE_COOLDOWN_MS, PlayerBullet,
  PlayerId, SHIP_R, SHIP_SPEED, SIM_STEP_MS, Ship, V2, WaveId, EMITTER_R,
};

/// How long after a contact a declaration may still arrive.
///
/// A ship on a slow link declares late by construction, so a window has to
/// exist. It bounds the other direction too: a declaration older than this
/// names a tick nobody can be asked to re-examine.
const DECLARE_WINDOW_TICKS: u64 = 40;

/// Ticks a contact goes unmentioned before the server counts it against the
/// ship. One declare window plus slack.
const SILENCE_TICKS: u64 = DECLARE_WINDOW_TICKS + 20;

const HISTORY_SAMPLES: usize = 128;

const WAVE_GAP_TICKS: u64 = 220;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipSnap {
  pub pos: V2,
  pub alive: bool,
}

impl plaza_client_utils::interpolation::Interpolatable<u64> for ShipSnap {
  fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
    Self {
      pos: self.pos.lerp(other.pos, t),
      alive: self.alive,
    }
  }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
  /// Contacts the server found for itself.
  pub server_found: u64,
  /// Declarations received.
  pub declared: u64,
  pub declared_confirmed: u64,
  /// Declarations the recomputed curtain disagreed with.
  pub declared_refused: u64,
  /// Contacts the server saw and nobody ever owned up to.
  ///
  /// The number that answers "how cheatable is letting the ship decide". The
  /// answer is completely, and also completely visible, because a shared
  /// curtain means the server can take this count for the price of the
  /// evaluation it was doing anyway.
  pub undeclared: u64,
  pub deaths: u64,
  /// Total ticks between a contact and the server acting on it.
  pub death_lateness_ticks: u64,

  pub bytes_derivable: u64,
  pub bytes_streamed: u64,
  /// The same traffic with numeric variant tags, for the share measurement.
  pub bytes_numerically_tagged: u64,
  pub bytes_total: u64,
  /// Peak enemy bullets alive at once, none of which was ever described.
  pub peak_curtain: usize,
  /// Bullet-ticks: one bullet alive for one tick.
  ///
  /// The denominator for a byte comparison, and it has to be cumulative to
  /// match a cumulative numerator. Dividing total bytes by the *instantaneous*
  /// count is the mistake that made this comparison read as a hundred-thousand
  /// to one: at the sampling instant the streamed half happened to hold no
  /// bullets at all, so it was total bytes over one.
  pub curtain_bullet_ticks: u64,
  pub player_bullet_ticks: u64,
}

impl Stats {
  pub fn mean_death_lateness(&self) -> f32 {
    if self.deaths == 0 { 0.0 } else { self.death_lateness_ticks as f32 / self.deaths as f32 }
  }

  /// The share of outbound bytes that is the names of variants.
  pub fn variant_name_share(&self) -> f32 {
    if self.bytes_total == 0 {
      return 0.0;
    }
    (self.bytes_total.saturating_sub(self.bytes_numerically_tagged)) as f32 / self.bytes_total as f32
  }
}

#[derive(Clone, Debug, Default)]
pub struct Tickout {
  pub frames: Vec<Frame>,
  pub waves: Vec<Wave>,
  pub downed: Vec<Downed>,
  pub deaths: Vec<DeathEvent>,
}

/// A contact the server has seen and is waiting to hear about.
#[derive(Clone, Copy, Debug)]
struct Pending {
  ship: PlayerId,
  tick: u64,
}

#[derive(Clone, Debug)]
pub struct Server {
  pub ships: Vec<Ship>,
  pub bullets: Vec<PlayerBullet>,
  pub waves: Vec<Wave>,
  pub downed: Vec<Downed>,
  pub emitter_health: Vec<(WaveId, u8, i32)>,
  pub stats: Stats,

  clock_ms: u64,
  tick: u64,
  pending_ms: u64,
  last_send_ms: u64,
  next_wave_at: u64,
  next_wave: WaveId,
  next_bullet: u32,

  /// A held direction is a level. A shot and a declaration are events, and a
  /// dropped declaration is a life.
  moves: Vec<InputSchedule<Dir8>>,
  events: Vec<InputSchedule<Intent>>,

  history: HistoricalStateBuffer<PlayerId, ShipSnap, u64>,
  /// Contacts seen and not yet declared.
  unowned: Vec<Pending>,
  /// Frames minted on the send grid, drained by `publish`.
  minted: Vec<Frame>,
  /// Deaths produced by a declaration.
  ///
  /// A declaration arrives through `submit`, which is outside any tick, so the
  /// death it causes has nowhere to go until the next publish. Queuing it is
  /// what keeps a declaration from being silently dropped between ticks.
  deferred: Vec<DeathEvent>,
  /// One buffer, reused. The curtain is evaluated once per tick and every ship
  /// is tested against the same evaluation.
  scratch: Vec<Bullet>,
  human: Vec<bool>,
  rng: u64,
}

impl Server {
  pub fn new(seats: usize, seed: u64) -> Self {
    let seats = seats.clamp(1, MAX_SEATS);
    Self {
      ships: (0..seats).map(|i| Ship::spawn(i as PlayerId)).collect(),
      bullets: Vec::new(),
      waves: Vec::new(),
      downed: Vec::new(),
      emitter_health: Vec::new(),
      stats: Stats::default(),
      clock_ms: 0,
      tick: 0,
      pending_ms: 0,
      last_send_ms: 0,
      next_wave_at: 30,
      next_wave: 0,
      next_bullet: 1,
      moves: (0..seats).map(|_| InputSchedule::new()).collect(),
      events: (0..seats).map(|_| InputSchedule::new()).collect(),
      history: HistoricalStateBuffer::new(HISTORY_SAMPLES),
      unowned: Vec::new(),
      minted: Vec::new(),
      deferred: Vec::new(),
      scratch: Vec::new(),
      human: vec![false; seats],
      rng: seed | 1,
    }
  }

  pub fn seats(&self) -> usize {
    self.ships.len()
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  pub fn tick(&self) -> u64 {
    self.tick
  }

  pub fn take_seat(&mut self, seat: usize) {
    if let Some(s) = self.ships.get_mut(seat) {
      s.bot = false;
    }
    if let Some(h) = self.human.get_mut(seat) {
      *h = true;
    }
    if let Some(s) = self.moves.get_mut(seat) {
      s.clear();
    }
    if let Some(s) = self.events.get_mut(seat) {
      s.clear();
    }
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(s) = self.ships.get_mut(seat) {
      s.bot = true;
      s.dir = Dir8::Still;
    }
    if let Some(h) = self.human.get_mut(seat) {
      *h = false;
    }
    if let Some(s) = self.moves.get_mut(seat) {
      s.clear();
    }
    if let Some(s) = self.events.get_mut(seat) {
      s.clear();
    }
  }

  pub fn start(&self) -> Start {
    Start {
      server_time_ms: self.clock_ms,
      tick: self.tick,
      ships: self.ships.clone(),
      // Everything in flight, so a joiner's curtain is complete from its first
      // frame. A joiner given only future waves would spend fifteen seconds
      // flying through bullets it cannot see.
      waves: self.waves.clone(),
      downed: self.downed.clone(),
    }
  }

  pub fn policy(&self, controls: &Controls) -> ServerPolicy {
    ServerPolicy {
      sync_hz: controls.sync_hz,
      playout_delay_ms: controls.playout_delay_ms,
      render_delay_ms: controls.render_delay_ms,
      input_max_late_ticks: controls.input_max_late_ticks,
      input_max_early_ticks: controls.input_max_early_ticks,
      death_rule: controls.death_rule,
      players: self.seats(),
    }
  }

  pub fn submit(&mut self, seat: usize, tick: u64, intent: Intent, controls: &Controls) -> bool {
    let window = InputWindow {
      max_late: controls.input_max_late_ticks,
      max_early: controls.input_max_early_ticks,
    };
    let current = self.clock_ms / SIM_STEP_MS;
    match intent {
      Intent::Move(dir) => match self.moves.get_mut(seat) {
        Some(s) => s.submit(tick, dir, current, window).accepted(),
        None => false,
      },
      // A declaration is deliberately *not* run through the input window: it
      // names a tick that is already gone by definition, so the window would
      // refuse every one of them. Its own window is `DECLARE_WINDOW_TICKS`,
      // and it is judged rather than scheduled.
      Intent::Struck => {
        self.declare(seat, tick, controls);
        true
      }
      Intent::Fire => match self.events.get_mut(seat) {
        Some(s) => s.submit(tick, intent, current, window).accepted(),
        None => false,
      },
    }
  }

  fn rand(&mut self) -> u64 {
    self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    self.rng >> 33
  }

  pub fn advance(&mut self, delta_ms: u64, controls: &Controls) -> Tickout {
    let mut out = Tickout::default();
    self.pending_ms += delta_ms;
    while self.pending_ms >= SIM_STEP_MS {
      self.pending_ms -= SIM_STEP_MS;
      self.step(controls, &mut out);
    }
    self.publish(controls, &mut out);
    out
  }

  fn step(&mut self, controls: &Controls, out: &mut Tickout) {
    self.clock_ms += SIM_STEP_MS;
    self.tick += 1;
    let dt = SIM_STEP_MS as f32 / 1000.0;

    self.spawn_wave(out);
    self.retire_waves();
    self.steer_bots(controls);
    self.apply_moves();
    self.move_ships(dt);

    for ship in &self.ships {
      self.history.record_state(ship.id, self.clock_ms, ShipSnap { pos: ship.pos, alive: ship.alive });
    }

    // Evaluated once. Every ship, every player bullet and every emitter is
    // tested against this one call, because the curtain is the expensive part
    // and it does not depend on any of them.
    curtain_at(&self.waves, &self.downed, self.tick, &mut self.scratch);
    self.stats.peak_curtain = self.stats.peak_curtain.max(self.scratch.len());
    self.stats.curtain_bullet_ticks += self.scratch.len() as u64;
    self.stats.player_bullet_ticks += self.bullets.len() as u64;

    self.fire_due(controls, out);
    self.advance_player_bullets(dt, out);
    self.judge_deaths(controls, out);
    self.mint_frame(controls);
  }

  fn spawn_wave(&mut self, out: &mut Tickout) {
    if self.tick < self.next_wave_at {
      return;
    }
    let seed = (self.rand() % 100_000) as u32;
    let wave = make_wave(self.next_wave, seed, self.tick);
    for emitter in &wave.emitters {
      self.emitter_health.push((wave.id, emitter.arm, crate::sim::types::EMITTER_HEALTH));
    }
    self.next_wave += 1;
    self.next_wave_at = self.tick + WAVE_GAP_TICKS;
    self.waves.push(wave.clone());
    out.waves.push(wave);
  }

  fn retire_waves(&mut self) {
    let tick = self.tick;
    // Kept a little past their end so a late client's curtain still resolves.
    self.waves.retain(|w| tick < w.end_tick + 300);
    let live: Vec<WaveId> = self.waves.iter().map(|w| w.id).collect();
    self.downed.retain(|d| live.contains(&d.wave));
    self.emitter_health.retain(|(id, _, _)| live.contains(id));
  }

  fn apply_moves(&mut self) {
    let current = self.clock_ms / SIM_STEP_MS;
    for seat in 0..self.ships.len() {
      if !self.human.get(seat).copied().unwrap_or(false) {
        continue;
      }
      if let Some(dir) = self.moves[seat].execute_due(current) {
        self.ships[seat].dir = dir;
      }
    }
  }

  fn move_ships(&mut self, dt: f32) {
    for ship in &mut self.ships {
      if !ship.alive {
        continue;
      }
      let delta = ship.dir.unit().scale(SHIP_SPEED * dt);
      ship.pos = V2::new(
        (ship.pos.x + delta.x).clamp(SHIP_R, FIELD_W - SHIP_R),
        (ship.pos.y + delta.y).clamp(SHIP_R, FIELD_H - SHIP_R),
      );
    }
  }

  fn fire_due(&mut self, controls: &Controls, out: &mut Tickout) {
    let current = self.clock_ms / SIM_STEP_MS;
    let now = self.clock_ms;
    for seat in 0..self.ships.len() {
      let human = self.human.get(seat).copied().unwrap_or(false);
      let wants = if human {
        self.events[seat].drain_due(current).filter(|i| matches!(i, Intent::Fire)).count() > 0
      } else {
        controls.bots
      };
      if !wants || !self.ships[seat].alive || now < self.ships[seat].fire_ready_at_ms {
        continue;
      }
      self.ships[seat].fire_ready_at_ms = now + PLAYER_FIRE_COOLDOWN_MS;
      let id = self.next_bullet;
      self.next_bullet += 1;
      self.bullets.push(PlayerBullet {
        id,
        owner: self.ships[seat].id,
        pos: self.ships[seat].pos.add(V2::new(0.0, -(SHIP_R + PLAYER_BULLET_R + 1.0))),
      });
    }
    let _ = out;
  }

  fn advance_player_bullets(&mut self, dt: f32, out: &mut Tickout) {
    for bullet in &mut self.bullets {
      bullet.pos.y -= PLAYER_BULLET_SPEED * dt;
    }
    self.bullets.retain(|b| b.pos.y > -8.0);

    // Player fire against emitters: the one place the two halves meet, and the
    // reason the curtain is not purely a function of the tick.
    let tick = self.tick;
    let mut hits: Vec<(WaveId, u8)> = Vec::new();
    let waves = &self.waves;
    let downed = &self.downed;
    let bullets = &mut self.bullets;
    bullets.retain(|bullet| {
      for wave in waves {
        for emitter in &wave.emitters {
          if downed.iter().any(|d| d.wave == wave.id && d.arm == emitter.arm) {
            continue;
          }
          let Some(at) = crate::sim::curtain::emitter_at(wave, emitter, tick) else { continue };
          if at.dist(bullet.pos) <= EMITTER_R + PLAYER_BULLET_R {
            hits.push((wave.id, emitter.arm));
            return false;
          }
        }
      }
      true
    });

    for (wave, arm) in hits {
      let Some(slot) = self.emitter_health.iter_mut().find(|(w, a, _)| *w == wave && *a == arm) else { continue };
      slot.2 -= 1;
      if slot.2 > 0 {
        continue;
      }
      // Named by tick, so both sides cut this gun's output at the same instant
      // for ever. One op replaces every bullet it would have fired.
      let down = Downed { wave, arm, tick: self.tick };
      self.downed.push(down);
      out.downed.push(down);
      for ship in &mut self.ships {
        ship.score += 100;
      }
    }
  }

  /// Who says a ship was hit.
  ///
  /// Three answers, and the difference between them is never the curtain. Both
  /// ends compute the identical field from the identical closed form. What they
  /// disagree about is **where the ship was**, and that is the whole of it.
  fn judge_deaths(&mut self, controls: &Controls, out: &mut Tickout) {
    let tick = self.tick;
    let now = self.clock_ms;
    let reach = SHIP_R + crate::sim::types::ENEMY_BULLET_R;

    let touched: Vec<PlayerId> = self
      .ships
      .iter()
      .filter(|s| s.alive && now >= s.invuln_until_ms)
      .filter(|s| self.scratch.iter().any(|b| b.pos.dist(s.pos) <= reach))
      .map(|s| s.id)
      .collect();

    for ship in touched {
      self.stats.server_found += 1;
      match controls.death_rule {
        // Against the ship position the server holds, which is a round trip
        // old. The player watched the bullet miss and dies anyway, and there is
        // no easing a death.
        DeathRule::ServerOnly => self.kill(ship, tick, tick, DeathVerdict::ServerFound, out),
        // Recorded and waited on. If nobody owns up inside the window it
        // becomes a count rather than a death.
        DeathRule::ClientDeclares | DeathRule::ServerConfirms => {
          if !self.unowned.iter().any(|p| p.ship == ship) {
            self.unowned.push(Pending { ship, tick });
          }
        }
      }
    }

    // A contact nobody claimed. Under `ClientDeclares` this is exactly what a
    // ship that has stopped declaring looks like, and it costs nothing to see.
    let cutoff = tick.saturating_sub(SILENCE_TICKS);
    let mut silent = 0;
    self.unowned.retain(|p| {
      if p.tick >= cutoff {
        return true;
      }
      silent += 1;
      false
    });
    self.stats.undeclared += silent;
  }

  /// A ship's own claim that it was hit on a tick.
  fn declare(&mut self, seat: usize, at_tick: u64, controls: &Controls) {
    let Some(ship) = self.ships.get(seat) else { return };
    let id = ship.id;
    if !ship.alive || self.clock_ms < ship.invuln_until_ms {
      return;
    }
    self.stats.declared += 1;

    let current = self.tick;
    if at_tick + DECLARE_WINDOW_TICKS < current || at_tick > current {
      self.stats.declared_refused += 1;
      return;
    }

    let verdict = match controls.death_rule {
      // Trusted. What shipped co-op shmups do, and it feels perfect because it
      // is judged against exactly what the player saw.
      DeathRule::ClientDeclares => DeathVerdict::Confirmed,
      // Checked, and checkable only because the curtain is a function of the
      // tick: the server recomputes the same field the client dodged, at the
      // tick that was named, against where this ship actually was then.
      DeathRule::ServerConfirms => {
        let Some(snap) = self.history.get_state_at_or_before(&id, at_tick * SIM_STEP_MS) else {
          self.stats.declared_refused += 1;
          return;
        };
        let mut scratch = Vec::new();
        if contact(&self.waves, &self.downed, at_tick, snap.pos, SHIP_R, &mut scratch) {
          DeathVerdict::Confirmed
        } else {
          self.stats.declared_refused += 1;
          return;
        }
      }
      DeathRule::ServerOnly => return,
    };

    self.stats.declared_confirmed += 1;
    self.unowned.retain(|p| p.ship != id);
    let mut out = Tickout::default();
    self.kill(id, at_tick, self.tick, verdict, &mut out);
    self.deferred.extend(out.deaths);
  }

  fn kill(&mut self, victim: PlayerId, at_tick: u64, resolved_tick: u64, verdict: DeathVerdict, out: &mut Tickout) {
    let now = self.clock_ms;
    let Some(ship) = self.ships.iter_mut().find(|s| s.id == victim) else { return };
    if !ship.alive || now < ship.invuln_until_ms {
      return;
    }
    ship.lives = ship.lives.saturating_sub(1);
    ship.invuln_until_ms = now + INVULN_MS;
    ship.pos = Ship::spawn(victim).pos;
    if ship.lives == 0 {
      ship.lives = crate::sim::types::SHIP_LIVES;
      ship.score = 0;
    }
    let lives_left = ship.lives;

    let late_by_ticks = resolved_tick.saturating_sub(at_tick);
    self.stats.deaths += 1;
    self.stats.death_lateness_ticks += late_by_ticks;

    out.deaths.push(DeathEvent {
      victim,
      at_tick,
      at_ms: now,
      lives_left,
      verdict,
      late_by_ticks,
    });
  }

  fn mint_frame(&mut self, controls: &Controls) {
    if self.clock_ms.saturating_sub(self.last_send_ms) < controls.sync_interval_ms() {
      return;
    }
    self.last_send_ms = self.clock_ms;
    self.minted.push(Frame {
      server_time_ms: self.clock_ms,
      tick: self.tick,
      ships: self.ships.clone(),
      bullets: self.bullets.clone(),
    });
  }

  fn publish(&mut self, _controls: &Controls, out: &mut Tickout) {
    out.frames.append(&mut self.minted);
    out.deaths.append(&mut self.deferred);
  }

  fn steer_bots(&mut self, controls: &Controls) {
    if !controls.bots {
      for seat in 0..self.ships.len() {
        if !self.human.get(seat).copied().unwrap_or(false) {
          self.ships[seat].dir = Dir8::Still;
        }
      }
      return;
    }
    // Dodges the nearest bullet, which is enough to keep a bot alive long
    // enough to be a target and deliberately no cleverer: a bot good enough to
    // be interesting would make every number a fact about the bot.
    for seat in 0..self.ships.len() {
      if self.human.get(seat).copied().unwrap_or(false) || !self.ships[seat].alive {
        continue;
      }
      let me = self.ships[seat].pos;
      let nearest = self
        .scratch
        .iter()
        .filter(|b| b.pos.dist(me) < 90.0)
        .min_by(|a, b| a.pos.dist(me).total_cmp(&b.pos.dist(me)));
      let dir = match nearest {
        Some(bullet) => {
          let away = me.sub(bullet.pos);
          Dir8::from_axes(sign(away.x), sign(away.y))
        }
        None => {
          // Drift back to the lower middle, where there is room to dodge.
          let home = V2::new(FIELD_W * 0.5, FIELD_H - 80.0);
          let to = home.sub(me);
          if to.len() < 20.0 { Dir8::Still } else { Dir8::from_axes(sign(to.x), sign(to.y)) }
        }
      };
      self.ships[seat].dir = dir;
    }
  }

  /// The live curtain, for drawing and for tests. Never sent.
  pub fn curtain(&self) -> &[Bullet] {
    &self.scratch
  }

  pub fn input_verdicts(&self) -> Vec<(u64, u64, u64, u64, Option<i64>)> {
    self
      .moves
      .iter()
      .zip(self.events.iter())
      .map(|(m, e)| {
        let (mc, ma) = m.rejected_split();
        let (ec, ea) = e.rejected_split();
        (
          m.accepted() + e.accepted(),
          m.late() + e.late(),
          mc + ec,
          ma + ea,
          e.last_reject_margin().or_else(|| m.last_reject_margin()),
        )
      })
      .collect()
  }
}

fn sign(v: f32) -> i32 {
  if v > 4.0 {
    1
  } else if v < -4.0 {
    -1
  } else {
    0
  }
}
