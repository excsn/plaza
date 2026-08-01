//! The shared vocabulary: the gravity field, the pellets it moves, the black
//! holes that define it, and the one integration step both sides run.
//!
//! The distinction this example is built around: a pellet's motion is decided
//! *entirely* by the black holes. There are thousands of pellets and a handful of
//! holes, so the cheap thing to send is the **field** (a few holes), not its
//! output (thousands of positions).

use serde::{Deserialize, Serialize};

pub const ARENA_W: f32 = 3000.0;
pub const ARENA_H: f32 = 3000.0;

/// Gravitational constant, tuned for visible orbits rather than realism.
pub const G: f32 = 90_000.0;
/// Softening term, so a pellet passing very close is not flung to infinity by a
/// singular force. Deliberately *small*: soften too much and the well goes flat
/// near the centre, which kills the whole feel. The pull should keep intensifying
/// the closer a pellet gets, so it drifts at the rim and whips in near the core.
pub const SOFTENING: f32 = 120.0;

pub const SIM_HZ: u32 = 60;
pub const SIM_DT: f32 = 1.0 / SIM_HZ as f32;

/// A black hole's mass floor, and what it gains per pellet.
///
/// There is deliberately **no ceiling**. Mass *is* the field strength, so letting
/// it grow linearly forever would steepen the well until one player dominated the
/// arena, but a hard cap solves that with a wall: growth feels normal and then
/// simply stops. Diminishing returns are the better shape, so mass keeps
/// accumulating and its *effect* is log-damped through
/// [`BlackHole::effective_mass`]. Nothing has an edge to hit.
pub const START_MASS: f32 = 120.0;
pub const PELLET_MASS: f32 = 3.0;
/// The scale over which growth stops paying off. Below it, mass behaves almost
/// linearly; well above it, doubling your mass barely moves your pull.
pub const MASS_SCALE: f32 = 400.0;
/// Contact is continuous, not a tap, and holes do **not** interpenetrate: they
/// press against each other like two marshmallows being squeezed, staying exactly
/// tangent while both drain.
///
/// The drain has a floor for merely touching, plus a term for how hard you are
/// pressing. Pressure is measured as the overlap that *would* have happened if
/// they could pass through each other, which is what makes dashing into someone
/// bite harder than drifting into them.
pub const CONTACT_DRAIN_BASE: f32 = 55.0;
pub const CONTACT_DRAIN_PRESS: f32 = 210.0;
// There is no separate merge reward. Merging *is* the other hole reaching zero:
// you squeeze until there is nothing left of them, and what remains is one hole.
// A bonus on top would pay twice for the same event.
/// Black holes pull *each other*, not just the pellets. This is what makes
/// contact sticky: once you are close the mutual attraction keeps dragging you
/// together, so a fight does not simply end when you stop steering into it.
///
/// Scaled so the pull at contact beats a walk (`HOLE_SPEED`) and loses to a dash,
/// which is the whole tension: you cannot stroll out of a grapple, and one dash
/// buys distance the pull immediately starts eating back. Getting clear usually
/// takes a few.
pub const HOLE_PULL_SCALE: f32 = 9_000.0;
/// Ceiling on that pull, so a close pass cannot fling anyone across the arena.
pub const MAX_HOLE_PULL: f32 = 430.0;


/// Dash: a short burst of speed, on a cooldown. What lets you close the gap and
/// force contact, or break away from someone draining you.
pub const DASH_SPEED_MULT: f32 = 2.9;
pub const DASH_DURATION_MS: u64 = 220;
pub const DASH_COOLDOWN_MS: u64 = 1400;

/// Drained to here and you are out.
pub const ELIMINATION_MASS: f32 = 1.0;
/// Eliminated players return after this, so the sandbox keeps running rather
/// than emptying out.
pub const RESPAWN_DELAY_MS: u64 = 2500;
/// Contact cannot drain you every frame.
pub const COLLISION_COOLDOWN_MS: u64 = 600;

