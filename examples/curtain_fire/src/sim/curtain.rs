//! The curtain, as a function of the tick.
//!
//! Nothing in this file is stored, stepped or sent. Every enemy bullet on the
//! screen is computed from a wave announcement of about twenty bytes plus one
//! small op for each emitter that has been shot down, and it is computed the
//! same way on the server and on every client.
//!
//! That is the point of the example and it is also its one hard constraint:
//! **no accumulation anywhere**. A bullet's position is `spawn + velocity *
//! age`, evaluated fresh, never integrated. An integrated curtain would drift
//! apart on two machines and there would be nothing to notice it with, because
//! nothing about it is ever compared.

use serde::{Deserialize, Serialize};

use crate::sim::types::{ENEMY_BULLET_R, EMITTER_R, FIELD_H, FIELD_W, SIM_STEP_MS, V2, WaveId};

/// How long a bullet lives before it is gone, whatever it is doing.
const BULLET_LIFE_TICKS: u64 = 260;

/// Ticks between one emitter's salvos.
const EMIT_PERIOD: u64 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pattern {
  /// One bullet per period, the angle advancing by a fixed step. The classic,
  /// and the one where a small arithmetic difference is instantly visible as
  /// an arm of the spiral bending.
  Spiral,
  /// A burst spread evenly around the circle.
  Ring,
  /// A burst spread across a downward arc.
  Fan,
}

impl Pattern {
  pub fn label(self) -> &'static str {
    match self {
      Pattern::Spiral => "spiral",
      Pattern::Ring => "ring",
      Pattern::Fan => "fan",
    }
  }

  /// Bullets released at once.
  ///
  /// A ring that released one bullet per period would be a spiral. The first
  /// draft did exactly that and produced a field of forty bullets, which is a
  /// shooting gallery rather than a curtain, and every byte comparison in the
  /// example was quietly measuring the wrong thing.
  pub const fn salvo(self) -> u64 {
    match self {
      Pattern::Spiral => 2,
      Pattern::Ring => 12,
      Pattern::Fan => 7,
    }
  }
}

/// One gun. Its position is closed form too: it enters at the top and descends
/// at a constant rate, so nothing about an emitter has to be sent per frame
/// either.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Emitter {
  pub x: f32,
  pub entry_y: f32,
  pub drift: f32,
  /// Where in the emit cycle this gun starts, so a row of them does not fire
  /// as one wall.
  pub phase: u64,
  pub bullet_speed: f32,
  pub arm: u8,
}

/// A wave: everything needed to draw thousands of bullets.
///
/// This whole struct crosses the wire once. It is the entire cost of the
/// derivable half.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wave {
  pub id: WaveId,
  pub pattern: Pattern,
  pub seed: u32,
  pub start_tick: u64,
  pub end_tick: u64,
  pub emitters: Vec<Emitter>,
}

/// When an emitter stopped firing, which is the one thing about the curtain
/// that is not a function of the tick alone.
///
/// A kill depends on a player bullet, which depends on a human, so it cannot be
/// derived. Naming the tick keeps everything downstream of it derivable anyway:
/// both sides cut the same emitter's output at the same instant, and one small
/// op replaces every bullet that would otherwise have to be described.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Downed {
  pub wave: WaveId,
  pub arm: u8,
  pub tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bullet {
  pub pos: V2,
  pub wave: WaveId,
  pub arm: u8,
  pub index: u64,
}

/// Where an emitter is at a tick, or `None` once it has left the field.
pub fn emitter_at(wave: &Wave, emitter: &Emitter, tick: u64) -> Option<V2> {
  if tick < wave.start_tick || tick >= wave.end_tick {
    return None;
  }
  let age = (tick - wave.start_tick) as f32 * SIM_STEP_MS as f32 / 1000.0;
  let y = emitter.entry_y + emitter.drift * age;
  (y < FIELD_H + EMITTER_R).then_some(V2::new(emitter.x, y))
}

/// The angle of slot `slot` of salvo `salvo` from this emitter.
fn angle_of(wave: &Wave, emitter: &Emitter, salvo: u64, slot: u64) -> f32 {
  use std::f32::consts::{PI, TAU};
  // Derived from the seed and the arm rather than stored, so a wave stays a
  // couple of hundred bytes however many bullets it turns into.
  let spin = ((wave.seed ^ (emitter.arm as u32 * 0x9E37)) % 1000) as f32 / 1000.0;
  let count = wave.pattern.salvo();
  match wave.pattern {
    Pattern::Spiral => spin * TAU + salvo as f32 * 0.41 + slot as f32 * PI,
    Pattern::Ring => spin * TAU + salvo as f32 * 0.17 + slot as f32 * (TAU / count as f32),
    // Downward, spread across a third of a turn, sweeping slowly.
    Pattern::Fan => PI * 0.5 - PI / 6.0 + slot as f32 * (PI / 3.0 / (count - 1) as f32) + (salvo as f32 * 0.09 + spin).sin() * 0.25,
  }
}

