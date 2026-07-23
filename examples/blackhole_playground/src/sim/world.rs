//! Ties the server, the clients, and the wire together, and measures the two
//! things this example exists to show: what sending the field costs against
//! sending the particles, and how far a locally integrated pellet drifts from
//! the truth between corrections.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::server::Server;
use crate::sim::types::{BlackHole, Controls, Packet, PelletId, PlayerId, SyncMode, Vec2, VIEW_RADIUS};

pub struct World {
  server: Server,
  clients: Vec<Client>,
  down: Vec<LatencyLink<Packet>>,
  rng: Rng,
  wall_ms: u64,

  bytes_sent: u64,
  packets_sent: u64,
  corrections_sent: u64,
  hole_bytes: u64,
}

impl World {
  pub fn new(controls: &Controls, player_count: usize, seed: u64) -> Self {
    Self {
      server: Server::new(controls.pellet_count, player_count),
      clients: (0..player_count).map(|p| Client::new(p as PlayerId)).collect(),
      down: (0..player_count).map(|_| LatencyLink::new()).collect(),
      rng: Rng::new(seed),
      wall_ms: 0,
      bytes_sent: 0,
      packets_sent: 0,
      corrections_sent: 0,
      hole_bytes: 0,
    }
  }

  /// Advances everything. `dash` requests a burst for player 0 this frame.
  pub fn step(&mut self, dt_ms: u64, local_input: Vec2, dash: bool, controls: &Controls) {
    self.wall_ms += dt_ms;
    if dash {
      self.server.try_dash(0);
    }

    for (player, packet) in self.server.advance(dt_ms, local_input, controls) {
      self.bytes_sent += packet.bytes() as u64;
      self.packets_sent += 1;
      self.corrections_sent += packet.corrections.len() as u64;
      self.hole_bytes += (packet.holes.len() * crate::sim::types::HOLE_BYTES + packet.clusters.len() * crate::sim::types::CLUSTER_BYTES) as u64;
      self.down[player as usize].send(self.wall_ms, packet, controls.latency_ms, controls.jitter_ms, 0.0, &mut self.rng);
    }

    for (p, link) in self.down.iter_mut().enumerate() {
      for packet in link.drain_due(self.wall_ms) {
        self.clients[p].on_packet(&packet, self.wall_ms, controls);
      }
    }
    for client in &mut self.clients {
      client.tick(dt_ms, controls);
    }
  }