pub const HOLE_SPEED: f32 = 210.0;
/// How far a player can see, for *rendering* culling only. Gravity does not care.
pub const VIEW_RADIUS: f32 = 620.0;

pub type PelletId = u32;
pub type PlayerId = u8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
  pub x: f32,
  pub y: f32,
}

impl Vec2 {
  pub fn new(x: f32, y: f32) -> Self {
    Self { x, y }
  }
  pub fn dist(self, o: Vec2) -> f32 {
    let (dx, dy) = (self.x - o.x, self.y - o.y);
    (dx * dx + dy * dy).sqrt()
  }
}

/// One black hole: a player. Position and mass together *are* the field.
///
/// `alive` is part of the field too, not just presentation: an eliminated hole
/// stops pulling, so a client that did not know would integrate a force that is
/// no longer there.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BlackHole {
  pub pos: Vec2,
  pub mass: f32,
  pub alive: bool,
}

impl BlackHole {
  /// What the field actually responds to: mass with diminishing returns.
  ///
  /// `scale * ln(1 + mass / scale)` is near-linear while you are small, so early
  /// growth feels like it should, and flattens as you get large, so a runaway
  /// leader's pull keeps rising without ever running away. This replaces a hard
  /// mass cap: same protection against a degenerate field, no wall to hit.
  ///
  /// Both the server and every client call this, because it is part of the shared
  /// rule; a client using raw mass would integrate a different world.
  pub fn effective_mass(&self) -> f32 {
    MASS_SCALE * (1.0 + self.mass / MASS_SCALE).ln()
  }

  /// The visible disk, and the body other players collide with. A pellet
  /// crossing this is caught but not yet gone: it still has the whole well to
  /// fall through, accelerating the whole way.
  ///
  /// Radius follows the square root of the *effective* mass, so area tracks the
  /// pull rather than the raw total: the curve you see is the curve you feel.
  pub fn radius(&self) -> f32 {
    6.0 + self.effective_mass().sqrt() * 2.4
  }

  /// The core: a pellet reaching this is swallowed.
  ///
  /// Much smaller than the disk on purpose. If a pellet were consumed at the rim
  /// it would never experience the steep part of the well, and the pull would
  /// look uniform instead of accelerating inward.
  pub fn core_radius(&self) -> f32 {
    self.radius() * 0.22
  }

  /// This hole as a point source, for the integrator and for the aggregation
  /// tree that coarsens it.
  pub fn as_attractor(&self) -> Attractor {
    Attractor {
      pos: self.pos,
      pull: self.effective_mass(),
    }
  }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Pellet {
  pub pos: Vec2,
  pub vel: Vec2,
}

/// A point source of pull: what the integrator actually reads.
///
/// A live hole contributes one of these. So does a *cluster* of distant holes,
/// standing in for all of them at their centre of mass, which is the entire point
/// of the separation: the integrator cannot tell the difference, so aggregating
/// the far field needs no second physics path.
///
/// `pull` is the already-damped [`BlackHole::effective_mass`], not raw mass,
/// because that is the quantity gravity superposes. Summing raw masses and
/// damping afterwards would give a different, wrong field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attractor {
  pub pos: Vec2,
  pub pull: f32,
}

/// **The shared integration step.** Server and client run exactly this, which is
/// what makes it possible to send the field instead of the particles.
///
/// Semi-implicit Euler at a fixed step: cheap, stable enough with softening, and
/// identical on both sides as long as the step and the attractor list match. Note
/// the second condition. It is why a client cannot simply *drop* a distant hole,
/// and why it can safely be handed a coarsened one instead.
pub fn step_pellet(pellet: &mut Pellet, field: &[Attractor], dt: f32) {
  let (mut ax, mut ay) = (0.0f32, 0.0f32);
  for source in field {
    let dx = source.pos.x - pellet.pos.x;
    let dy = source.pos.y - pellet.pos.y;
    let r2 = dx * dx + dy * dy + SOFTENING;
    let inv_r = 1.0 / r2.sqrt();
    let a = G * source.pull / r2;
    ax += a * dx * inv_r;
    ay += a * dy * inv_r;
  }
  pellet.vel.x += ax * dt;
  pellet.vel.y += ay * dt;
  pellet.pos.x += pellet.vel.x * dt;
  pellet.pos.y += pellet.vel.y * dt;
}

