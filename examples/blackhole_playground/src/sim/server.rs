//! The authoritative server: integrates the field, decides who swallowed what,
//! and settles player collisions.
//!
//! Note what it is authoritative *for*. Pellet motion is not a decision, it is a
//! consequence of the field, and every client can derive it. Swallowing and
//! collisions are decisions, and only the server makes them.

use plaza_client_utils::{FixedTimestep, Periodic};
use plaza_server_utils::aggregate::{AggregateTree, WeightedPoint};
use plaza_server_utils::relevance::{TierBoundary, VisibilitySet};

use crate::sim::types::{
  exact_field, step_pellet, Attractor, BlackHole, Controls, Packet, Pellet, PelletCorrection, PelletId, PelletSpawn, PlayerId, SyncMode, Vec2, ARENA_H, ARENA_W, CONTACT_DRAIN_BASE, CONTACT_DRAIN_PRESS,
  DASH_COOLDOWN_MS, DASH_DURATION_MS, DASH_SPEED_MULT, ELIMINATION_MASS, HOLE_PULL_SCALE, HOLE_SPEED, MAX_HOLE_PULL, PELLET_MASS, RESPAWN_DELAY_MS, SIM_DT, SIM_STEP_MS, START_MASS, VIEW_RADIUS,
};

/// Who is driving a hole this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Seat {
  /// A person, with the direction they asked for.
  Steered(Vec2),
  /// Nobody, so the scripted drift takes it.
  #[default]
  Bot,
}

#[derive(Clone, Debug)]
pub struct Server {
  pub holes: Vec<BlackHole>,
  pub pellets: Vec<Pellet>,

  clock_ms: u64,
  /// Simulation time, spent in whole fixed steps. The step has to be the same one
  /// the client uses, which is why it is taken from here rather than passed in.
  sim: FixedTimestep,
  /// When to build the next round of packets. Its interval is a live setting, so
  /// dragging the send-rate slider takes effect from now rather than restarting
  /// the period.
  sync: Periodic,
  /// Rotating cursor, so corrections cover every pellet in turn rather than
  /// refreshing the same few forever.
  correction_cursor: usize,
  /// Dash state per player: when the current burst ends, and when the next is
  /// available.
  dash_until_ms: Vec<u64>,
  dash_ready_ms: Vec<u64>,
  /// When an eliminated player comes back.
  respawn_at_ms: Vec<Option<u64>>,
  /// Who each player is currently pressed against, for readouts and for knowing
  /// a squeeze is in progress.
  contact_with: Vec<Option<usize>>,
  pub eliminations: u64,

  /// Which pellets each player's correction stream currently covers, in
  /// `SyncMode::Particles`: the memory the near boundary's hysteresis judges
  /// against. Keyed by pellet slot, which is stable (slots are recycled in
  /// place, never removed).
  near_pellets: Vec<VisibilitySet>,
  swallowed: Vec<PelletId>,
  spawned: Vec<PelletSpawn>,
  pub swallow_count: u64,
  pub collision_count: u64,
  /// Total mass removed by contact, so "contact drains" is measurable rather
  /// than inferred from a total that pellets are simultaneously inflating.
  pub mass_drained: f32,
  /// Pellets eaten per player. Deliberately separate from mass: mass is the
  /// physical stat, capped and reduced by collisions, while the score is what you
  /// achieved and only goes up. Without the split a player at the mass ceiling
  /// has nothing left to play for.
  pub scores: Vec<u32>,
}

/// The correction stream's near boundary in [`SyncMode::Particles`], with
/// hysteresis: admitted at the view radius, kept until the pellet draw cutoff
/// (`render.rs` draws pellets to 1.15x the radius), so a pellet wobbling on
/// the screen edge does not pop in and out of the stream every packet. The
/// band also means everything actually being drawn keeps its corrections.
const PELLET_NEAR: TierBoundary = TierBoundary::new(VIEW_RADIUS, VIEW_RADIUS * 1.15);