  /// How far the pellets a client draws are from where the server has them.
  ///
  /// Only counts pellets near the viewer, because under particle sync a client is
  /// never told about distant ones and comparing those would flatter field sync
  /// unfairly.
  pub fn mean_pellet_error(&self, player: usize) -> f32 {
    let eye = self.server.holes[player].pos;
    let mut sum = 0.0;
    let mut n = 0u32;
    for (id, drawn) in self.clients[player].render() {
      let truth = self.server.pellets[id as usize].pos;
      if truth.dist(eye) <= VIEW_RADIUS {
        sum += drawn.dist(truth);
        n += 1;
      }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
  }

  /// The error distribution is heavy-tailed: near a core, gravity is chaotic
  /// enough that a few pellets diverge wildly while the rest track well. The mean
  /// is dominated by those few, so the median is what describes the typical
  /// pellet, and p90 shows how deep the tail runs.
  pub fn pellet_error_percentiles(&self, player: usize) -> (f32, f32) {
    let eye = self.server.holes[player].pos;
    let mut errs: Vec<f32> = self.clients[player]
      .render()
      .into_iter()
      .filter_map(|(id, drawn)| {
        let truth = self.server.pellets[id as usize].pos;
        (truth.dist(eye) <= VIEW_RADIUS).then(|| drawn.dist(truth))
      })
      .collect();
    if errs.is_empty() {
      return (0.0, 0.0);
    }
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = errs[errs.len() / 2];
    let p90 = errs[(errs.len() * 9 / 10).min(errs.len() - 1)];
    (median, p90)
  }

  pub fn max_pellet_error(&self, player: usize) -> f32 {
    let eye = self.server.holes[player].pos;
    let mut worst = 0.0f32;
    for (id, drawn) in self.clients[player].render() {
      let truth = self.server.pellets[id as usize].pos;
      if truth.dist(eye) <= VIEW_RADIUS {
        worst = worst.max(drawn.dist(truth));
      }
    }
    worst
  }

  pub fn bytes_per_sec(&self) -> f64 {
    if self.wall_ms == 0 {
      return 0.0;
    }
    self.bytes_sent as f64 / (self.wall_ms as f64 / 1000.0)
  }

  /// Pellet states sent per packet: the number field sync is trying to avoid.
  pub fn mean_corrections_per_packet(&self) -> f64 {
    if self.packets_sent == 0 {
      return 0.0;
    }
    self.corrections_sent as f64 / self.packets_sent as f64
  }

  /// How long, on average, before a given pellet is refreshed. Field sync bounds
  /// divergence by covering everything eventually, not by covering it often.
  pub fn refresh_interval_secs(&self, controls: &Controls) -> f64 {
    if controls.mode != SyncMode::Field || controls.corrections_per_packet == 0 {
      return f64::INFINITY;
    }
    let packets_per_sweep = self.server.pellets.len() as f64 / controls.corrections_per_packet as f64;
    packets_per_sweep / controls.sync_hz.max(1) as f64
  }

  pub fn pellet_count(&self) -> usize {
    self.server.pellets.len()
  }

  pub fn known_pellets(&self, player: usize) -> usize {
    self.clients[player].known_pellets()
  }

  pub fn swallow_count(&self) -> u64 {
    self.server.swallow_count
  }

  pub fn collision_count(&self) -> u64 {
    self.server.collision_count
  }

  pub fn player_count(&self) -> usize {
    self.clients.len()
  }

  /// Whether player 0's dash is off cooldown.
  pub fn dash_ready(&self) -> bool {
    self.server.dash_ready(0)
  }

  /// Whether a hole is mid-dash, for the burst effect.
  pub fn is_dashing(&self, player: usize) -> bool {
    self.server.is_dashing(player)
  }

  /// Force evaluations per second **on one machine**: every pellet integrated
  /// against every point source in the field it was given, every step. This is
  /// what the "send the field" bet spends instead of bandwidth, and it grows
  /// linearly with the size of that field, so it is the number that decides
  /// whether the technique is affordable at a given crowd size.
  ///
  /// Read off the client rather than the server's hole count, because those stop
  /// being the same number once the field is aggregated: that is precisely the
  /// saving, and a metric computed from the hole count could not see it.
  pub fn force_evals_per_client_per_sec(&self) -> f64 {
    let field = self.mean_field_size();
    self.server.pellets.len() as f64 * field * crate::sim::types::SIM_HZ as f64
  }

  /// Mean point sources per client field. Equals the live hole count when
  /// aggregation is off.
  pub fn mean_field_size(&self) -> f64 {
    let known: Vec<usize> = self.clients.iter().map(|c| c.field_size()).filter(|n| *n > 0).collect();
    if known.is_empty() {
      return self.server.holes.iter().filter(|h| h.alive).count() as f64;
    }
    known.iter().sum::<usize>() as f64 / known.len() as f64
  }

  /// Total pull a client's field exerts, against what the server's actually
  /// does.
  ///
  /// This is the one number that separates aggregating from culling, and neither
  /// the bandwidth readout nor the error readout can show it. Culling deletes
  /// forces, so the client's total falls below the truth and it integrates a
  /// world that is quietly lighter than the real one. Aggregating keeps every
  /// gram and only blurs where it acts, so the total matches whatever the angle
  /// is set to.
  pub fn field_weight(&self, player: usize) -> (f32, f32) {
    let believed = self.clients[player].field_weight();
    let truth: f32 = self.server.holes.iter().filter(|h| h.alive).map(|h| h.effective_mass()).sum();
    (believed, truth)
  }

  /// Share of traffic that is hole states rather than pellet corrections.
  pub fn hole_bytes_share(&self) -> f64 {
    if self.bytes_sent == 0 {
      return 0.0;
    }
    self.hole_bytes as f64 / self.bytes_sent as f64
  }

  /// Total mass removed by contact.
  pub fn mass_drained(&self) -> f32 {
    self.server.mass_drained
  }

  /// How many players have been drained to nothing.
  pub fn eliminations(&self) -> u64 {
    self.server.eliminations
  }

  /// Pellets eaten per player: the score, which keeps climbing after mass caps.
  pub fn scores(&self) -> &[u32] {
    &self.server.scores
  }

  /// The authoritative holes.
  pub fn holes(&self) -> &[BlackHole] {
    &self.server.holes
  }

  /// The field as one client believes it, for showing what culling it does.
  pub fn client_holes(&self, player: usize) -> &[BlackHole] {
    &self.clients[player].holes
  }

  pub fn truth_pellets(&self) -> &[crate::sim::types::Pellet] {
    &self.server.pellets
  }

  pub fn client_render(&self, player: usize) -> Vec<(PelletId, Vec2)> {
    self.clients[player].render()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn run(controls: &Controls, secs: u64) -> World {
    let mut w = World::new(controls, controls.player_count, 0x81AC_C0DE);
    for i in 0..(secs * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, controls);
    }
    w
  }

  #[test]
  fn sending_the_field_costs_far_less_than_sending_the_particles() {
    let field = Controls { mode: SyncMode::Field, ..Controls::default() };
    let particles = Controls { mode: SyncMode::Particles, ..Controls::default() };

    let wf = run(&field, 5);
    let wp = run(&particles, 5);

    assert!(
      wf.bytes_per_sec() * 3.0 < wp.bytes_per_sec(),
      "field sync should be far cheaper: {:.0} vs {:.0} B/s",
      wf.bytes_per_sec(),
      wp.bytes_per_sec()
    );
  }

  #[test]
  fn a_locally_integrated_field_stays_close_to_the_truth() {
    // The whole bet: with the same field and the same step, a client that
    // integrates locally tracks the server without being sent pellet positions.
    // Judged on the median: the error distribution is heavy-tailed, because a
    // few pellets falling through a core diverge chaotically and dominate any
    // mean. The median describes the pellet you actually look at.
    let c = Controls::default();
    let w = run(&c, 6);
    let (median, _p90) = w.pellet_error_percentiles(0);
    assert!(median < 80.0, "the typical locally integrated pellet should track the truth, median {median:.1}px");
  }

  #[test]
  fn corrections_bound_the_divergence() {
    // Gravity is divergent: without correction the client drifts. With a rotating
    // slice of corrections it should do better.
    let none = Controls { corrections_per_packet: 0, ..Controls::default() };
    let some = Controls { corrections_per_packet: 80, ..Controls::default() };

    let w_none = run(&none, 8);
    let w_some = run(&some, 8);

    // On the median, for the same reason as the test above: a handful of pellets
    // falling through a core diverge chaotically and own any mean, which makes
    // the mean non-monotonic in the correction budget even though the typical
    // pellet improves steadily.
    let (none_med, _) = w_none.pellet_error_percentiles(0);
    let (some_med, _) = w_some.pellet_error_percentiles(0);
    assert!(
      some_med < none_med,
      "corrections should reduce drift for the typical pellet: {some_med:.1}px corrected vs {none_med:.1}px uncorrected"
    );
  }

  #[test]
  fn culling_the_field_breaks_the_local_physics() {
    // Relevance culling is right for rendering and wrong for simulation inputs:
    // a hole you were not told about still bends every pellet you hold.
    let full = Controls { cull_attractors: false, ..Controls::default() };
    let culled = Controls { cull_attractors: true, ..Controls::default() };

    let w_full = run(&full, 6);
    let w_culled = run(&culled, 6);

    assert!(
      w_culled.mean_pellet_error(0) > w_full.mean_pellet_error(0),
      "culling attractors should make the physics wrong: {:.1}px culled vs {:.1}px full",
      w_culled.mean_pellet_error(0),
      w_full.mean_pellet_error(0)
    );
  }

  fn run_at(controls: &Controls, secs: u64) -> World {
    let mut w = World::new(controls, controls.player_count, 0x81AC_C0DE);
    for i in 0..(secs * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, controls);
    }
    w
  }