/// The exact field of a set of holes: one attractor per live hole.
pub fn exact_field(holes: &[BlackHole]) -> Vec<Attractor> {
  holes.iter().filter(|h| h.alive).map(|h| h.as_attractor()).collect()
}

/// A pellet entering play, or re-entering after being swallowed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PelletSpawn {
  pub id: PelletId,
  pub pos: Vec2,
  pub vel: Vec2,
}

/// An authoritative correction for one pellet. Under field sync these are sent
/// for a small rotating subset, because divergence is bounded by *refreshing
/// everything eventually*, not by refreshing everything often.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PelletCorrection {
  pub id: PelletId,
  pub pos: Vec2,
  pub vel: Vec2,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Packet {
  pub server_time_ms: u64,
  /// The holes sent exactly. Everything, unless aggregation is on, in which case
  /// this is what the recipient is close enough to resolve individually.
  pub holes: Vec<(PlayerId, BlackHole)>,
  /// Stand-ins for the holes *not* sent exactly: each carries a distant group's
  /// combined pull at its centre of mass. Empty when aggregation is off.
  ///
  /// This is the difference between aggregating and culling. Culling drops the
  /// distant contribution and the client integrates a field that is missing
  /// forces; this keeps every gram of it and only blurs where it comes from.
  pub clusters: Vec<Attractor>,
  /// Pellets swallowed since the last packet, an authoritative outcome.
  pub swallowed: Vec<PelletId>,
  pub spawned: Vec<PelletSpawn>,
  /// Corrections, or under particle sync the full stream of visible pellets.
  pub corrections: Vec<PelletCorrection>,
  /// The players mid-dash right now, so a client can show the burst rather than
  /// only feel it as a hole that lurched. Authoritative and view-independent: a
  /// dash is a fact about a player, not about who is looking.
  pub dashing: Vec<PlayerId>,
}

// Byte accounting, consistent so the comparison between modes is meaningful.
pub const ID_BYTES: usize = 3;
pub const POS_BYTES: usize = 4;
pub const VEL_BYTES: usize = 4;
/// A hole is a position plus a mass, and there are only a handful.
pub const HOLE_BYTES: usize = 1 + POS_BYTES + 2;
/// A cluster is the same minus the player id, which it does not have: it is not
/// anybody, it is the weight of several somebodies.
pub const CLUSTER_BYTES: usize = POS_BYTES + 2;

impl Packet {
  pub fn bytes(&self) -> usize {
    self.holes.len() * HOLE_BYTES
      + self.clusters.len() * CLUSTER_BYTES
      + self.swallowed.len() * ID_BYTES
      + self.spawned.len() * (ID_BYTES + POS_BYTES + VEL_BYTES)
      + self.corrections.len() * (ID_BYTES + POS_BYTES + VEL_BYTES)
      + self.dashing.len()
  }
}

/// What the server actually sends, and the comparison the example exists for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
  /// Send the field (a few holes) and let every client integrate the pellets
  /// itself. Corrections go to a small rotating subset.
  Field,
  /// Send pellet positions, the way a conventional replicated-entity game would.
  /// Culled by render relevance, because otherwise it is hopeless.
  Particles,
}