/// Every enemy bullet alive at `tick`, appended to `out`.
///
/// Takes the buffer rather than returning one: this is called every frame on
/// the client and every step on the server, and allocating a thousand-element
/// vector each time is the difference between the closed form being free and
/// merely being cheap.
pub fn curtain_at(waves: &[Wave], downed: &[Downed], tick: u64, out: &mut Vec<Bullet>) {
  out.clear();
  for wave in waves {
    if tick < wave.start_tick {
      continue;
    }
    for emitter in &wave.emitters {
      // The one piece of state, and it is a *cut-off* rather than a position:
      // an emitter shot down at tick T contributes exactly the bullets it had
      // already fired, on both machines, for ever.
      let stop = downed
        .iter()
        .find(|d| d.wave == wave.id && d.arm == emitter.arm)
        .map(|d| d.tick)
        .unwrap_or(u64::MAX)
        .min(wave.end_tick);

      let count = wave.pattern.salvo();
      // Only salvos still inside a bullet's lifetime can contribute, so the
      // loop starts at the oldest live one rather than at zero. Without this
      // the cost of evaluating a wave grows for as long as the wave lasts,
      // which would make the closed form slower than the thing it replaced.
      let base = wave.start_tick + emitter.phase;
      let newest = tick.saturating_sub(base) / EMIT_PERIOD;
      let oldest = tick.saturating_sub(base).saturating_sub(BULLET_LIFE_TICKS) / EMIT_PERIOD;

      for salvo in oldest..=newest {
        let spawn = base + salvo * EMIT_PERIOD;
        if spawn >= stop || spawn > tick {
          break;
        }
        let age = tick - spawn;
        if age >= BULLET_LIFE_TICKS {
          continue;
        }
        let Some(origin) = emitter_at(wave, emitter, spawn) else { continue };
        let secs = age as f32 * SIM_STEP_MS as f32 / 1000.0;
        for slot in 0..count {
          let dir = V2::from_angle(angle_of(wave, emitter, salvo, slot));
          let pos = origin.add(dir.scale(emitter.bullet_speed * secs));
          if pos.x < -16.0 || pos.x > FIELD_W + 16.0 || pos.y < -16.0 || pos.y > FIELD_H + 16.0 {
            continue;
          }
          out.push(Bullet {
            pos,
            wave: wave.id,
            arm: emitter.arm,
            index: salvo * count + slot,
          });
        }
      }
    }
  }
}

/// Whether a ship of radius `r` at `pos` is touching the curtain at `tick`.
///
/// The whole reason the death question has three answers: both ends can call
/// this, with the same arguments, and get the same result. What they disagree
/// about is never the curtain, only where the ship was.
pub fn contact(waves: &[Wave], downed: &[Downed], tick: u64, pos: V2, r: f32, scratch: &mut Vec<Bullet>) -> bool {
  curtain_at(waves, downed, tick, scratch);
  let reach = r + ENEMY_BULLET_R;
  scratch.iter().any(|b| b.pos.dist(pos) <= reach)
}