  #[test]
  fn aggregating_keeps_the_weight_that_culling_throws_away() {
    // The distinction the whole technique rests on, as a number. Both send a
    // shorter field than the truth; only one of them still adds up.
    let base = Controls { player_count: 64, ..Controls::default() };
    let culled = run_at(&Controls { cull_attractors: true, ..base }, 4);
    let aggregated = run_at(&Controls { aggregation_theta: 0.5, ..base }, 4);

    let (culled_weight, truth) = culled.field_weight(0);
    let (agg_weight, agg_truth) = aggregated.field_weight(0);

    assert!(culled_weight < truth * 0.5, "culling deletes most of the field: {culled_weight:.0} of {truth:.0}");
    assert!(
      (agg_weight - agg_truth).abs() < agg_truth * 0.02,
      "aggregating keeps all of it: {agg_weight:.0} of {agg_truth:.0}"
    );
    assert!(
      aggregated.mean_field_size() < 64.0,
      "and still sends fewer sources than there are holes: {:.1}",
      aggregated.mean_field_size()
    );
  }

  #[test]
  fn a_zero_angle_is_the_full_field_exactly() {
    // The off switch has to be the same code path. If it were not, "aggregation
    // disabled" would be a second implementation with its own behaviour, and
    // every comparison against it would be measuring two changes at once.
    let base = Controls { player_count: 32, ..Controls::default() };
    let off = run_at(&base, 3);
    let zero = run_at(&Controls { aggregation_theta: 0.0, ..base }, 3);
    assert_eq!(off.bytes_per_sec() as u64, zero.bytes_per_sec() as u64);
    assert!((off.mean_field_size() - zero.mean_field_size()).abs() < 0.01);
  }