#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  /// Percentage of packets the impairment link drops.
  ///
  /// Worth having even though this example has no delta stream to recover: a
  /// dropped frame here means a whole send interval's worth of corrections never
  /// lands, and the local field integration carries on regardless, so it shows
  /// what running on stale forces costs.
  pub loss_pct: f32,
  /// What a lost packet costs, which is a property of the link rather than of
  /// this simulation. The transport underneath is a WebSocket, so the truthful
  /// answer is a retransmission: the frame is late and nothing is missing. The
  /// netcode above is written for the other answer, where the packet is gone,
  /// which is the one worth demonstrating here.
  pub datagram_link: bool,
  pub sync_hz: u32,
  pub mode: SyncMode,
  /// Under field sync: how many pellets are corrected per packet. Zero means the
  /// client is never corrected at all, which is the pure-divergence case.
  pub corrections_per_packet: usize,
  /// Spend the correction budget on the pellets deepest in a well instead of
  /// sweeping every pellet in rotation.
  ///
  /// Measured to be **worse**, badly, at every budget. Kept as a toggle because
  /// the reason is worth seeing: a pellet deep in a well is about to be swallowed
  /// and respawned, and a respawn already resyncs it, while targeting starves
  /// every other pellet into unbounded drift. Coverage beats targeting when the
  /// failure mode is drift without a bound.
  pub priority_corrections: bool,
  /// Cull the *field* by render relevance: the deliberate mistake. A client that
  /// only knows nearby holes integrates the wrong physics.
  pub cull_attractors: bool,
  /// Barnes-Hut opening angle: the third option between sending the whole field
  /// and culling it.
  ///
  /// A group of holes standing `d` away with a spread of `s` is replaced by one
  /// attractor at their centre of mass when `s / d < theta`. Zero disables it and
  /// sends every hole exactly, which is the honest off switch because it is the
  /// same code path rather than a different one.
  ///
  /// Turn it up with 64 holes on the field: what it buys is not the same trade
  /// the cull toggle offers. Culling saves bandwidth by *deleting* forces, and
  /// the client's physics goes wrong in proportion. This saves bandwidth by
  /// blurring where a distant force comes from, and gravity barely notices,
  /// because the further away a crowd is the better one point mass describes it.
  pub aggregation_theta: f32,
  pub pellet_count: usize,
  /// How many black holes share the arena. Worth turning up: the field is only
  /// cheap to send while it is small, and every pellet integrates against every
  /// hole, so both the wire cost and the compute cost scale with this.
  pub player_count: usize,
  pub smooth: bool,
  /// Predict the dash burst locally instead of letting it arrive as a correction.
  /// On, the local hole moves at dash speed the instant the press is granted, so
  /// the burst is smooth; off, the dash is unpredicted and the hole snaps forward
  /// a round trip later. A client-side choice, so it rides in `Controls` next to
  /// `smooth` rather than in the server policy.
  pub predict_dash: bool,
  /// Draw where the last frame put your hole, faintly, under where you predict
  /// it is.
  ///
  /// **This means something different from horde's ghost, because the two
  /// clients have different architectures**, and flattening the two would be
  /// worse than the extra sentence. Horde buffers packets and plays them out on a
  /// render clock, so its ghost is the *future* it already holds and the gap is
  /// the playout delay. This client applies a packet on arrival and predicts
  /// forward from it, so its ghost is the newest authoritative sample and the gap
  /// is prediction error plus the one-way delay the sample is old. That is the
  /// classic server ghost, the same quantity `netcode_playground` draws.
  ///
  /// **On by default**, and this example is the one that needs it most. The hole
  /// is a *forced* entity, pulled by every other hole and pushed out of every
  /// overlap, so its prediction is the hardest thing here and three separate bugs
  /// in it wore one symptom. Each was found by reading a correction log, which is
  /// a slower way of seeing what a ring next to the marker shows directly.
  ///
  /// **Every role has one.** A host's ring is the server's state *now*, because
  /// it is the server, so its gap is prediction error alone. A joiner's is the
  /// newest sample it received, which is not a privilege, and its gap is that
  /// error plus how stale the sample is.
  pub show_ghost: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 80,
      jitter_ms: 15,
      loss_pct: 0.0,
      datagram_link: true,
      sync_hz: 16,
      mode: SyncMode::Field,
      corrections_per_packet: 40,
      priority_corrections: false,
      cull_attractors: false,
      aggregation_theta: 0.0,
      pellet_count: 2000,
      player_count: 8,
      smooth: true,
      predict_dash: true,
      show_ghost: true,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }
}