/// Builds the next wave. Deterministic in the seed, so a run repeats.
pub fn make_wave(id: WaveId, seed: u32, start_tick: u64) -> Wave {
  let pattern = match id % 3 {
    0 => Pattern::Spiral,
    1 => Pattern::Fan,
    _ => Pattern::Ring,
  };
  let arms = 2 + (seed % 3) as u8;
  let emitters = (0..arms)
    .map(|arm| Emitter {
      x: FIELD_W * (arm as f32 + 1.0) / (arms as f32 + 1.0),
      entry_y: -20.0 - arm as f32 * 18.0,
      drift: 16.0 + (seed % 7) as f32,
      phase: (arm as u64 * 3) % EMIT_PERIOD,
      bullet_speed: 78.0 + (seed % 40) as f32,
      arm,
    })
    .collect();
  Wave {
    id,
    pattern,
    seed,
    start_tick,
    end_tick: start_tick + 900,
    emitters,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn wave() -> Wave {
    make_wave(0, 12345, 100)
  }

  #[test]
  fn the_curtain_is_a_function_of_the_tick_and_nothing_else() {
    // Called twice with the same arguments in a different order of operations,
    // it must produce the same field. If it ever does not, something in here
    // is accumulating, and an accumulating curtain drifts apart on two
    // machines with nothing to notice it.
    let waves = vec![wave()];
    let downed = Vec::new();
    let mut a = Vec::new();
    let mut b = Vec::new();

    curtain_at(&waves, &downed, 400, &mut a);
    for tick in 100..400 {
      curtain_at(&waves, &downed, tick, &mut b);
    }
    curtain_at(&waves, &downed, 400, &mut b);

    assert!(!a.is_empty(), "there is a curtain to compare");
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
      assert_eq!(x.pos, y.pos, "evaluating the same tick twice gave two answers");
    }
  }

  #[test]
  fn a_wave_costs_a_fixed_number_of_bytes_however_many_bullets_it_becomes() {
    // The headline. The wire cost of the derivable half does not grow with the
    // thing it describes, which is the property no amount of compressing
    // positions can reach.
    //
    // Checked per pattern rather than once, because the three differ by an
    // order of magnitude in how much curtain the same handful of bytes buys,
    // and a single sample would be a claim about whichever one it happened to
    // pick.
    let mut out = Vec::new();
    let mut best = 0;
    for id in 0..3 {
      let waves = vec![make_wave(id, 12345, 100)];
      let encoded = rmp_serde::to_vec(&waves[0]).expect("encode");
      assert!(encoded.len() < 300, "{:?} cost {} bytes", waves[0].pattern, encoded.len());

      let mut peak = 0;
      for tick in 100..1000 {
        curtain_at(&waves, &[], tick, &mut out);
        peak = peak.max(out.len());
      }
      assert!(peak > 40, "{:?} only reached {peak} bullets", waves[0].pattern);
      best = best.max(peak);
    }
    assert!(best > 400, "the densest pattern only reached {best} bullets, which is not a curtain");
  }

  #[test]
  fn an_emitter_shot_down_stops_contributing_at_the_tick_it_was_named() {
    let waves = vec![wave()];
    let arm = waves[0].emitters[0].arm;
    let downed = vec![Downed { wave: 0, arm, tick: 300 }];

    let mut with = Vec::new();
    let mut without = Vec::new();
    curtain_at(&waves, &downed, 500, &mut with);
    curtain_at(&waves, &[], 500, &mut without);
    assert!(with.len() < without.len(), "killing a gun changed nothing");
    assert!(
      with.iter().filter(|b| b.arm == arm).count() < without.iter().filter(|b| b.arm == arm).count(),
      "and it was that gun's bullets that stopped"
    );
  }

  #[test]
  fn a_death_that_has_not_arrived_yet_only_ever_adds_bullets_never_removes_them() {
    // What a client sees between an emitter dying and the op landing. It draws
    // bullets that no longer exist, which is a phantom the next op clears, and
    // never *misses* one, which would be a bullet you cannot dodge.
    let waves = vec![wave()];
    let downed = vec![Downed { wave: 0, arm: 0, tick: 300 }];
    let mut stale = Vec::new();
    let mut fresh = Vec::new();
    for tick in 300..700 {
      curtain_at(&waves, &[], tick, &mut stale);
      curtain_at(&waves, &downed, tick, &mut fresh);
      for bullet in &fresh {
        assert!(
          stale.iter().any(|s| s.wave == bullet.wave && s.arm == bullet.arm && s.index == bullet.index),
          "a client behind on deaths was missing a real bullet at tick {tick}"
        );
      }
    }
  }

  #[test]
  fn contact_is_the_same_question_on_both_ends() {
    let waves = vec![wave()];
    let mut scratch = Vec::new();
    let mut bullets = Vec::new();
    curtain_at(&waves, &[], 420, &mut bullets);
    let target = bullets.first().copied().expect("a bullet to sit on");
    assert!(contact(&waves, &[], 420, target.pos, crate::sim::types::SHIP_R, &mut scratch));
    assert!(!contact(&waves, &[], 420, V2::new(-500.0, -500.0), crate::sim::types::SHIP_R, &mut scratch));
  }

  #[test]
  fn an_emitter_leaves_the_field_rather_than_descending_for_ever() {
    let wave = wave();
    let emitter = wave.emitters[0];
    assert!(emitter_at(&wave, &emitter, wave.start_tick + 1).is_some());
    assert!(emitter_at(&wave, &emitter, wave.end_tick).is_none(), "the wave has an end");
  }
}
