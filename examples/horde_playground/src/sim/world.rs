//! Ties the server, the clients, and the wire together, and measures the things
//! this example exists to settle: what relevance saves, what a low sync rate
//! costs, and which remote-drawing strategy is actually closest to the truth.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::server::Server;
use crate::sim::types::ACK_BYTES;
use crate::sim::types::{ClientMsg, Controls, Downstream, EnemyKind, Handle, PlayerId, Vec2};

pub struct World {
  server: Server,
  clients: Vec<Client>,
  /// One downstream link per player.
  down: Vec<LatencyLink<Downstream>>,
  /// Acknowledgements travelling back. They take the same latency and the same
  /// loss as everything else, which matters: a baseline is always at least a
  /// round trip out of date, so the server re-derives slightly more than it
  /// strictly has to.
  up: Vec<LatencyLink<ClientMsg>>,
  rng: Rng,
  wall_ms: u64,

  // Rolling measurements.
  bytes_sent: u64,
  crowd_bytes: u64,
  naive_bytes_sent: u64,
  packets_sent: u64,
  relevant_total: u64,
  relevant_samples: u64,
  spawns_total: u64,
  despawns_total: u64,
  /// The actual despawn index sets, kept so an encoding can be measured against
  /// real data rather than assumed. Bounded so a long run cannot grow forever.
  despawn_sets: Vec<Vec<u32>>,
  bytes_by_part: [u64; 5],
}

impl World {
  pub fn new(controls: &Controls, player_count: usize, seed: u64) -> Self {
    Self {
      server: Server::new(controls.enemy_count, player_count, controls.spread_players),
      clients: (0..player_count).map(|p| Client::new(p as PlayerId, player_count)).collect(),
      down: (0..player_count).map(|_| LatencyLink::new()).collect(),
      up: (0..player_count).map(|_| LatencyLink::new()).collect(),
      rng: Rng::new(seed),
      wall_ms: 0,
      bytes_sent: 0,
      crowd_bytes: 0,
      naive_bytes_sent: 0,
      packets_sent: 0,
      relevant_total: 0,
      relevant_samples: 0,
      spawns_total: 0,
      despawns_total: 0,
      despawn_sets: Vec::new(),
      bytes_by_part: [0; 5],
    }
  }