  #[test]
  fn aggregation_buys_far_more_compute_than_it_costs_accuracy() {
    // The trade that justifies it at 64 holes: per-machine force evaluations fall
    // by a third while the typical pellet barely moves, because a distant crowd
    // really is well described by one body at its centre of mass.
    //
    // Note which resource this saves. Bandwidth is dominated by pellet
    // corrections, not by the field, so coarsening the field can only ever reach
    // the third of the traffic that the field occupies. Compute is entirely
    // field-bound, and that is where the saving lands.
    let base = Controls { player_count: 64, ..Controls::default() };
    let full = run_at(&base, 5);
    let agg = run_at(&Controls { aggregation_theta: 0.3, ..base }, 5);

    let (full_med, _) = full.pellet_error_percentiles(0);
    let (agg_med, _) = agg.pellet_error_percentiles(0);

    assert!(
      agg.force_evals_per_client_per_sec() < full.force_evals_per_client_per_sec() * 0.75,
      "a third less work: {:.1}M against {:.1}M",
      agg.force_evals_per_client_per_sec() / 1e6,
      full.force_evals_per_client_per_sec() / 1e6
    );
    assert!(agg_med < full_med * 1.25, "for very little accuracy: {agg_med:.0}px against {full_med:.0}px");
  }

  #[test]
  fn a_wide_angle_is_worse_than_culling_and_that_is_the_limit() {
    // Measured, and it refutes the obvious expectation that keeping every gram of
    // the field must beat deleting most of it. Past an opening angle of about 1.0
    // the criterion lets a viewer sit close to a cell it has already accepted, so
    // a whole quadrant's mass collapses onto a single point near the pellets being
    // integrated, and a spurious concentration is worse than a missing force.
    //
    // The technique has a safe range rather than a monotone dial, which is the
    // part that would not have been obvious without measuring it.
    let base = Controls { player_count: 64, ..Controls::default() };
    let culled = run_at(&Controls { cull_attractors: true, ..base }, 5);
    let wide = run_at(&Controls { aggregation_theta: 1.2, ..base }, 5);
    let safe = run_at(&Controls { aggregation_theta: 0.5, ..base }, 5);

    let (culled_med, _) = culled.pellet_error_percentiles(0);
    let (wide_med, _) = wide.pellet_error_percentiles(0);
    let (safe_med, _) = safe.pellet_error_percentiles(0);

    assert!(wide_med > culled_med, "a wide angle really is worse than culling: {wide_med:.0}px against {culled_med:.0}px");
    assert!(safe_med < culled_med, "and a safe one really is better: {safe_med:.0}px against {culled_med:.0}px");
  }

  #[test]
  fn covering_every_pellet_beats_targeting_the_worst() {
    // Counter-intuitive and measured: spending the budget on the pellets deepest
    // in a well is much worse than sweeping all of them in rotation, because the
    // deep ones are about to be swallowed (and a respawn resyncs them anyway)
    // while everything else is left to drift without a bound.
    let sweep = Controls { priority_corrections: false, ..Controls::default() };
    let target = Controls { priority_corrections: true, ..Controls::default() };

    let w_sweep = run(&sweep, 8);
    let w_target = run(&target, 8);

    let (sweep_med, _) = w_sweep.pellet_error_percentiles(0);
    let (target_med, _) = w_target.pellet_error_percentiles(0);
    assert!(
      sweep_med < target_med,
      "round-robin coverage should beat priority targeting: {sweep_med:.1}px vs {target_med:.1}px"
    );
  }

