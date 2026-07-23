//! One player's client.
//!
//! Under [`SyncMode::Field`] it holds the whole pellet set and integrates it
//! locally every frame from the hole states it was told, applying the rotating
//! corrections as they arrive. Under [`SyncMode::Particles`] it does no physics
//! at all and simply draws the positions it was sent.
//!
//! The difference is the point. Field sync costs a few hole states per packet and
//! makes the client do the work; particle sync costs thousands of positions and
//! makes the client do nothing.

use std::collections::BTreeMap;

use crate::sim::types::{step_pellet, Attractor, BlackHole, Controls, Packet, Pellet, PelletId, PlayerId, SyncMode, Vec2, SIM_DT};

pub struct Client {
  pub id: PlayerId,
  /// The holes told exactly: the ones near enough to draw as players.
  pub holes: Vec<BlackHole>,
  /// The field it integrates against: those holes plus any cluster stand-ins.
  ///
  /// Cached rather than rebuilt per step, and flat rather than two lists, because
  /// the integrator must not care which entries are real holes and which are
  /// stand-ins for a distant crowd. If it could tell, aggregation would be a
  /// second physics path and the two sides would no longer be running the same
  /// rule.
  field: Vec<Attractor>,
  /// Locally integrated pellets (field mode), or last-received positions
  /// (particle mode).
  pellets: BTreeMap<PelletId, Pellet>,
  now_ms: u64,
  /// Leftover time, so the local integration advances in the *same fixed step*
  /// the server uses. Integrating with the raw frame delta instead is a slightly
  /// different timestep, and in a divergent system that alone pulls the two
  /// simulations apart.
  sim_accum_ms: u64,
  /// Corrections applied, so the cost of staying converged is visible.
  pub corrections_applied: u64,
}

impl Client {
  pub fn new(id: PlayerId) -> Self {
    Self {
      id,
      holes: Vec::new(),
      field: Vec::new(),
      pellets: BTreeMap::new(),
      now_ms: 0,
      sim_accum_ms: 0,
      corrections_applied: 0,
    }
  }

  pub fn known_pellets(&self) -> usize {
    self.pellets.len()
  }

  /// How many point sources this client integrates every pellet against. The
  /// number aggregation exists to hold down, and the one that decides whether the
  /// per-machine cost is affordable.
  pub fn field_size(&self) -> usize {
    self.field.len()
  }

  /// Total pull the client believes the world exerts.
  pub fn field_weight(&self) -> f32 {
    self.field.iter().map(|a| a.pull).sum()
  }

  pub fn on_packet(&mut self, packet: &Packet, recv_ms: u64, controls: &Controls) {
    self.now_ms = recv_ms;

    // The field: the exact holes it was told, plus the cluster stand-ins for the
    // ones it was not. If the server *culled* instead of aggregating, there are
    // no stand-ins and the client integrates physics that is missing forces,
    // which is exactly what the cull toggle is for.
    self.holes = packet.holes.iter().map(|(_, h)| *h).collect();
    self.field.clear();
    self.field.extend(self.holes.iter().filter(|h| h.alive).map(|h| h.as_attractor()));
    self.field.extend_from_slice(&packet.clusters);

    for id in &packet.swallowed {
      self.pellets.remove(id);
    }
    for spawn in &packet.spawned {
      self.pellets.insert(
        spawn.id,
        Pellet {
          pos: spawn.pos,
          vel: spawn.vel,
        },
      );
    }

    // Corrections are authoritative. Under field sync only a rotating slice
    // arrives; under particle sync this is the entire visible set.
    let age_ms = recv_ms.saturating_sub(packet.server_time_ms);
    let catch_up_steps = ((age_ms as f32 / 1000.0) / SIM_DT) as u32;
    let field = self.field.clone();

    for c in &packet.corrections {
      let mut pellet = Pellet { pos: c.pos, vel: c.vel };
      if controls.mode == SyncMode::Field {
        // The correction describes the past. Integrate it forward by its own age
        // with the shared step, so it lands on *now* rather than dragging the
        // pellet backwards by a latency every time it is refreshed.
        for _ in 0..catch_up_steps.min(240) {
          step_pellet(&mut pellet, &field, SIM_DT);
        }
      }
      self.pellets.insert(c.id, pellet);
      self.corrections_applied += 1;
    }
  }

  /// Advances the local simulation. Under field sync this is what moves every
  /// pellet; under particle sync the client holds still between packets.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.now_ms += dt_ms;
    if controls.mode != SyncMode::Field {
      return;
    }
    // Fixed step, matching the server exactly. Same rule *and* same timestep, or
    // the two integrations are not the same simulation at all.
    self.sim_accum_ms += dt_ms;
    let step_ms = (SIM_DT * 1000.0) as u64;
    let field = self.field.clone();
    while self.sim_accum_ms >= step_ms {
      self.sim_accum_ms -= step_ms;
      for pellet in self.pellets.values_mut() {
        step_pellet(pellet, &field, SIM_DT);
      }
    }
  }

  /// Where this client draws each pellet it knows about.
  pub fn render(&self) -> Vec<(PelletId, Vec2)> {
    self.pellets.iter().map(|(id, p)| (*id, p.pos)).collect()
  }

  pub fn pellet(&self, id: PelletId) -> Option<&Pellet> {
    self.pellets.get(&id)
  }
}