  /// Advances everything. `local_input` is the direction player 0 is steering.
  pub fn step(&mut self, dt_ms: u64, local_input: Vec2, controls: &Controls) {
    self.wall_ms += dt_ms;

    for (player, packet) in self.server.advance(dt_ms, local_input, controls) {
      self.bytes_sent += packet.bytes() as u64;
      for (slot, part) in self.bytes_by_part.iter_mut().zip(packet.bytes_breakdown()) {
        *slot += part as u64;
      }
      self.crowd_bytes += (packet.crowds.len() * crate::sim::types::CROWD_BYTES) as u64;
      self.naive_bytes_sent += packet.naive_bytes() as u64;
      self.packets_sent += 1;
      self.relevant_total += (packet.samples.len() + packet.entered.len()) as u64;
      self.relevant_samples += 1;
      self.spawns_total += packet.entered.len() as u64;
      self.despawns_total += packet.left.len() as u64;
      if !packet.left.is_empty() && self.despawn_sets.len() < 4096 {
        let mut ids: Vec<u32> = packet.left.iter().map(|(h, _)| h.idx).collect();
        ids.sort_unstable();
        ids.dedup();
        self.despawn_sets.push(ids);
      }

      let link = &mut self.down[player as usize];
      link.send(self.wall_ms, Downstream::Frame(Box::new(packet)), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
    }

    // The player stream, on the same impaired link so it is delayed and dropped
    // exactly as the entity stream is. Everyone gets the same frame.
    if let Some(frame) = self.server.take_player_frame() {
      self.bytes_sent += (frame.bytes() * self.down.len()) as u64;
      for link in self.down.iter_mut() {
        link.send(self.wall_ms, Downstream::Players(frame.clone()), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }

    for (p, link) in self.down.iter_mut().enumerate() {
      for message in link.drain_due(self.wall_ms) {
        match message {
          // Taken, not applied. It is played out when the render clock reaches
          // the instant it describes.
          Downstream::Frame(packet) => self.clients[p].receive_packet(*packet, self.wall_ms),
          Downstream::Players(frame) => self.clients[p].on_player_frame(&frame, self.wall_ms),
        }
      }
      // Play out whatever is due, then acknowledge. Acknowledging on *receipt*
      // would send a digest of a state this client has not reached yet, and the
      // server compares that digest against what it believes the client holds.
      self.clients[p].set_render_delay(controls.render_delay_ms);
      let got = self.clients[p].tick(dt_ms, controls);
      if got && let Some((newest, mask)) = self.clients[p].acks().encode() {
        self.bytes_sent += ACK_BYTES as u64;
        self.up[p].send(self.wall_ms, ClientMsg::Ack { newest, mask, digest: self.clients[p].last_digest() }, controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
      // A purchase is a *request*, and it travels the same lossy wire as anything
      // else, so the client's belief about its own wallet is unconfirmed for a
      // round trip whether or not it chose to predict.
      if controls.coins && controls.auto_buy && let Some(upgrade) = self.clients[p].wants_to_buy() {
        self.bytes_sent += 2;
        self.up[p].send(self.wall_ms, ClientMsg::Buy(upgrade), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }
    for (p, link) in self.up.iter_mut().enumerate() {
      for msg in link.drain_due(self.wall_ms) {
        match msg {
          ClientMsg::Ack { newest, mask, digest } => self.server.receive_ack(p, newest, mask, digest),
          ClientMsg::Buy(upgrade) => self.server.receive_buy(p, upgrade),
        }
      }
    }
  }

  /// Mean distance between where clients *draw* enemies and where the server
  /// actually has them. The headline accuracy number.
  pub fn mean_render_error(&self, controls: &Controls) -> f32 {
    let truth: std::collections::BTreeMap<Handle, Vec2> = self.truth().into_iter().collect();
    let mut sum = 0.0;
    let mut n = 0u32;
    for client in &self.clients {
      let Some(at) = client.render_at() else { continue };
      for (handle, drawn, _kind) in client.render(controls, at) {
        // Compare only against the occupant the client believes it holds.
        if let Some(t) = truth.get(&handle) {
          sum += drawn.dist(*t);
          n += 1;
        }
      }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
  }

  /// The worst single error, which is what a player actually notices.
  pub fn max_render_error(&self, controls: &Controls) -> f32 {
    let truth: std::collections::BTreeMap<Handle, Vec2> = self.truth().into_iter().collect();
    let mut worst = 0.0f32;
    for client in &self.clients {
      let Some(at) = client.render_at() else { continue };
      for (handle, drawn, _kind) in client.render(controls, at) {
        if let Some(t) = truth.get(&handle) {
          worst = worst.max(drawn.dist(*t));
        }
      }
    }
    worst
  }

  /// Bytes per second across all players, with compact ids and quantized
  /// positions.
  pub fn bytes_per_sec(&self) -> f64 {
    if self.wall_ms == 0 {
      return 0.0;
    }
    self.bytes_sent as f64 / (self.wall_ms as f64 / 1000.0)
  }

  /// The same traffic with UUIDs and raw `f32` positions.
  pub fn naive_bytes_per_sec(&self) -> f64 {
    if self.wall_ms == 0 {
      return 0.0;
    }
    self.naive_bytes_sent as f64 / (self.wall_ms as f64 / 1000.0)
  }

  /// Average entities included per packet (the relevance result).
  pub fn mean_relevant(&self) -> f64 {
    if self.relevant_samples == 0 {
      return 0.0;
    }
    self.relevant_total as f64 / self.relevant_samples as f64
  }

  pub fn mean_spawns_per_packet(&self) -> f64 {
    if self.packets_sent == 0 {
      return 0.0;
    }
    self.spawns_total as f64 / self.packets_sent as f64
  }

  pub fn mean_despawns_per_packet(&self) -> f64 {
    if self.packets_sent == 0 {
      return 0.0;
    }
    self.despawns_total as f64 / self.packets_sent as f64
  }

  /// Zeroes the rolling measurements, so a readout re-baselines after a control
  /// changes instead of averaging across two different configurations.
  pub fn reset_stats(&mut self) {
    self.bytes_sent = 0;
    self.crowd_bytes = 0;
    self.naive_bytes_sent = 0;
    self.packets_sent = 0;
    self.relevant_total = 0;
    self.relevant_samples = 0;
    self.spawns_total = 0;
    self.despawns_total = 0;
    self.wall_ms = 0;
  }

  pub fn enemy_count(&self) -> usize {
    self.server.alive_count()
  }

  pub fn player_count(&self) -> usize {
    self.clients.len()
  }

  pub fn known_entities(&self, player: usize) -> usize {
    self.clients[player].known_entities()
  }

  /// The server's true live positions, for the ground-truth overlay.
  pub fn truth(&self) -> Vec<(Handle, Vec2)> {
    self.server.live_enemies().map(|(h, e)| (h, e.pos)).collect()
  }

  /// Shots this client currently knows about, for drawing.
  /// Where the offline playground draws a client's shots: at the render target,
  /// the same instant the peers are drawn at.
  pub fn client_projectiles(&self, player: usize) -> Vec<Vec2> {
    self.clients[player]
      .render_at()
      .map(|at| self.clients[player].render_projectiles(at))
      .unwrap_or_default()
  }

  pub fn alive_enemies(&self) -> usize {
    self.server.alive_count()
  }

  pub fn kills(&self) -> u64 {
    self.server.kills
  }

  /// How long ago the last area pulse fired, in seconds, while it is still worth
  /// drawing. `None` once it has faded or if none has fired.
  pub fn nova_flash_age(&self) -> Option<f32> {
    let fired = self.server.last_nova_ms?;
    let age = self.server.now_ms().saturating_sub(fired) as f32 / 1000.0;
    (age <= 0.45).then_some(age)
  }

  /// Enemies killed by the most recent area pulse: the despawn burst.
  pub fn last_nova_kills(&self) -> usize {
    self.server.nova_kills_last
  }

  /// Packet references that named an entity this client no longer holds. With
  /// generational handles these are rejected; without them they would have been
  /// applied to whatever now occupies the slot.
  pub fn stale_refs(&self) -> u64 {
    self.clients.iter().map(|c| c.stale_refs()).sum()
  }

  /// What this client *believes* its balance is, against what the server says.
  ///
  /// Two numbers rather than one, deliberately. Collapsing them into a single
  /// field would make the disagreement unobservable, and an unobservable
  /// disagreement is exactly how a currency bug survives: it only surfaces at a
  /// purchase, long after the divergence that caused it.
  pub fn balance(&self, player: usize) -> (u32, u32) {
    (self.clients[player].believed_balance, self.server.wallets[player].balance)
  }

  /// Pickups this client predicted and the server awarded to somebody else.
  ///
  /// The price of predicting a discrete event. A mispredicted *position* is eased
  /// away over a few frames by `ErrorSmoother`; there is no smooth way to
  /// un-collect a coin, so this number is a count of visible snaps.
  pub fn denied_claims(&self) -> u64 {
    self.clients.iter().map(|c| c.denied_claims).sum()
  }

  /// Purchase requests the server refused, which happens when a client's believed
  /// balance ran ahead of the truth.
  pub fn denied_purchases(&self) -> u64 {
    self.server.denied_purchases
  }

  /// Coins that expired uncollected, against those claimed. A drop rule that
  /// outruns the pickup rule shows up here and nowhere else.
  pub fn coins_expired(&self) -> u64 {
    self.server.coins_expired
  }

  pub fn coins_on_field(&self) -> usize {
    self.server.coins.len()
  }

  pub fn coins_claimed(&self, player: usize) -> u32 {
    self.server.coins_claimed[player]
  }

  /// Coins this client can see, for drawing.
  pub fn client_coins(&self, player: usize) -> &[crate::sim::types::Coin] {
    &self.clients[player].coins
  }

  pub fn wallet(&self, player: usize) -> &crate::sim::types::Wallet {
    &self.server.wallets[player]
  }

  /// Players whose *believed* upgrades differ from the truth: the window in which
  /// a client is simulating enemies under the wrong rule.
  ///
  /// This is what makes coins a netcode question rather than a gameplay one. A
  /// wrong balance is a wrong number; a wrong *upgrade* is a wrong input to the
  /// behaviour rule every client runs locally, so a mispredicted purchase
  /// silently corrupts the simulation until the sample stream grinds it back.
  /// Recent announcements for this client, with the age of each in seconds.
  pub fn notices(&self, player: usize) -> &[(String, f32)] {
    &self.clients[player].notices
  }

  /// Coins in flight to their winners, as this client draws them.
  pub fn coin_flights(&self, player: usize) -> Vec<Vec2> {
    self.clients[player].flight_positions()
  }

  /// The repulsor pulse a given player is emitting right now, as *this client*
  /// believes it, or `None` between pulses.
  ///
  /// Read from the client rather than the server on purpose. The pulse is derived
  /// from a shared rule against an estimated clock, so drawing the server's copy
  /// would hide exactly the disagreement worth seeing: a client whose clock or
  /// whose upgrade belief is wrong fires at a different moment than the world it
  /// is being corrected against.
  pub fn repulsor_pulse_for(&self, viewer: usize, player: usize) -> Option<f32> {
    self.clients[viewer].repel_radius(player)
  }

  /// Packets, summed over clients, received while a client believed in an
  /// upgrade the server did not agree it had: the total time spent simulating
  /// enemies under the wrong rule.
  pub fn wrong_rule_packets(&self) -> u64 {
    self.clients.iter().map(|c| c.wrong_rule_packets).sum()
  }

  pub fn upgrade_disagreements(&self) -> usize {
    (0..self.clients.len())
      .filter(|p| self.clients[*p].believed_upgrades != self.server.wallets[*p].upgrades)
      .count()
  }

  /// The crowd stand-ins this client holds, for drawing the world beyond its
  /// view radius from its own knowledge.
  pub fn crowds(&self, player: usize) -> &[crate::sim::types::Crowd] {
    &self.clients[player].crowds
  }

  /// What share of the enemies outside this client's view radius it has any
  /// awareness of at all, through crowd summaries.
  ///
  /// Relevance culling alone makes this exactly zero: beyond the radius the
  /// client knows nothing, so any whole-arena view it draws is either blank or
  /// borrowed from the server. The number worth watching against it is what that
  /// awareness costs, which is a handful of bytes rather than a share of the
  /// population.
  pub fn crowd_awareness(&self, player: usize) -> f32 {
    let eye = self.server.players[player];
    let distant = self.server.live_enemies().filter(|(_, e)| e.pos.dist(eye) > crate::sim::types::VIEW_RADIUS).count();
    if distant == 0 {
      return 1.0;
    }
    let summarised: u32 = self.clients[player].crowds.iter().map(|c| c.count as u32).sum();
    (summarised as f32 / distant as f32).min(1.0)
  }

  /// Bytes per second spent on crowd summaries.
  pub fn crowd_bytes_per_sec(&self) -> f64 {
    if self.wall_ms == 0 {
      return 0.0;
    }
    self.crowd_bytes as f64 / (self.wall_ms as f64 / 1000.0)
  }

  /// Entities the server considers relevant to this client that the client does
  /// **not** hold: the opposite failure to a phantom, and the one that hides
  /// behind every other readout.
  ///
  /// [`phantom_entities`](Self::phantom_entities) counts what a client wrongly
  /// keeps, and on its own it can be driven to zero by a client that keeps
  /// nothing. Render error cannot see an omission either, since it only averages
  /// over entities both sides have. Without this, a recovery mechanism that
  /// quietly starves the mirror scores perfectly on everything else.
  pub fn missing_entities(&self, player: usize, controls: &Controls) -> usize {
    let eye = self.server.players[player];
    let held: std::collections::BTreeSet<Handle> = self.clients[player]
      .render_at()
      .map(|at| self.clients[player].render(controls, at))
      .unwrap_or_default()
      .into_iter()
      .map(|(h, _, _)| h)
      .collect();
    self
      .server
      .live_enemies()
      .filter(|(_, e)| e.pos.dist(eye) <= crate::sim::types::VIEW_RADIUS)
      .filter(|(h, _)| !held.contains(h))
      .count()
  }

  /// Entities this client still holds that name no live server entity: corpses
  /// it was never able to remove. Should always be zero; anything else means a
  /// despawn failed to land, which is invisible from bandwidth or error numbers.
  pub fn phantom_entities(&self, player: usize, controls: &Controls) -> usize {
    let live: std::collections::BTreeSet<Handle> = self.truth().into_iter().map(|(h, _)| h).collect();
    self.client_render(player, controls).into_iter().filter(|(h, _, _)| !live.contains(h)).count()
  }

  /// Total bytes by part: samples, spawns, despawns, projectiles, other.
  pub fn bytes_by_part(&self) -> [u64; 5] {
    self.bytes_by_part
  }

  /// Every despawn set observed, for measuring how it would encode.
  pub fn despawn_sets(&self) -> &[Vec<u32>] {
    &self.despawn_sets
  }

  /// Size of the dense id space the sets index into.
  pub fn slot_space(&self) -> usize {
    self.server.slot_count()
  }

  /// Packets after which a client's mirror disagreed with the server's digest.
  /// Should be zero; anything else means a delta failed to land.
  /// How many times a client's baseline aged out and it needed the whole visible
  /// set resent. Recovery's price, and the readout that says whether the sent
  /// history is long enough for the link.
  pub fn full_resends(&self) -> u64 {
    self.server.full_resends()
  }

  pub fn digest_mismatches(&self) -> u64 {
    self.clients.iter().map(|c| c.digest_mismatches()).sum()
  }

  pub fn frames_lost(&self) -> u64 {
    self.clients.iter().map(|c| c.frames_lost()).sum()
  }

  pub fn deaths_seen(&self, player: usize) -> u64 {
    self.clients[player].deaths_seen
  }

  pub fn players(&self) -> &[Vec2] {
    &self.server.players
  }

  /// A player's health, its respawn shield, and its death count, and the current
  /// difficulty ramp, for the health bar and the readouts.
  pub fn player_health(&self, player: usize) -> u8 {
    self.server.player_health(player)
  }

  pub fn player_invuln(&self, player: usize) -> bool {
    self.server.is_player_invuln(player)
  }

  pub fn player_deaths(&self, player: usize) -> u64 {
    self.server.player_deaths[player]
  }

  pub fn difficulty(&self) -> f32 {
    self.server.difficulty()
  }

  /// The floating damage numbers this client is drawing.
  pub fn client_popups(&self, player: usize) -> &[crate::sim::client::DamagePopup] {
    &self.clients[player].popups
  }

  /// The hit sparks and death explosions this client is drawing.
  pub fn client_bursts(&self, player: usize) -> &[crate::sim::client::Burst] {
    &self.clients[player].bursts
  }

  /// What one client would draw, at its own render instant. Empty before the
  /// stream has started, which is the join transient.
  pub fn client_render(&self, player: usize, controls: &Controls) -> Vec<(Handle, Vec2, EnemyKind)> {
    self.clients[player]
      .render_at()
      .map(|at| self.clients[player].render(controls, at))
      .unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::RemoteMode;

  fn run(controls: &Controls, secs: u64) -> World {
    let mut w = World::new(controls, 4, 0x5EED_D00D);
    for _ in 0..(secs * 60) {
      w.step(16, Vec2::new(1.0, 0.0), controls);
    }
    w
  }

  #[test]
  fn killed_enemies_actually_disappear_from_the_client() {
    // A death must reach the client and remove the entity. Getting the handle's
    // generation wrong makes the lookup miss silently, so the corpse stands there
    // forever and the client's known set grows without bound while the server's
    // live population stays flat. Compare the two.
    let c = Controls::default();
    let mut w = run(&c, 10);

    assert!(w.kills() > 50, "combat actually killed things: {}", w.kills());
    assert!(w.deaths_seen(0) > 0, "the client was told about deaths");

    // Let the wire drain with combat stopped, so no *new* deaths are in flight.
    // Without this the check trips on despawns that are simply still travelling,
    // which is latency, not a leak.
    let quiet = Controls { combat: false, ..c };
    for _ in 0..90 {
      w.step(16, Vec2::new(1.0, 0.0), &quiet);
    }

    // The precise invariant: every entity the client holds must name a live one.
    // A corpse the client could not remove is invisible in bandwidth and in the
    // error numbers (which skip unmatched handles), so check it directly.
    for p in 0..w.player_count() {
      let phantoms = w.phantom_entities(p, &quiet);
      assert_eq!(phantoms, 0, "player {p} still holds {phantoms} entities that are dead on the server");
    }
  }

  #[test]
  fn a_pulsed_repulsor_does_not_ring_up_and_wall_the_player_off() {
    // Regression for a rule bug that looked like a rendering artifact.
    //
    // The first repulsor was a permanent aura with a hard sign flip at a fixed
    // radius: flee inside it, chase outside it, both at chase speed. That is a
    // stable equilibrium, so every enemy converged on exactly that radius and
    // stopped, leaving a motionless ring the player could never be reached
    // through. It also flattered every accuracy readout, because stationary
    // entities are trivially easy to predict.
    //
    // The fix is not a weaker push, which would ring up at the same radius: the
    // equilibrium comes from the sign flip, not the magnitude. It is to make the
    // repulsion *intermittent*, so there is no radius at which net motion is
    // zero. This checks the consequence rather than the mechanism: enemies can
    // still reach you.
    let c = Controls {
      spread_players: false,
      ..Controls::default()
    };
    let w = run_circling(&c, 60);
    assert!(w.wallet(0).has(crate::sim::types::Upgrade::Repulsor), "the run really did buy the upgrade");

    let you = w.players()[0];
    let closest = w.truth().iter().map(|(_, pos)| pos.dist(you)).fold(f32::MAX, f32::min);
    // Inside the *largest* pulse is the meaningful bar. A stable ring at any
    // radius keeps the nearest enemy pinned out there, which is what the old aura
    // did at 190px; getting well inside the biggest pulse proves nothing has
    // settled at a fixed distance.
    assert!(
      closest < crate::sim::types::REPULSOR_MAX_RADIUS,
      "enemies must still be able to close: nearest is {closest:.0}px, against a largest pulse of {:.0}px",
      crate::sim::types::REPULSOR_MAX_RADIUS
    );
  }

  /// Drives player 0 in a circle rather than in a straight line.
  ///
  /// The shared `run` helper holds one direction, which pins the player against a
  /// wall within a couple of seconds. That is fine for the relevance and loss
  /// tests, and useless for anything about *contested* pickups: a stationary
  /// player wanders into nobody. Contest rates measured against it read zero for
  /// the wrong reason.
  fn run_circling(controls: &Controls, secs: u64) -> World {
    let mut w = World::new(controls, 4, 0x5EED_D00D);
    for i in 0..(secs * 60) {
      let t = i as f32 * 0.02;
      w.step(16, Vec2::new(t.cos(), t.sin()), controls);
    }
    w
  }

  #[test]
  fn a_consistent_timeline_turns_predicting_a_pickup_into_replaying_it() {
    // This test used to assert the opposite, and the reversal is the finding.
    //
    // Coins are claimed by whoever is nearest inside the radius. When the client
    // applied each packet the moment it landed, it judged "am I nearest?" against
    // a mixture: its own position fresh, everyone else's a latency out of date.
    // That is a genuine guess, and it was wrong often enough to measure, more
    // often the worse the link (15 taken back over thirty seconds at 250 ms).
    //
    // Now that packets are played out when the render clock reaches the instant
    // they describe, the client is not guessing at the present, it is **replaying
    // the past**: it evaluates the same rule, on the same positions, at the same
    // instant the server did. Same inputs and same function give the same answer,
    // so the guess stops being a guess. Measured at 250 ms, 166 predictions and
    // zero taken back.
    //
    // What it costs instead is *lateness*: the pickup is shown when the timeline
    // reaches it rather than the moment the player believes it happened. That is
    // the trade the playout buffer makes everywhere, and it is why this is worth
    // pinning: the same change that removed the snapping introduced the delay.
    let together = Controls { spread_players: false, ..Controls::default() };
    // The delay has to cover the trip, or the client is rendering ahead of every
    // sample it holds and cannot replay anything. 250 ms of latency needs a
    // timeline at least that far back, plus jitter and an interval.
    let together = Controls { render_delay_ms: 350, ..together };
    let confirmed = run_circling(&Controls { predict_balance: false, latency_ms: 250, ..together }, 30);
    let far = run_circling(&Controls { predict_balance: true, latency_ms: 250, ..together }, 30);

    assert_eq!(confirmed.denied_claims(), 0, "waiting for the server cannot mispredict");
    assert!(
      far.clients[0].predicted_total > 50,
      "the test needs the client to actually be predicting: {}",
      far.clients[0].predicted_total
    );
    // Rare, not zero, and the earlier "zero" was over-fitted to one configuration.
    // Replaying the past makes a contested claim very nearly deterministic,
    // because both sides run one rule over the same positions at the same
    // instant. What survives is the boundary: two players equidistant to within
    // float error still flip, and no amount of timeline agreement removes that.
    // Measured across declared delays of 300 to 500 ms it is 1 to 3 claims,
    // against hundreds predicted, and it does not improve with a wider delay,
    // which is what says it is a tie-break rather than a staleness problem.
    assert!(
      far.denied_claims() * 20 <= far.clients[0].predicted_total as u64,
      "adjudicating on the server's own timeline should lose races only at the boundary: {} of {}",
      far.denied_claims(),
      far.clients[0].predicted_total
    );
  }

  #[test]
  fn a_predicted_balance_is_an_offset_not_a_counter() {
    // The modelling mistake this feature was built to expose, pinned so it cannot
    // come back. Maintaining the believed balance as its own running total drifts,
    // because it models income and not spending: every purchase the server
    // approves decrements the authoritative balance and leaves the local one
    // untouched. Measured, that was 115 coins of drift over one run.
    //
    // Deriving it as confirmed-plus-outstanding cannot drift, because there is
    // nothing to drift from, and it absorbs anything the server does that the
    // client never modelled.
    let c = Controls {
      spread_players: false,
      predict_balance: true,
      latency_ms: 250,
      ..Controls::default()
    };
    let w = run_circling(&c, 30);
    let (believed, truth) = w.balance(0);
    let drift = (believed as i64 - truth as i64).abs();
    assert!(drift < 20, "a derived balance tracks the truth within what is in flight: {believed} against {truth}");
    assert!(
      (0..w.player_count()).map(|p| w.coins_claimed(p)).sum::<u32>() > 100,
      "and the run really did move a lot of currency"
    );
  }

  #[test]
  fn an_optimistic_purchase_makes_the_client_simulate_the_wrong_world() {
    // The coupling that makes currency a netcode question rather than a gameplay
    // one. `Repulsor` is an input to `step_enemy`, the rule every client runs
    // locally, so a purchase shown before it is confirmed does not merely display
    // a wrong number: for as long as the answer takes to arrive, the client is
    // simulating enemies fleeing a player the server has chasing them.
    //
    // Nothing else in these examples has this shape. Every divergence measured so
    // far comes from latency or from chaos, never from a wrong boolean upstream of
    // the physics.
    let together = Controls {
      spread_players: false,
      predict_balance: true,
      ..Controls::default()
    };
    let near = run_circling(&Controls { latency_ms: 80, ..together }, 30);
    let far = run_circling(&Controls { latency_ms: 250, ..together }, 30);
    for d in [300u64, 350, 400, 500] {
      let t = Controls { render_delay_ms: d, ..together };
      let f = run_circling(&Controls { predict_balance: true, latency_ms: 250, ..t }, 30);
      println!("  delay {d}ms -> predicted {} denied {}", f.clients[0].predicted_total, f.denied_claims());
    }
    let confirmed = run_circling(&Controls { predict_balance: false, latency_ms: 250, ..together }, 30);

    assert_eq!(confirmed.wrong_rule_packets(), 0, "waiting for confirmation never simulates a rule the server rejects");
    assert!(
      far.wrong_rule_packets() > near.wrong_rule_packets(),
      "and the window widens with latency: {} at 250ms against {} at 80ms",
      far.wrong_rule_packets(),
      near.wrong_rule_packets()
    );
  }

  #[test]
  fn crowd_summaries_cover_the_world_relevance_culling_deletes() {
    // Relevance culling answers "which distant entities does this client get?"
    // with "none", so a client's knowledge stops at its radius and anything it
    // draws of the wider world is borrowed from an authority it does not have.
    // Aggregation answers the useful question instead, which is how *precisely*
    // it needs them, and a headcount at a centroid is enough to draw a crowd.
    //
    // The cost is the point: a summary stands for an arbitrary number of enemies,
    // so awareness of the whole arena is a few bytes rather than a share of the
    // population.
    let culled = run(&Controls { crowd_lod_theta: 0.0, ..Controls::default() }, 12);
    let lod = run(&Controls { crowd_lod_theta: 1.5, ..Controls::default() }, 12);

    assert_eq!(culled.crowd_awareness(0), 0.0, "culling alone leaves the client knowing nothing out there");
    assert!(lod.crowd_awareness(0) > 0.9, "summaries should cover almost all of it: {:.0}%", lod.crowd_awareness(0) * 100.0);
    assert!(
      lod.crowds(0).len() < 40,
      "and do it with a handful of stand-ins for thousands of enemies: {}",
      lod.crowds(0).len()
    );
    assert!(
      lod.bytes_per_sec() < culled.bytes_per_sec() * 1.10,
      "for under a tenth more traffic: {:.0} against {:.0} B/s",
      lod.bytes_per_sec(),
      culled.bytes_per_sec()
    );
  }

  #[test]
  fn packet_loss_strands_corpses_and_ack_recovery_clears_them() {
    // The failure a delta-relevance stream has by construction, and the fix.
    //
    // Diffing against "what I last sent" assumes every packet arrives. One that
    // does not leaves the client holding entities nothing will ever retract
    // again, because the server has already moved its baseline past them. At 25%
    // loss that is hundreds of corpses per run, and no bandwidth or error readout
    // shows it.
    //
    // Diffing against what the client *acknowledged* re-derives the difference
    // instead. Two details are load-bearing and both were wrong at first: the
    // baseline must be the newest **contiguous** acknowledgement rather than the
    // newest bit set, and the sets must be keyed by index **and generation**, or
    // a retraction sent after the slot was recycled names the new occupant and
    // the corpse survives.
    let lossy = Controls {
      loss_pct: 25.0,
      ..Controls::default()
    };
    let naive = run(&Controls { ack_recovery: false, ..lossy }, 20);
    let recovered = run(&Controls { ack_recovery: true, ..lossy }, 20);

    let naive_phantoms: usize = (0..naive.player_count()).map(|p| naive.phantom_entities(p, &lossy)).sum();
    let recovered_phantoms: usize = (0..recovered.player_count()).map(|p| recovered.phantom_entities(p, &lossy)).sum();

    assert!(naive_phantoms > 50, "loss really does strand entities without recovery: {naive_phantoms}");
    assert!(
      recovered_phantoms < naive_phantoms / 10,
      "recovery should clear almost all of them: {recovered_phantoms} against {naive_phantoms}"
    );

    // And the mirror-image failure, which the first working version traded the
    // corpses for: a client can also be *short* of entities it should hold, and
    // no other readout shows it. Phantoms alone can be driven to zero by a client
    // that keeps nothing, so both directions have to be pinned or the next change
    // is free to fix one by breaking the other.
    let naive_missing: usize = (0..naive.player_count()).map(|p| naive.missing_entities(p, &lossy)).sum();
    let recovered_missing: usize = (0..recovered.player_count()).map(|p| recovered.missing_entities(p, &lossy)).sum();
    assert!(
      recovered_missing <= naive_missing,
      "recovery must not trade corpses for omissions: {recovered_missing} missing against {naive_missing}"
    );
    assert!(
      recovered.mean_render_error(&lossy) < naive.mean_render_error(&lossy) * 0.5,
      "and the positions converge too: {:.1}px against {:.1}px",
      recovered.mean_render_error(&lossy),
      naive.mean_render_error(&lossy)
    );
  }

  #[test]
  fn turning_coins_off_does_not_panic() {
    // Regression: with coins off the server sends no wallets, so the client's
    // wallet list is empty, and it used to index that list by player id and
    // panic (the first networked joiner takes seat 3, so the index was 3 into a
    // length of 0). The offline world reaches the same client path.
    let c = Controls { coins: false, ..Controls::default() };
    let w = run(&c, 5);
    assert!(w.alive_enemies() > 0, "the game still runs with coins off");
    assert_eq!(w.balance(0), (0, 0), "no currency exists, so both balances read zero");
  }

  #[test]
  fn the_digest_agrees_when_every_delta_lands() {
    // The check itself must not cry wolf: with the stream applied correctly, the
    // client's mirror matches the server's summary on every packet.
    let c = Controls::default();
    let w = run(&c, 10);
    assert!(w.deaths_seen(0) > 0, "there were deltas worth checking");
    assert_eq!(w.digest_mismatches(), 0, "a correct mirror should never disagree");
  }

  #[test]
  fn relevance_culls_the_broadcast() {
    let on = Controls::default();
    let off = Controls { relevance: false, ..Controls::default() };

    let w_on = run(&on, 4);
    let w_off = run(&off, 4);

    assert!(
      w_on.bytes_per_sec() * 10.0 < w_off.bytes_per_sec(),
      "relevance should cut bandwidth by more than 10x: {:.0} vs {:.0} B/s",
      w_on.bytes_per_sec(),
      w_off.bytes_per_sec()
    );
  }

  /// The worst a client's idea of a *peer* is behind the server's truth, in px,
  /// sampled every frame rather than at the end so a transient is not missed.
  fn worst_peer_lag(controls: &Controls, secs: u64) -> f32 {
    let mut w = World::new(controls, 4, 0x5EED_D00D);
    let mut worst = 0.0f32;
    // Warm up first. Before the first packet lands a client believes the origin,
    // and the distance from there to wherever a player actually started is
    // arena-sized: measuring through that reports the same number for every
    // configuration, which is what the first version of this test did.
    let warmup = 2 * 60;
    for frame in 0..(secs * 60) {
      w.step(16, Vec2::new(1.0, 0.0), controls);
      if frame < warmup {
        continue;
      }
      // What client 0 believes about player 1, against where player 1 really is.
      let believed = w.clients[0].players()[1];
      worst = worst.max(believed.dist(w.server.players[1]));
    }
    worst
  }

  #[test]
  fn a_peer_is_drawn_smoothly_rather_than_snapped_to_the_newest_sample() {
    // The complaint this pair of fixes came from, as a number. A peer drawn at
    // the raw newest sample stands still between arrivals and jumps a whole
    // send interval's worth when one lands; drawn through `RemoteView` against a
    // clock steered by the stream, it moves a little every frame.
    //
    // Measuring the *largest single frame's movement* rather than an average is
    // the point: an averaged position error is exactly the metric that said this
    // was fine while it visibly stuttered.
    // A 10 Hz player stream on an 80 ms link needs 80 + 20 + 100 back before two
    // samples bracket T. Declared, because under one shared timeline the delay is
    // a chosen number rather than something the client discovers.
    let controls = Controls { sync_hz: 4, player_sync_hz: 10, render_delay_ms: 250, ..Controls::default() };
    let mut w = World::new(&controls, 4, 0x5EED_D00D);

    let mut worst_raw = 0.0f32;
    let mut worst_drawn = 0.0f32;
    let mut last_raw: Option<Vec2> = None;
    let mut last_drawn: Option<Vec2> = None;
    let warmup = 2 * 60;
    for frame in 0..(8 * 60) {
      w.step(16, Vec2::new(1.0, 0.0), &controls);
      let raw = w.clients[0].players()[1];
      let drawn = w.clients[0].render_at().map(|at| w.clients[0].render_players(at)[1]).unwrap_or_default();
      if frame >= warmup {
        if let Some(previous) = last_raw {
          worst_raw = worst_raw.max(previous.dist(raw));
        }
        if let Some(previous) = last_drawn {
          worst_drawn = worst_drawn.max(previous.dist(drawn));
        }
      }
      last_raw = Some(raw);
      last_drawn = Some(drawn);
    }

    assert!(worst_raw > 8.0, "the raw sample should visibly jump between arrivals: {worst_raw:.1} px");
    assert!(
      worst_drawn * 2.0 < worst_raw,
      "a drawn peer should move in small steps, not the raw sample's jumps: {worst_drawn:.1} px against {worst_raw:.1} px"
    );
  }

  #[test]
  fn a_bad_link_underruns_instead_of_quietly_getting_an_older_world() {
    // This test asserted the opposite, and the reversal is the finding.
    //
    // It used to require that a steady link buy itself a *smaller* buffer, on the
    // reasoning that every millisecond of render delay is a peer drawn further
    // behind where they are. The buffer was sized from measured arrival jitter,
    // so each client picked its own render instant.
    //
    // That conflated two unrelated things. Latency and jitter describe when bytes
    // show up, a property of one link. The render delay describes which moment is
    // on screen, a property of the world. Letting the first move the second means
    // no two clients agree on what "now" is, the server cannot say what any of
    // them has yet to play, and, worst, a player on a bad link is silently shown
    // an older world than everybody else while every readout says fine.
    //
    // Fixed on the server's clock, both links render the same instant. The
    // difference does not vanish, it becomes visible: the link that cannot keep
    // up reports underruns, packets that arrived after the moment they describe
    // had already gone past.
    // Measured on the timeline itself rather than on a position, because the two
    // answer different questions. How far behind the server clock the client is
    // *rendering* is the timeline, and it must be identical. How far the drawn
    // peer is from the truth is a consequence, and it legitimately gets worse
    // when the data for that instant has not arrived, which is the starvation the
    // underrun counter is there to name.
    fn timeline_and_underruns(controls: &Controls) -> (i64, u64) {
      let mut w = World::new(controls, 4, 0x5EED_D00D);
      let mut worst = 0i64;
      for frame in 0..(8 * 60) {
        w.step(16, Vec2::new(1.0, 0.0), controls);
        if frame < 180 {
          continue;
        }
        if let Some(at) = w.clients[0].render_at() {
          let behind = w.server.now_ms() as i64 - at.server_time_ms() as i64;
          worst = worst.max((behind - controls.render_delay_ms as i64).abs());
        }
      }
      (worst, w.clients[0].underruns())
    }

    let base = Controls { sync_hz: 16, player_sync_hz: 30, latency_ms: 0, render_delay_ms: 100, ..Controls::default() };
    let (steady_drift, steady_late) = timeline_and_underruns(&Controls { jitter_ms: 0, ..base });
    let (rough_drift, rough_late) = timeline_and_underruns(&Controls { jitter_ms: 200, ..base });

    assert!(steady_drift <= 16, "a steady link should render exactly the declared delay back: off by {steady_drift} ms");
    assert!(
      rough_drift <= 16,
      "and so should a jittery one, because the timeline is the world's and not the link's: off by {rough_drift} ms"
    );
    assert_eq!(steady_late, 0, "a link inside the declared delay should never underrun");
    assert!(rough_late > 0, "a link outside it should say so rather than absorb it: {rough_late} underruns");
  }

  #[test]
  fn a_slow_entity_stream_does_not_have_to_starve_the_player_stream() {
    // The bug this pair of rates exists for. Enemy positions can be stale,
    // because every client runs the enemies' own rule and only needs correcting.
    // Player positions cannot: they are the *input* to that rule, so a stale one
    // makes every enemy aim at a ghost and the whole horde changes heading at
    // once each time the stream ticks. It also makes peers visibly teleport.
    //
    // With one shared rate at 1 Hz a player can be a full second stale, and at
    // PLAYER_SPEED that is 190 px, nearly half a view radius. Splitting the rates
    // keeps the entity stream at 1 Hz and the input to it fresh.
    let shared = Controls { sync_hz: 1, player_sync_hz: 1, latency_ms: 0, jitter_ms: 0, ..Controls::default() };
    let split = Controls { player_sync_hz: 30, ..shared };

    let starved = worst_peer_lag(&shared, 6);
    let fed = worst_peer_lag(&split, 6);

    println!("worst peer lag: shared 1 Hz = {starved:.0} px, split (1 Hz entities / 30 Hz players) = {fed:.0} px");
    assert!(starved > 100.0, "one shared rate should strand a peer badly: {starved:.0} px");
    assert!(
      fed * 4.0 < starved,
      "a separate player rate should keep the input to the enemy rule fresh: {fed:.0} px against {starved:.0} px"
    );
  }

  #[test]
  fn simulating_beats_interpolating_at_a_low_sync_rate() {
    // The claim the case study rests on: at 1 Hz, running the behaviour rule
    // locally is far closer to the truth than interpolating between samples,
    // because interpolation renders a full interval in the past.
    let base = Controls { sync_hz: 1, ..Controls::default() };
    let sim = run(&Controls { mode: RemoteMode::Simulate, ..base }, 6);
    let interp = run(&Controls { mode: RemoteMode::Interpolate, ..base }, 6);

    let e_sim = sim.mean_render_error(&Controls { mode: RemoteMode::Simulate, ..base });
    let e_interp = interp.mean_render_error(&Controls { mode: RemoteMode::Interpolate, ..base });
    assert!(e_sim < e_interp, "simulate {e_sim:.1}px should beat interpolate {e_interp:.1}px at 1 Hz");
  }

  #[test]
  fn a_client_only_knows_what_is_relevant_to_it() {
    let c = Controls::default();
    let w = run(&c, 3);
    for p in 0..w.player_count() {
      assert!(w.known_entities(p) > 0, "player {p} sees something");
      assert!(
        w.known_entities(p) < w.enemy_count() / 4,
        "player {p} knows {} of {} enemies, far short of everything",
        w.known_entities(p),
        w.enemy_count()
      );
    }
  }

  #[test]
  fn compact_ids_and_quantized_positions_cut_the_wire_cost() {
    let w = run(&Controls::default(), 3);
    assert!(
      w.bytes_per_sec() * 2.0 < w.naive_bytes_per_sec(),
      "3-byte ids + quantized positions should more than halve it: {:.0} vs {:.0} B/s",
      w.bytes_per_sec(),
      w.naive_bytes_per_sec()
    );
  }
}