  #[test]
  fn contact_drains_both_players_and_can_eliminate() {
    // Holes attract each other, so with four of them in one arena contact is
    // inevitable, and contact drains. Somebody should lose mass for it.
    let c = Controls::default();
    let mut w = World::new(&c, c.player_count, 0x81AC_C0DE);
    // Drive player 0 toward the middle, where the others are drifting.
    for _ in 0..(20 * 60) {
      w.step(16, Vec2::new(0.2, 0.2), false, &c);
    }
    // Measure the drain directly. Comparing masses against their starting value
    // would be wrong, because pellets are inflating them at the same time, so a
    // hole can be draining and still growing.
    assert!(w.collision_count() > 0, "the holes attract each other, so they should have met");
    assert!(w.mass_drained() > 0.0, "contact should cost mass, drained {:.1}", w.mass_drained());
    for h in w.holes() {
      assert!(h.mass >= 0.0 && h.mass.is_finite());
    }
  }

  #[test]
  fn pressed_holes_never_interpenetrate() {
    // They squeeze, they do not pass through: at every moment any two live holes
    // are at least tangent.
    let c = Controls::default();
    let mut w = World::new(&c, c.player_count, 0x81AC_C0DE);
    for i in 0..(20 * 60) {
      let t = i as f32 * 0.03;
      w.step(16, Vec2::new(t.cos(), t.sin()), false, &c);
      let holes = w.holes();
      for a in 0..holes.len() {
        for b in (a + 1)..holes.len() {
          if holes[a].alive && holes[b].alive {
            let gap = holes[a].pos.dist(holes[b].pos) - (holes[a].radius() + holes[b].radius());
            assert!(gap > -0.5, "holes overlapped by {:.2}px", -gap);
          }
        }
      }
    }
  }

  #[test]
  fn a_dash_outruns_the_pull_that_a_walk_cannot() {
    // The escape mechanic, as a number: the attraction at contact is tuned above
    // walking speed and below a dash, which is what makes a grapple sticky and a
    // dash the way out.
    use crate::sim::types::{BlackHole, DASH_SPEED_MULT, HOLE_PULL_SCALE, HOLE_SPEED, MAX_HOLE_PULL, START_MASS};
    let hole = BlackHole { pos: Vec2::new(0.0, 0.0), mass: START_MASS, alive: true };
    let contact = hole.radius() * 2.0;
    let pull = (HOLE_PULL_SCALE * hole.effective_mass() / (contact * contact)).min(MAX_HOLE_PULL);
    assert!(pull > HOLE_SPEED, "a walk cannot escape contact: pull {pull:.0} vs walk {HOLE_SPEED:.0}");
    assert!(pull < HOLE_SPEED * DASH_SPEED_MULT, "a dash can: pull {pull:.0} vs dash {:.0}", HOLE_SPEED * DASH_SPEED_MULT);
  }

  #[test]
  fn the_game_actually_runs() {
    let w = run(&Controls::default(), 6);
    assert!(w.swallow_count() > 0, "pellets are being swallowed");
    for h in w.holes() {
      // Mass has no ceiling now; what has to stay bounded is its *effect*, which
      // is the log-damped value the field actually uses. It can reach zero, which
      // is elimination, so the floor is zero rather than a minimum mass.
      assert!(h.mass.is_finite() && h.mass >= 0.0, "mass stayed finite and non-negative: {}", h.mass);
      assert!(h.effective_mass().is_finite(), "the damped mass stayed finite");
      assert!(
        h.effective_mass() < h.mass.max(crate::sim::types::START_MASS) * 1.5,
        "growth is damped rather than linear: effective {} against raw {}",
        h.effective_mass(),
        h.mass
      );
    }
    for p in w.truth_pellets() {
      assert!(p.pos.x.is_finite() && p.pos.y.is_finite(), "the integrator stayed stable");
    }
  }
}