impl Server {
  pub fn new(pellet_count: usize, player_count: usize) -> Self {
    let holes = (0..player_count)
      .map(|p| BlackHole {
        pos: hole_start(p),
        mass: START_MASS,
        alive: true,
      })
      .collect();
    let pellets = (0..pellet_count).map(|i| spawn_pellet(i as u32)).collect();

    Self {
      holes,
      pellets,
      clock_ms: 0,
      sim: FixedTimestep::from_step_ms(SIM_STEP_MS),
      sync: Periodic::new(1),
      correction_cursor: 0,
      dash_until_ms: vec![0; player_count],
      dash_ready_ms: vec![0; player_count],
      respawn_at_ms: vec![None; player_count],
      contact_with: vec![None; player_count],
      eliminations: 0,
      near_pellets: (0..player_count).map(|_| VisibilitySet::with_capacity(pellet_count as u32)).collect(),
      swallowed: Vec::new(),
      spawned: Vec::new(),
      swallow_count: 0,
      collision_count: 0,
      mass_drained: 0.0,
      scores: vec![0; player_count],
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  /// Advances by `dt_ms`; `local_input` steers player 0, the rest drift.
  /// Whether a player's dash is off cooldown, for a readout.
  pub fn dash_ready(&self, player: usize) -> bool {
    self.clock_ms >= self.dash_ready_ms[player]
  }

  /// Whether a player is mid-dash right now, for the burst effect.
  pub fn is_dashing(&self, player: usize) -> bool {
    self.clock_ms < self.dash_until_ms[player]
  }

  /// Starts a dash if it is available.
  pub fn try_dash(&mut self, player: usize) {
    if self.clock_ms >= self.dash_ready_ms[player] && self.holes[player].alive {
      self.dash_until_ms[player] = self.clock_ms + DASH_DURATION_MS;
      self.dash_ready_ms[player] = self.clock_ms + DASH_COOLDOWN_MS;
    }
  }

  /// Advances with one human at seat 0 and bots everywhere else.
  ///
  /// The offline shape, kept so the headless tests and the single-process
  /// playground are unchanged by networking.
  pub fn advance(&mut self, dt_ms: u64, local_input: Vec2, controls: &Controls) -> Vec<(PlayerId, Packet)> {
    let mut seats = vec![Seat::Bot; self.holes.len()];
    if !seats.is_empty() {
      seats[0] = Seat::Steered(local_input);
    }
    self.advance_seats(dt_ms, &seats, controls)
  }

  /// Advances with an explicit occupant per seat, which is what a real server
  /// has: some seats are people, the rest are bots, and the set changes as
  /// players come and go.
  pub fn advance_seats(&mut self, dt_ms: u64, seats: &[Seat], controls: &Controls) -> Vec<(PlayerId, Packet)> {
    // The clock tracks *simulated* time, not wall time. They are the same thing
    // until the step cap refuses to catch up on a long stall, and a packet's
    // `server_time_ms` has to say when its state is from: a client integrates the
    // field forward by that packet's age, so a clock ahead of the state it
    // describes would make every client over-integrate.
    for step_ms in self.sim.advance(dt_ms) {
      self.clock_ms += step_ms;
      self.step(seats);
    }

    self.sync.set_interval_ms(controls.sync_interval_ms());
    if self.sync.due(dt_ms) {
      return self.build_packets(controls);
    }
    Vec::new()
  }

  fn step(&mut self, seats: &[Seat]) {
    self.respawn_due();

    let t = self.clock_ms as f32 / 1000.0;
    let clock = self.clock_ms;
    for (p, hole) in self.holes.iter_mut().enumerate() {
      if !hole.alive {
        continue;
      }
      let (dx, dy) = match seats.get(p) {
        Some(Seat::Steered(dir)) => (dir.x, dir.y),
        // An unoccupied seat drifts rather than standing still, so an arena with
        // one player in it is still a game. It also keeps the field interesting
        // enough to measure, which every existing test depends on.
        _ => hole_drift(p, t),
      };
      let speed = if clock < self.dash_until_ms[p] { HOLE_SPEED * DASH_SPEED_MULT } else { HOLE_SPEED };
      hole.pos.x = (hole.pos.x + dx * speed * SIM_DT).clamp(0.0, ARENA_W);
      hole.pos.y = (hole.pos.y + dy * speed * SIM_DT).clamp(0.0, ARENA_H);
    }
    self.attract_holes();
    self.ai_dashes();

    // The field moves the pellets. The server always integrates the *exact*
    // field: aggregation is a transmission decision, so the truth a client is
    // measured against must never be the approximation.
    let field = exact_field(&self.holes);
    for pellet in &mut self.pellets {
      step_pellet(pellet, &field, SIM_DT);
    }

    self.resolve_swallows();
    self.resolve_collisions();
  }

  /// A pellet inside a hole is gone, and the hole grows. An authoritative
  /// decision: clients are told, they do not infer it.
  fn resolve_swallows(&mut self) {
    for i in 0..self.pellets.len() {
      let pos = self.pellets[i].pos;
      let out_of_bounds = pos.x < -400.0 || pos.x > ARENA_W + 400.0 || pos.y < -400.0 || pos.y > ARENA_H + 400.0;
      let mut eaten_by: Option<usize> = None;
      for (h, hole) in self.holes.iter().enumerate() {
        // Swallowed at the core, not the rim: the fall between the two is where
        // the acceleration is visible.
        if pos.dist(hole.pos) <= hole.core_radius() {
          eaten_by = Some(h);
          break;
        }
      }
      if let Some(h) = eaten_by {
        // No ceiling: growth is damped by the curve, not stopped by a wall.
        self.holes[h].mass += PELLET_MASS;
        self.scores[h] += 1;
        self.swallow_count += 1;
      } else if !out_of_bounds {
        continue;
      }
      // Swallowed or escaped: recycle the slot with a fresh pellet.
      let fresh = spawn_pellet(self.clock_ms as u32 ^ (i as u32).wrapping_mul(2_654_435_761));
      self.pellets[i] = fresh;
      self.swallowed.push(i as PelletId);
      self.spawned.push(PelletSpawn {
        id: i as PelletId,
        pos: fresh.pos,
        vel: fresh.vel,
      });
    }
  }

  /// The holes pull on each other exactly as they pull on pellets.
  ///
  /// This is what makes a grapple a grapple: drift too close and the attraction
  /// closes the rest of the distance for you, and keeps closing it while you are
  /// draining. Walking away does not work, because the pull at contact is tuned
  /// above walking speed; a dash outruns it briefly, and the pull starts eating
  /// the gap back the moment the dash ends.
  fn attract_holes(&mut self) {
    let snapshot = self.holes.clone();
    for (a, hole) in self.holes.iter_mut().enumerate() {
      if !hole.alive {
        continue;
      }
      let (mut vx, mut vy) = (0.0f32, 0.0f32);
      for (b, other) in snapshot.iter().enumerate() {
        if a == b || !other.alive {
          continue;
        }
        let (dx, dy) = (other.pos.x - hole.pos.x, other.pos.y - hole.pos.y);
        let r2 = (dx * dx + dy * dy).max(1.0);
        let pull = (HOLE_PULL_SCALE * other.effective_mass() / r2).min(MAX_HOLE_PULL);
        let inv_r = 1.0 / r2.sqrt();
        vx += dx * inv_r * pull;
        vy += dy * inv_r * pull;
      }
      hole.pos.x = (hole.pos.x + vx * SIM_DT).clamp(0.0, ARENA_W);
      hole.pos.y = (hole.pos.y + vy * SIM_DT).clamp(0.0, ARENA_H);
    }
  }

  /// Contact between players: they press, they do not pass through.
  ///
  /// Two holes never interpenetrate. Whatever overlap the pull would have created
  /// is measured as *pressure* and then undone, leaving them exactly tangent, so
  /// what you see is two bodies squeezing rather than two circles sliding through
  /// one another. Both drain the whole time, harder the harder they are pressed,
  /// and because draining shrinks their radii they stay in contact while getting
  /// smaller. Merging happens only at the end, when one of them is finished.
  fn resolve_collisions(&mut self) {
    for slot in &mut self.contact_with {
      *slot = None;
    }

    for a in 0..self.holes.len() {
      for b in (a + 1)..self.holes.len() {
        if !self.holes[a].alive || !self.holes[b].alive {
          continue;
        }
        let (ra, rb) = (self.holes[a].radius(), self.holes[b].radius());
        let touch = ra + rb;
        let dist = self.holes[a].pos.dist(self.holes[b].pos);
        let press = touch - dist;
        if press <= 0.0 {
          continue;
        }
        self.collision_count += 1;
        self.contact_with[a] = Some(b);
        self.contact_with[b] = Some(a);

        // Pressure is the overlap that would have happened. Touching costs a
        // little; leaning on someone costs a lot.
        let depth = (press / touch).clamp(0.0, 1.0);
        let drain = (CONTACT_DRAIN_BASE + CONTACT_DRAIN_PRESS * depth) * SIM_DT;
        for h in [a, b] {
          let before = self.holes[h].mass;
          self.holes[h].mass = (before - drain).max(0.0);
          self.mass_drained += before - self.holes[h].mass;
        }

        // Undo the interpenetration completely: tangent, never overlapping.
        let (dx, dy) = (self.holes[b].pos.x - self.holes[a].pos.x, self.holes[b].pos.y - self.holes[a].pos.y);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let fix = press * 0.5;
        self.holes[a].pos.x = (self.holes[a].pos.x - dx / len * fix).clamp(0.0, ARENA_W);
        self.holes[a].pos.y = (self.holes[a].pos.y - dy / len * fix).clamp(0.0, ARENA_H);
        self.holes[b].pos.x = (self.holes[b].pos.x + dx / len * fix).clamp(0.0, ARENA_W);
        self.holes[b].pos.y = (self.holes[b].pos.y + dy / len * fix).clamp(0.0, ARENA_H);
      }
    }
    self.resolve_eliminations();
  }

  /// Drained to nothing: out, and back after a delay so the sandbox keeps going.
  fn resolve_eliminations(&mut self) {
    for p in 0..self.holes.len() {
      if self.holes[p].alive && self.holes[p].mass <= ELIMINATION_MASS {
        self.holes[p].alive = false;
        self.holes[p].mass = 0.0;
        self.respawn_at_ms[p] = Some(self.clock_ms + RESPAWN_DELAY_MS);
        self.eliminations += 1;
      }
    }
  }

  fn respawn_due(&mut self) {
    for p in 0..self.holes.len() {
      if let Some(at) = self.respawn_at_ms[p]
        && self.clock_ms >= at
      {
        self.respawn_at_ms[p] = None;
        // Not back onto the fixed start slot: by now other holes have drifted all
        // over the arena, and the start slot is as likely as anywhere to be under
        // one of them, so you would reappear inside the crowd that just ate you
        // and be grappled again before you could move. Land in open space
        // instead. Placed then marked alive, so several respawns on the same tick
        // spread out rather than stacking.
        let pos = self.open_spawn(p);
        self.holes[p] = BlackHole {
          pos,
          mass: START_MASS,
          alive: true,
        };
      }
    }
  }

  /// The emptiest spot to reappear at: of a spread of candidate points, the one
  /// whose nearest live hole is furthest away.
  ///
  /// A coarse grid is enough. The arena is large and the search only has to avoid
  /// the crowd, not find a provably optimal point, so a few dozen candidates
  /// scored against the live holes lands you somewhere with room every time.
  fn open_spawn(&self, respawning: usize) -> Vec2 {
    const COLS: usize = 6;
    const ROWS: usize = 6;
    let mut best = hole_start(respawning);
    let mut best_clearance = f32::MIN;
    for row in 0..ROWS {
      for col in 0..COLS {
        let candidate = Vec2::new(ARENA_W * (col as f32 + 0.5) / COLS as f32, ARENA_H * (row as f32 + 0.5) / ROWS as f32);
        let mut nearest = f32::MAX;
        for (h, hole) in self.holes.iter().enumerate() {
          if h == respawning || !hole.alive {
            continue;
          }
          nearest = nearest.min(candidate.dist(hole.pos));
        }
        if nearest > best_clearance {
          best_clearance = nearest;
          best = candidate;
        }
      }
    }
    best
  }

  /// The other players dash when a rival is close: enough to make contact happen
  /// without a human driving it.
  fn ai_dashes(&mut self) {
    for p in 1..self.holes.len() {
      if !self.holes[p].alive || self.clock_ms < self.dash_ready_ms[p] {
        continue;
      }
      let near = (0..self.holes.len()).any(|q| {
        q != p && self.holes[q].alive && self.holes[p].pos.dist(self.holes[q].pos) < (self.holes[p].radius() + self.holes[q].radius()) * 2.2
      });
      if near {
        self.try_dash(p);
      }
    }
  }

  fn build_packets(&mut self, controls: &Controls) -> Vec<(PlayerId, Packet)> {
    let all_holes: Vec<(PlayerId, BlackHole)> = self.holes.iter().enumerate().map(|(p, h)| (p as PlayerId, *h)).collect();
    // View-independent, so it is computed once and shared by every recipient's
    // packet rather than rebuilt per eye.
    let dashing: Vec<PlayerId> = (0..self.holes.len()).filter(|p| self.is_dashing(*p)).map(|p| p as PlayerId).collect();
    let mut out = Vec::with_capacity(self.holes.len());

    // One tree over the live field, walked once per recipient. It is built from
    // the *viewer-independent* set, which is what makes the technique affordable:
    // the O(n log n) build is paid once a tick no matter how many clients ask it
    // for a different view.
    let aggregation = (controls.aggregation_theta > 0.0).then(|| self.build_field_tree());
    let mut summaries: Vec<plaza_server_utils::aggregate::Summary> = Vec::new();

    for p in 0..self.holes.len() {
      let eye = self.holes[p].pos;

      // Three ways to answer "what field does this client get?".
      let (holes, clusters) = match &aggregation {
        // Coarsen the distant part, keep the near part exact. Nothing is dropped.
        Some((tree, live)) => {
          tree.summarize(eye.x, eye.y, controls.aggregation_theta, &mut summaries);
          let mut exact = Vec::new();
          let mut grouped = Vec::new();
          for summary in &summaries {
            if summary.count == 1 {
              // Resolvable from here, so send the real hole: the client gets an
              // avatar to draw, not just a force. Near detail comes free, because
              // a close node always fails the opening-angle test.
              let (id, hole) = all_holes[live[tree.members(summary)[0] as usize] as usize];
              exact.push((id, hole));
            } else {
              grouped.push(Attractor {
                pos: Vec2::new(summary.x, summary.y),
                pull: summary.weight,
              });
            }
          }
          (exact, grouped)
        }
        // Delete the distant part: the deliberate mistake, kept as a toggle
        // because it is what "relevance" naively applied to a simulation input
        // looks like.
        None if controls.cull_attractors => (all_holes.iter().filter(|(_, h)| h.pos.dist(eye) <= VIEW_RADIUS).copied().collect(), Vec::new()),
        // Everything, exactly.
        None => (all_holes.clone(), Vec::new()),
      };

      let mut packet = Packet {
        server_time_ms: self.clock_ms,
        holes,
        clusters,
        swallowed: self.swallowed.clone(),
        spawned: self.spawned.clone(),
        corrections: Vec::new(),
        dashing: dashing.clone(),
      };

      match controls.mode {
        // Send the field, plus a slice of corrections.
        SyncMode::Field => {
          let n = controls.corrections_per_packet.min(self.pellets.len());
          let ids: Vec<usize> = if controls.priority_corrections {
            // Divergence is not uniform: a pellet deep in a well is being
            // accelerated hardest, so tiny differences there grow fastest. Spend
            // the budget where it is actually being lost rather than sweeping
            // everything in rotation.
            let mut risk: Vec<(f32, usize)> = self
              .pellets
              .iter()
              .enumerate()
              .map(|(i, pellet)| {
                let nearest = self.holes.iter().map(|h| pellet.pos.dist(h.pos)).fold(f32::MAX, f32::min);
                (nearest, i)
              })
              .collect();
            if n < risk.len() {
              risk.select_nth_unstable_by(n, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            }
            risk.iter().take(n).map(|(_, i)| *i).collect()
          } else {
            (0..n).map(|k| (self.correction_cursor + k) % self.pellets.len()).collect()
          };

          for id in ids {
            packet.corrections.push(PelletCorrection {
              id: id as PelletId,
              pos: self.pellets[id].pos,
              vel: self.pellets[id].vel,
            });
          }
        }
        // Send every pellet the player can see, every packet, with hysteresis
        // on the boundary so edge pellets do not flap in and out of the stream.
        SyncMode::Particles => {
          let near = &mut self.near_pellets[p];
          for (id, pellet) in self.pellets.iter().enumerate() {
            let was_near = near.contains(id as u32);
            if PELLET_NEAR.admits(was_near, pellet.pos.dist(eye)) {
              near.insert(id as u32);
              packet.corrections.push(PelletCorrection {
                id: id as PelletId,
                pos: pellet.pos,
                vel: pellet.vel,
              });
            } else if was_near {
              near.remove(id as u32);
            }
          }
        }
      }

      out.push((p as PlayerId, packet));
    }

    if controls.mode == SyncMode::Field && !self.pellets.is_empty() {
      self.correction_cursor = (self.correction_cursor + controls.corrections_per_packet) % self.pellets.len();
    }
    self.swallowed.clear();
    self.spawned.clear();
    out
  }

  /// The live field as an aggregation tree, plus the player id behind each point
  /// so a resolved leaf can be sent as the hole it actually is.
  ///
  /// Weighted by [`BlackHole::effective_mass`], not raw mass, because the tree
  /// sums weights and gravity superposes the damped value. Dead holes are left
  /// out: they exert nothing, so including them would drag every centroid they
  /// sat near toward a body that is not there.
  fn build_field_tree(&self) -> (AggregateTree, Vec<PlayerId>) {
    let mut points = Vec::with_capacity(self.holes.len());
    let mut live = Vec::with_capacity(self.holes.len());
    for (p, hole) in self.holes.iter().enumerate() {
      if hole.alive {
        points.push(WeightedPoint::new(hole.pos.x, hole.pos.y, hole.effective_mass()));
        live.push(p as PlayerId);
      }
    }
    // Pinned to the arena, not fitted to the holes. Fitting re-centres the whole
    // subdivision whenever any hole moves, so clusters would re-form for reasons
    // unrelated to the holes in them and the client's field would twitch every
    // packet.
    let tree = AggregateTree::build_in(&points, (ARENA_W * 0.5, ARENA_H * 0.5), ARENA_W.max(ARENA_H), 10);
    (tree, live)
  }
}

/// A pellet enters at the arena edge with enough tangential speed to orbit
/// rather than fall straight in, which is what makes the field interesting.
fn spawn_pellet(seed: u32) -> Pellet {
  let a = (seed.wrapping_mul(2_654_435_761) % 6283) as f32 / 1000.0;
  let r = 900.0 + ((seed.wrapping_mul(40_503) >> 3) % 500) as f32;
  let (cx, cy) = (ARENA_W * 0.5, ARENA_H * 0.5);
  let pos = Vec2::new(cx + r * a.cos(), cy + r * a.sin());
  // Tangential, scaled to roughly a circular orbit for the starting mass.
  let speed = (crate::sim::types::G * START_MASS * 2.0 / r).sqrt();
  Pellet {
    pos,
    vel: Vec2::new(-a.sin() * speed, a.cos() * speed),
  }
}

/// Spreads any number of holes over the arena on a square-ish grid, so 64 start
/// apart rather than stacked on each other.
fn hole_start(p: usize) -> Vec2 {
  let per_row = 8usize;
  let (fx, fy) = ((p % per_row) as f32, (p / per_row) as f32);
  let step_x = ARENA_W / (per_row as f32 + 1.0);
  let step_y = ARENA_H / 9.0;
  Vec2::new(step_x * (fx + 1.0), (step_y * (fy + 1.0)).min(ARENA_H - 40.0))
}

fn hole_drift(p: usize, t: f32) -> (f32, f32) {
  let phase = p as f32 * 2.1;
  ((t * 0.42 + phase).cos(), (t * 0.35 + phase).sin())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_respawn_lands_clear_of_the_crowd() {
    // The annoyance this fixes: reappearing on top of the holes that just ate
    // you. Jam every rival into one corner and a respawn must go somewhere else.
    let mut server = Server::new(50, 8);
    for (i, hole) in server.holes.iter_mut().enumerate() {
      hole.pos = Vec2::new(60.0 + i as f32 * 8.0, 60.0);
      hole.alive = true;
    }
    let spot = server.open_spawn(0);
    let nearest = server
      .holes
      .iter()
      .enumerate()
      .filter(|(i, h)| *i != 0 && h.alive)
      .map(|(_, h)| spot.dist(h.pos))
      .fold(f32::MAX, f32::min);
    // A grapple bites within a few hole-radii; a fresh hole is tiny. Landing over
    // a thousand pixels from the nearest rival proves it found open space rather
    // than the pile.
    assert!(nearest > 1000.0, "respawn landed only {nearest:.0}px from the crowd");
  }
}
