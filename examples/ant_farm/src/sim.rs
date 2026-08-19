//! The colony. Struct-of-arrays because the step visits every ant every tick
//! and the tick is the ceiling this example exists to measure.

use plaza_server_utils::relevance::{CellSpace, CellTable, GridQuantizer};

use crate::protocol::{CELL, EXTENT};

/// How far an ant walks in world units per second.
pub const SPEED: f32 = 24.0;

/// Inside this distance of the nest a carried crumb is delivered.
pub const NEST_REACH: f32 = 12.0;

/// Food a site cell starts with and regrows toward.
pub const SITE_FOOD: u16 = 4000;

pub struct Colony {
  pub x: Vec<f32>,
  pub y: Vec<f32>,
  hx: Vec<f32>,
  hy: Vec<f32>,
  carrying: Vec<bool>,
  site: Vec<u16>,
  pub nest: (f32, f32),
  pub sites: Vec<(f32, f32)>,
  pub food: CellTable<u16>,
  pub delivered: u64,
  space: CellSpace,
  extent: f32,
  rng: u32,
}

pub fn board(extent: f32) -> CellSpace {
  CellSpace::new(GridQuantizer::new((0.0, 0.0), CELL), extent)
}

impl Colony {
  pub fn new(population: usize, extent: f32, site_count: usize, seed: u32) -> Self {
    let space = board(extent);
    let mut rng = seed.max(1);
    let nest = (extent * 0.5, extent * 0.5);

    let mut sites = Vec::with_capacity(site_count);
    let mut food = CellTable::new(space.clone());
    for _ in 0..site_count.max(1) {
      let sx = margin_clamp(next_f32(&mut rng) * extent, extent);
      let sy = margin_clamp(next_f32(&mut rng) * extent, extent);
      sites.push((sx, sy));
      if let Some(amount) = food.at_mut(sx, sy) {
        *amount = SITE_FOOD;
      }
    }

    let mut colony = Self {
      x: Vec::with_capacity(population),
      y: Vec::with_capacity(population),
      hx: Vec::with_capacity(population),
      hy: Vec::with_capacity(population),
      carrying: vec![false; population],
      site: Vec::with_capacity(population),
      nest,
      sites,
      food,
      delivered: 0,
      space,
      extent,
      rng,
    };

    for _ in 0..population {
      let a = next_f32(&mut colony.rng) * std::f32::consts::TAU;
      let r = next_f32(&mut colony.rng).sqrt() * extent * 0.05;
      colony.x.push(margin_clamp(nest.0 + a.cos() * r, extent));
      colony.y.push(margin_clamp(nest.1 + a.sin() * r, extent));
      colony.hx.push(a.cos());
      colony.hy.push(a.sin());
      let pick = (next_u32(&mut colony.rng) as usize) % colony.sites.len();
      colony.site.push(pick as u16);
    }
    colony
  }

  pub fn len(&self) -> usize {
    self.x.len()
  }

  pub fn is_empty(&self) -> bool {
    self.x.is_empty()
  }

  pub fn space(&self) -> &CellSpace {
    &self.space
  }

  pub fn extent(&self) -> f32 {
    self.extent
  }

  /// One tick for every ant: steer, walk, pick up, deliver.
  pub fn step(&mut self, dt: f32) {
    let stride = SPEED * dt;
    let nest = self.nest;
    let sites = std::mem::take(&mut self.sites);

    for i in 0..self.x.len() {
      let (px, py) = (self.x[i], self.y[i]);
      let carrying = self.carrying[i];
      let (tx, ty) = if carrying {
        nest
      } else {
        sites[self.site[i] as usize]
      };

      let (dx, dy) = (tx - px, ty - py);
      let d2 = dx * dx + dy * dy;

      if carrying {
        if d2 < NEST_REACH * NEST_REACH {
          self.carrying[i] = false;
          self.delivered += 1;
        }
      } else if let Some(amount) = self.food.at_mut(px, py) {
        if *amount > 0 {
          *amount -= 1;
          self.carrying[i] = true;
        } else if d2 < CELL * CELL {
          // Standing on an exhausted site: forage somewhere else.
          let pick = (next_u32(&mut self.rng) as usize) % sites.len();
          self.site[i] = pick as u16;
        }
      }

      // Steer: lean the heading toward the target, keep it unit length.
      if d2 > 1.0 {
        let inv = 1.0 / d2.sqrt();
        let (mut nx, mut ny) = (
          self.hx[i] * 0.9 + dx * inv * 0.1,
          self.hy[i] * 0.9 + dy * inv * 0.1,
        );
        let n2 = nx * nx + ny * ny;
        if n2 > 1e-6 {
          let renorm = 1.0 / n2.sqrt();
          nx *= renorm;
          ny *= renorm;
        }
        self.hx[i] = nx;
        self.hy[i] = ny;
      }

      // A pinch of wander so the column is a stream, not a rail.
      if next_u32(&mut self.rng) & 0x3f == 0 {
        let spin = if next_u32(&mut self.rng) & 1 == 0 { 0.35f32 } else { -0.35 };
        let (s, c) = spin.sin_cos();
        let (hx, hy) = (self.hx[i], self.hy[i]);
        self.hx[i] = hx * c - hy * s;
        self.hy[i] = hx * s + hy * c;
      }

      let mut nx = px + self.hx[i] * stride;
      let mut ny = py + self.hy[i] * stride;
      if nx < CELL || nx > self.extent - CELL {
        self.hx[i] = -self.hx[i];
        nx = margin_clamp(nx, self.extent);
      }
      if ny < CELL || ny > self.extent - CELL {
        self.hy[i] = -self.hy[i];
        ny = margin_clamp(ny, self.extent);
      }
      self.x[i] = nx;
      self.y[i] = ny;
    }

    self.sites = sites;
    self.regrow();
  }

  /// Sites regrow a little each tick, so the colony never starves flat.
  fn regrow(&mut self) {
    for _ in 0..4 {
      let pick = (next_u32(&mut self.rng) as usize) % self.sites.len();
      let (sx, sy) = self.sites[pick];
      if let Some(amount) = self.food.at_mut(sx, sy) {
        *amount = (*amount + 8).min(SITE_FOOD);
      }
    }
  }
}

fn margin_clamp(v: f32, extent: f32) -> f32 {
  v.clamp(CELL, extent - CELL)
}

fn next_u32(rng: &mut u32) -> u32 {
  let mut v = *rng;
  v ^= v << 13;
  v ^= v >> 17;
  v ^= v << 5;
  *rng = v;
  v
}

fn next_f32(rng: &mut u32) -> f32 {
  (next_u32(rng) >> 8) as f32 / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ants_stay_on_the_board() {
    let mut colony = Colony::new(2000, EXTENT, 8, 7);
    for _ in 0..300 {
      colony.step(1.0 / 30.0);
    }
    for i in 0..colony.len() {
      assert!(colony.x[i] >= 0.0 && colony.x[i] <= EXTENT);
      assert!(colony.y[i] >= 0.0 && colony.y[i] <= EXTENT);
    }
  }

  #[test]
  fn foraging_delivers_food_to_the_nest() {
    let mut colony = Colony::new(2000, 512.0, 8, 11);
    for _ in 0..1200 {
      colony.step(1.0 / 30.0);
    }
    assert!(
      colony.delivered > 0,
      "a colony this dense should have delivered something in forty seconds"
    );
  }

  #[test]
  fn the_default_board_is_a_u16_of_cells() {
    let space = board(EXTENT);
    assert!(space.len() <= u16::MAX as usize + 1, "cell indices must fit the wire's u16");
  }
}
