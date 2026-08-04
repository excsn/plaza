//! The play field, the ships, and the settings that decide who may say you
//! died.

use serde::{Deserialize, Serialize};

pub const SIM_STEP_MS: u64 = 16;

pub const FIELD_W: f32 = 420.0;
pub const FIELD_H: f32 = 600.0;

/// Deliberately small. A shmup is dodged by pixels, and a hitbox the size of
/// the sprite would make the whole example about tolerance rather than about
/// timing.
pub const SHIP_R: f32 = 2.5;
pub const SHIP_SPEED: f32 = 190.0;
pub const SHIP_LIVES: u32 = 3;
pub const INVULN_MS: u64 = 1400;

pub const ENEMY_BULLET_R: f32 = 4.0;
pub const PLAYER_BULLET_R: f32 = 3.0;
pub const PLAYER_BULLET_SPEED: f32 = 460.0;
pub const PLAYER_FIRE_COOLDOWN_MS: u64 = 110;

pub const EMITTER_R: f32 = 14.0;
pub const EMITTER_HEALTH: i32 = 8;

pub const MAX_SEATS: usize = 4;

pub type PlayerId = u8;
pub type WaveId = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct V2 {
  pub x: f32,
  pub y: f32,
}

impl V2 {
  pub const ZERO: V2 = V2 { x: 0.0, y: 0.0 };

  pub const fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }

  pub fn add(self, o: V2) -> V2 {
    V2::new(self.x + o.x, self.y + o.y)
  }

  pub fn sub(self, o: V2) -> V2 {
    V2::new(self.x - o.x, self.y - o.y)
  }

  pub fn scale(self, k: f32) -> V2 {
    V2::new(self.x * k, self.y * k)
  }

  pub fn len(self) -> f32 {
    (self.x * self.x + self.y * self.y).sqrt()
  }

  pub fn dist(self, o: V2) -> f32 {
    self.sub(o).len()
  }

  pub fn lerp(self, o: V2, t: f32) -> V2 {
    self.add(o.sub(self).scale(t))
  }

  pub fn from_angle(radians: f32) -> V2 {
    V2::new(radians.cos(), radians.sin())
  }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir8 {
  #[default]
  Still,
  N,
  Ne,
  E,
  Se,
  S,
  Sw,
  W,
  Nw,
}

impl Dir8 {
  pub fn unit(self) -> V2 {
    const D: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match self {
      Dir8::Still => V2::ZERO,
      Dir8::N => V2::new(0.0, -1.0),
      Dir8::Ne => V2::new(D, -D),
      Dir8::E => V2::new(1.0, 0.0),
      Dir8::Se => V2::new(D, D),
      Dir8::S => V2::new(0.0, 1.0),
      Dir8::Sw => V2::new(-D, D),
      Dir8::W => V2::new(-1.0, 0.0),
      Dir8::Nw => V2::new(-D, -D),
    }
  }

  pub fn from_axes(x: i32, y: i32) -> Dir8 {
    match (x.signum(), y.signum()) {
      (0, 0) => Dir8::Still,
      (0, -1) => Dir8::N,
      (1, -1) => Dir8::Ne,
      (1, 0) => Dir8::E,
      (1, 1) => Dir8::Se,
      (0, 1) => Dir8::S,
      (-1, 1) => Dir8::Sw,
      (-1, 0) => Dir8::W,
      (-1, -1) => Dir8::Nw,
      _ => Dir8::Still,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ship {
  pub id: PlayerId,
  pub pos: V2,
  pub dir: Dir8,
  pub lives: u32,
  pub alive: bool,
  /// Server time until which contact does nothing. Without it a ship reappears
  /// inside the curtain that just killed it and loses every life in one second.
  pub invuln_until_ms: u64,
  pub fire_ready_at_ms: u64,
  pub score: u32,
  pub bot: bool,
}

impl Ship {
  pub fn spawn(id: PlayerId) -> Self {
    Self {
      id,
      pos: V2::new(FIELD_W * (0.2 + 0.2 * id as f32), FIELD_H - 70.0),
      dir: Dir8::Still,
      lives: SHIP_LIVES,
      alive: true,
      invuln_until_ms: 0,
      fire_ready_at_ms: 0,
      score: 0,
      bot: true,
    }
  }
}

/// A bullet a player fired. **Not derivable**: it exists because a human
/// pressed a key at a moment nothing can predict, so every one of these costs
/// bytes on the wire. The comparison this example is built around.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerBullet {
  pub id: u32,
  pub owner: PlayerId,
  pub pos: V2,
}

/// Who is allowed to say a ship was hit.
///
/// The question no other example in this repository asks, because it is the
/// only one where the correction has no ease and no undo: a position can be
/// smoothed towards the truth over a few frames, and a death cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeathRule {
  /// The server decides, against the ship position it holds. Correct, and it
  /// kills you a round trip after you watched the bullet miss.
  ServerOnly,
  /// The ship decides. What shipped co-op shmups actually do, and it feels
  /// perfect because it is judged against exactly what the player saw.
  ClientDeclares,
  /// The ship declares and the server recomputes the curtain at the tick that
  /// was named. Only possible because the curtain is a function of the tick.
  ServerConfirms,
}

impl DeathRule {
  pub fn label(self) -> &'static str {
    match self {
      DeathRule::ServerOnly => "the server decides",
      DeathRule::ClientDeclares => "the ship decides",
      DeathRule::ServerConfirms => "the ship declares, the server checks",
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  pub loss_pct: f32,
  pub datagram_link: bool,

  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  pub render_delay_ms: u64,

  pub death_rule: DeathRule,
  /// Stops declaring deaths, for one seat, on purpose.
  ///
  /// The fault this example injects. Under `ClientDeclares` it is an immortal
  /// ship, and the number worth watching is not that it works but how loudly it
  /// shows up in a count the server can take for free.
  pub silent_seat: bool,

  pub predict_self: bool,
  /// Draw the curtain from the closed form rather than from anything sent.
  /// Off, the field is empty, which is the cheapest possible demonstration that
  /// nothing about it crossed the wire.
  pub derive_curtain: bool,
  pub show_hitbox: bool,

  pub bots: bool,
  pub players: usize,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 90,
      jitter_ms: 15,
      loss_pct: 0.0,
      datagram_link: false,

      sync_hz: 20,
      playout_delay_ms: 100,
      input_max_late_ticks: 4,
      input_max_early_ticks: 30,
      render_delay_ms: 100,

      death_rule: DeathRule::ServerConfirms,
      silent_seat: false,

      predict_self: true,
      derive_curtain: true,
      show_hitbox: true,

      bots: true,
      players: 2,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }

  pub fn playable_one_way_ms(&self) -> u64 {
    self.playout_delay_ms + self.input_max_late_ticks * SIM_STEP_MS
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_ship_is_smaller_than_the_bullets_it_dodges() {
    // The genre's whole feel. A hitbox the size of the sprite turns the
    // example into a question about tolerance rather than about timing, and
    // every number here would then be measuring the tolerance.
    assert!(SHIP_R < ENEMY_BULLET_R);
  }

  #[test]
  fn every_seat_spawns_inside_the_field() {
    for id in 0..MAX_SEATS as PlayerId {
      let ship = Ship::spawn(id);
      assert!(ship.pos.x > 0.0 && ship.pos.x < FIELD_W, "{:?}", ship.pos);
      assert!(ship.pos.y > 0.0 && ship.pos.y < FIELD_H, "{:?}", ship.pos);
    }
  }
}
