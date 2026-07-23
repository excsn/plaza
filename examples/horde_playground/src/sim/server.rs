//! The authoritative server: owns every enemy, simulates them at full rate, runs
//! the combat, and sends each player only what is relevant to them, far less
//! often than it simulates.
//!
//! Entities live in **recycled slots**. A dead enemy frees its slot and bumps its
//! generation, so a handle naming the previous occupant is distinguishable from
//! one naming the new. Whether that generation actually earns its keep is
//! something this example measures rather than assumes.

use std::collections::BTreeSet;

use plaza_client_utils::ack::AckWindow;
use plaza_server_utils::aggregate::{AggregateTree, WeightedPoint};
use plaza_server_utils::relevance::{GridQuantizer, SetDigest, SpatialGrid, VisibilitySet};

use crate::sim::types::{
  coin_pull, difficulty, enemy_speed_scale, repulsor_pulse, step_coin, step_enemy, Coin, CoinId, Controls, Crowd, Enemy, EnemyKind, EntityIndex, Handle, LeaveReason, Packet, PlayerId, Projectile, Sample, Spawn, Upgrade, Vec2, Wallet, COIN_PICKUP_RADIUS, COIN_DROP_IN, COIN_TTL_MS, ARENA_H, ARENA_W, CELL_SIZE, CONTACT_HIT_DAMAGE, FIRE_INTERVAL_MS, HIT_INVULN_MS, HIT_RADIUS, NOVA_INTERVAL_MS,
  NOVA_DAMAGE, NOVA_RADIUS, PLAYER_CONTACT_RADIUS, PLAYER_INVULN_MS, PLAYER_MAX_HEALTH, PLAYER_SPEED, PROJECTILE_SPEED, PROJECTILE_TTL, SIM_DT, VIEW_RADIUS, WAVE_INTERVAL_MS,
};

const RETARGET_INTERVAL_MS: u64 = 1000;

/// Who is driving a player this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Seat {
  /// A person, with the direction they asked for.
  Steered(Vec2),
  /// Nobody, so the scripted drift takes it.
  #[default]
  Bot,
}

/// One entity's identity as a single integer: slot index in the high bits,
/// generation in the low. The same shape the digest hashes, deliberately, so the
/// recovery baseline and the agreement check cannot disagree about what an
/// entity *is*.
fn slot_key(idx: EntityIndex, generation: u16) -> u32 {
  (idx << 16) | generation as u32
}

#[derive(Clone, Debug)]
struct Slot {
  generation: u16,
  alive: bool,
  enemy: Enemy,
}

#[derive(Clone, Debug)]
pub struct Server {
  pub players: Vec<Vec2>,
  slots: Vec<Slot>,
  free: Vec<EntityIndex>,
  pub projectiles: Vec<Projectile>,

  grid: SpatialGrid<EntityIndex>,
  prev_vis: Vec<VisibilitySet>,
  cur_vis: Vec<VisibilitySet>,
  announced_target: Vec<PlayerId>,

  /// Enemies that died since the last send, so their despawn is explicit rather
  /// than inferred, which is what lets a slot be safely reused.
  deaths_since_send: Vec<(EntityIndex, u16)>,

  clock_ms: u64,
  sim_accum_ms: u64,
  sync_accum_ms: u64,
  retarget_accum_ms: u64,
  wave_accum_ms: u64,
  fire_accum_ms: u64,
  nova_accum_ms: u64,

  target_population: usize,
  pub kills: u64,
  pub nova_kills_last: usize,
  /// When the last area pulse fired, so the renderer can show it happening.
  pub last_nova_ms: Option<u64>,

  candidates: Vec<EntityIndex>,
  entered_buf: Vec<u32>,
  left_buf: Vec<u32>,

  next_seq: u64,
  /// Per client, the visibility set each recent packet would leave them holding.
  ///
  /// This is the whole recovery mechanism. A delta stream has to be diffed
  /// against a baseline, and the naive choice is "what I last sent", which
  /// silently assumes every packet arrives. Keeping the last few sent states lets
  /// the diff be taken against the last one the client *acknowledged* instead, so
  /// whatever a dropped packet carried is simply re-derived by the next diff. No
  /// retransmission buffer and no gap detection: the diff already knows how to
  /// say "here is the difference between what you have and what you need".
  /// Keyed by **(index, generation)**, not by index alone, and that is the fix
  /// for the bug this mechanism otherwise reintroduces.
  ///
  /// A retraction has to name the occupant the client was told about. A bare
  /// index cannot: by the time a lost retraction is re-derived, the slot may have
  /// died and been refilled, so naming its *current* generation retracts an
  /// entity the client has never heard of while the corpse it does hold is never
  /// mentioned again. Working in the same key space the digest uses makes the
  /// baseline and the agreement check answer the same question.
  sent_states: Vec<std::collections::VecDeque<(u64, BTreeSet<u32>)>>,
  /// The newest acknowledged baseline per client, once one is known.
  acked_keys: Vec<Option<(u64, BTreeSet<u32>)>>,
  /// The last sequence sent to each client, which is the baseline the naive
  /// policy diffs against.
  last_sent_seq: Vec<Option<u64>>,
  /// Everything each client *might* be holding: the acknowledged baseline plus
  /// everything announced since. What a retraction has to be measured against,
  /// because an entity that entered and left inside that gap is in neither the
  /// baseline nor the current set, so a single diff never mentions it.
  assumed_held: Vec<BTreeSet<u32>>,
  /// Currency on the ground, and what each player has banked and bought.
  pub coins: Vec<Coin>,
  next_coin_id: CoinId,
  pub wallets: Vec<Wallet>,
  pub coins_claimed: Vec<u32>,
  /// Coins nobody reached before they expired. The number that says whether the
  /// pickup rule can actually keep up with the drop rule.
  pub coins_expired: u64,
  /// Claims since the last send, so a client is *told* who won a coin rather than
  /// inferring it from the coin's absence. Inference would leave a client that
  /// mispredicted a pickup unable to tell "you lost it" from "you never saw it".
  claims_since_send: Vec<(PlayerId, CoinId)>,
  /// Purchase requests the server refused, which is the number that says whether
  /// a client's balance belief is drifting.
  pub denied_purchases: u64,
  /// Refusals to report to each client on the next packet.
  denials_since_send: Vec<Vec<Upgrade>>,

  /// Packets sent whose fate is still unknown, for a readout.
  pub unacked: Vec<usize>,
  /// How often a client's baseline aged out and it had to be sent the whole
  /// visible set. The cost of recovery, and the number that says whether the
  /// history window is long enough.
  pub full_resends: Vec<u64>,

  /// Each player's health as a float so fractional per-step contact damage
  /// accumulates; it goes out quantized to a byte. Zero means a death is being
  /// resolved this step.
  player_health: Vec<f32>,
  /// When each player can next take damage: pushed forward briefly by every hit
  /// and longer by a respawn. Gameplay immunity, not sent on the wire.
  player_invuln_until_ms: Vec<u64>,
  /// When each player's respawn *shield* ends. The subset of immunity worth
  /// drawing, so a hit's brief window does not flicker a shield on.
  player_shield_until_ms: Vec<u64>,
  /// Times each player has been overrun, for the scoreboard.
  pub player_deaths: Vec<u64>,
  /// Weapon hits since the last send, `(where, damage)`, for floating numbers.
  hits_since_send: Vec<(Vec2, u8)>,
}

/// How many sent states to remember per client. A packet older than this is past
/// recovery, and the baseline falls back to the last sent state, which is the
/// naive behaviour: the mechanism degrades to the thing it replaced rather than
/// to something worse.
const SENT_HISTORY: usize = 24;

impl Server {
  pub fn new(enemy_count: usize, player_count: usize, spread: bool) -> Self {
    let players = (0..player_count).map(|p| player_start(p, spread)).collect::<Vec<_>>();
    let slots = (0..enemy_count)
      .map(|i| Slot {
        generation: 0,
        alive: true,
        enemy: {
          let kind = EnemyKind::from_seed(i as u32);
          Enemy {
            pos: scatter(i as u32),
            target: (i % player_count.max(1)) as PlayerId,
            kind,
            health: kind.max_health(),
          }
        },
      })
      .collect::<Vec<_>>();
    let announced_target = slots.iter().map(|s| s.enemy.target).collect();

    Self {
      players,
      slots,
      free: Vec::new(),
      projectiles: Vec::new(),
      grid: SpatialGrid::new(GridQuantizer::new((0.0, 0.0), CELL_SIZE)),
      prev_vis: (0..player_count).map(|_| VisibilitySet::with_capacity(enemy_count as u32)).collect(),
      cur_vis: (0..player_count).map(|_| VisibilitySet::with_capacity(enemy_count as u32)).collect(),
      announced_target,
      deaths_since_send: Vec::new(),
      clock_ms: 0,
      sim_accum_ms: 0,
      sync_accum_ms: 0,
      retarget_accum_ms: 0,
      wave_accum_ms: 0,
      fire_accum_ms: 0,
      nova_accum_ms: 0,
      target_population: enemy_count,
      kills: 0,
      nova_kills_last: 0,
      last_nova_ms: None,
      candidates: Vec::new(),
      entered_buf: Vec::new(),
      left_buf: Vec::new(),
      next_seq: 0,
      sent_states: (0..player_count).map(|_| std::collections::VecDeque::with_capacity(SENT_HISTORY)).collect(),
      acked_keys: vec![None; player_count],
      last_sent_seq: vec![None; player_count],
      assumed_held: (0..player_count).map(|_| BTreeSet::new()).collect(),
      coins: Vec::new(),
      next_coin_id: 0,
      wallets: vec![Wallet::default(); player_count],
      coins_claimed: vec![0; player_count],
      coins_expired: 0,
      claims_since_send: Vec::new(),
      denied_purchases: 0,
      denials_since_send: vec![Vec::new(); player_count],
      unacked: vec![0; player_count],
      full_resends: vec![0; player_count],
      player_health: vec![PLAYER_MAX_HEALTH; player_count],
      player_invuln_until_ms: vec![0; player_count],
      player_shield_until_ms: vec![0; player_count],
      player_deaths: vec![0; player_count],
      hits_since_send: Vec::new(),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  /// Adopts an existing clock, so a rebuilt world keeps time continuous.
  ///
  /// A fresh `Server` starts at zero; handing a running arena a rebuilt one would
  /// jump `server_time_ms` backward, and a client computing a packet's age from it
  /// would suddenly see every sample as seconds stale and simulate it far forward,
  /// which reads as the whole horde dashing at the player.
  pub fn set_clock(&mut self, ms: u64) {
    self.clock_ms = ms;
  }

  /// How many slots exist, alive or free: the id space a presence mask covers.
  pub fn slot_count(&self) -> usize {
    self.slots.len()
  }

  pub fn alive_count(&self) -> usize {
    self.slots.iter().filter(|s| s.alive).count()
  }

  /// A player's health, quantized the way the packet carries it.
  pub fn player_health(&self, p: usize) -> u8 {
    self.player_health[p].round().clamp(0.0, PLAYER_MAX_HEALTH) as u8
  }

  /// Whether a player is in its post-respawn shield (the drawn kind).
  pub fn is_player_invuln(&self, p: usize) -> bool {
    self.clock_ms < self.player_shield_until_ms[p]
  }

  /// The current difficulty multiplier, for a readout.
  pub fn difficulty(&self) -> f32 {
    difficulty(self.clock_ms)
  }

  /// Every live enemy with its handle, for rendering the ground truth.
  pub fn live_enemies(&self) -> impl Iterator<Item = (Handle, &Enemy)> {
    self
      .slots
      .iter()
      .enumerate()
      .filter(|(_, s)| s.alive)
      .map(|(i, s)| (Handle::new(i as EntityIndex, s.generation), &s.enemy))
  }

  /// Advances by `dt_ms`. `local_input` steers player 0; the rest drift.
  ///
  /// The offline shape, kept so the headless tests and the single-process
  /// playground are unchanged by networking.
  pub fn advance(&mut self, dt_ms: u64, local_input: Vec2, controls: &Controls) -> Vec<(PlayerId, Packet)> {
    let mut seats = vec![Seat::Bot; self.players.len()];
    if !seats.is_empty() {
      seats[0] = Seat::Steered(local_input);
    }
    self.advance_seats(dt_ms, &seats, controls)
  }

  /// Advances with an explicit occupant per seat, which is what a real server
  /// has: some seats are people, the rest are bots, and the set changes as
  /// players come and go.
  pub fn advance_seats(&mut self, dt_ms: u64, seats: &[Seat], controls: &Controls) -> Vec<(PlayerId, Packet)> {
    self.clock_ms += dt_ms;
    self.sim_accum_ms += dt_ms;
    self.retarget_accum_ms += dt_ms;

    let step_ms = (SIM_DT * 1000.0) as u64;
    while self.sim_accum_ms >= step_ms {
      self.sim_accum_ms -= step_ms;
      self.step(seats, controls);
    }

    if self.retarget_accum_ms >= RETARGET_INTERVAL_MS {
      self.retarget_accum_ms = 0;
      self.retarget();
    }

    if controls.combat {
      self.wave_accum_ms += dt_ms;
      while self.wave_accum_ms >= WAVE_INTERVAL_MS {
        self.wave_accum_ms -= WAVE_INTERVAL_MS;
        self.spawn_wave();
      }
      self.fire_accum_ms += dt_ms;
      while self.fire_accum_ms >= FIRE_INTERVAL_MS {
        self.fire_accum_ms -= FIRE_INTERVAL_MS;
        self.fire_weapons();
      }
      self.nova_accum_ms += dt_ms;
      while self.nova_accum_ms >= NOVA_INTERVAL_MS {
        self.nova_accum_ms -= NOVA_INTERVAL_MS;
        self.nova();
      }
    }

    self.sync_accum_ms += dt_ms;
    let interval = controls.sync_interval_ms();
    if self.sync_accum_ms >= interval {
      self.sync_accum_ms -= interval;
      return self.build_packets(controls);
    }
    Vec::new()
  }

  fn step(&mut self, seats: &[Seat], controls: &Controls) {
    let t = self.clock_ms as f32 / 1000.0;
    for (p, pos) in self.players.iter_mut().enumerate() {
      let (dx, dy) = match seats.get(p) {
        Some(Seat::Steered(dir)) => (dir.x, dir.y),
        // An unoccupied seat drifts rather than standing still, so an arena with
        // one player in it is still a game and the horde still has somewhere to go.
        _ => player_drift(p, t, controls.spread_players),
      };
      pos.x = (pos.x + dx * PLAYER_SPEED * SIM_DT).clamp(0.0, ARENA_W);
      pos.y = (pos.y + dy * PLAYER_SPEED * SIM_DT).clamp(0.0, ARENA_H);
    }
    let speed_scale = enemy_speed_scale(self.clock_ms);
    for slot in self.slots.iter_mut().filter(|s| s.alive) {
      let t_idx = slot.enemy.target as usize % self.players.len();
      let repel = self.wallets[t_idx].has(Upgrade::Repulsor).then(|| repulsor_pulse(self.clock_ms)).flatten();
      step_enemy(&mut slot.enemy, self.players[t_idx], repel, speed_scale, SIM_DT);
    }
    self.resolve_contact_damage();
    if controls.combat {
      self.step_projectiles();
    }
    if controls.coins {
      self.step_coins();
    }
  }

  /// Touching an enemy costs one discrete hit, harder as the difficulty ramps,
  /// and then a brief invulnerability so a whole pile lands one hit per window
  /// rather than one per tick. Being reduced to zero refills health and grants a
  /// longer shield in place: a continuous sandbox, not a game over. In place
  /// rather than teleporting, so the player's position (and the enemy dynamics
  /// every proximity readout measures) stays continuous; the shield is what lets
  /// you walk out of the pile that got you.
  fn resolve_contact_damage(&mut self) {
    let now = self.clock_ms;
    let hit_damage = difficulty(now) * CONTACT_HIT_DAMAGE;
    for p in 0..self.players.len() {
      if now < self.player_invuln_until_ms[p] {
        continue;
      }
      let eye = self.players[p];
      let touched = self.slots.iter().filter(|s| s.alive).any(|s| s.enemy.pos.dist(eye) <= PLAYER_CONTACT_RADIUS + s.enemy.kind.radius());
      if !touched {
        continue;
      }
      self.player_health[p] -= hit_damage;
      if self.player_health[p] <= 0.0 {
        self.player_deaths[p] += 1;
        self.player_health[p] = PLAYER_MAX_HEALTH;
        self.player_invuln_until_ms[p] = now + PLAYER_INVULN_MS;
        self.player_shield_until_ms[p] = now + PLAYER_INVULN_MS;
      } else {
        self.player_invuln_until_ms[p] = now + HIT_INVULN_MS;
      }
    }
  }

  /// Coin motion, expiry, and the contested claim.
  fn step_coins(&mut self) {
    let attractors: Vec<(Vec2, f32, f32)> = self
      .wallets
      .iter()
      .enumerate()
      .map(|(p, w)| {
        let (radius, speed) = coin_pull(w.has(Upgrade::Magnet));
        (self.players[p], radius, speed)
      })
      .collect();
    for coin in &mut self.coins {
      step_coin(coin, &attractors, SIM_DT);
    }

    // Nearest player inside the radius claims it. A rule rather than a race, so
    // it is deterministic on the server and *predictable but not certain* on a
    // client, which is exactly the interesting case: a client decides whether it
    // is nearest using remote positions that are a latency out of date.
    let players = self.players.clone();
    let mut claimed: Vec<(usize, CoinId)> = Vec::new();
    let mut expired = 0u64;
    self.coins.retain(|coin| {
      let mut best: Option<(f32, usize)> = None;
      for (p, pos) in players.iter().enumerate() {
        let d = coin.pos.dist(*pos);
        if d <= COIN_PICKUP_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
          best = Some((d, p));
        }
      }
      match best {
        Some((_, p)) => {
          claimed.push((p, coin.id));
          false
        }
        None => {
          let alive = self.clock_ms.saturating_sub(coin.spawned_ms) < COIN_TTL_MS;
          if !alive {
            expired += 1;
          }
          alive
        }
      }
    });
    self.coins_expired += expired;
    for (p, id) in claimed {
      self.wallets[p].balance += 1;
      self.coins_claimed[p] += 1;
      self.claims_since_send.push((p as PlayerId, id));
    }
  }

  /// Applies a purchase request. The only thing a client may ask for, and it is
  /// still the server that decides.
  pub fn receive_buy(&mut self, player: usize, upgrade: Upgrade) {
    let wallet = &mut self.wallets[player];
    if wallet.has(upgrade) || wallet.balance < upgrade.cost() {
      self.denied_purchases += 1;
      self.denials_since_send[player].push(upgrade);
      return;
    }
    wallet.balance -= upgrade.cost();
    wallet.upgrades.push(upgrade);
    wallet.upgrades.sort_unstable();
  }

  fn step_projectiles(&mut self) {
    for p in &mut self.projectiles {
      p.pos.x += p.vel.x * SIM_DT;
      p.pos.y += p.vel.y * SIM_DT;
      p.ttl -= SIM_DT;
    }
    self.projectiles.retain(|p| p.ttl > 0.0);

    // Resolve hits. Small counts, so a direct scan is fine here; a real server
    // would query the same spatial grid it already maintains.
    let mut hits: Vec<(usize, EntityIndex)> = Vec::new();
    for (pi, proj) in self.projectiles.iter().enumerate() {
      for (i, slot) in self.slots.iter().enumerate() {
        if slot.alive && slot.enemy.pos.dist(proj.pos) <= HIT_RADIUS + slot.enemy.kind.radius() {
          hits.push((pi, i as EntityIndex));
          break;
        }
      }
    }
    // Damage after the scan so indices stay stable during it. A shot spends
    // itself on contact whether or not it was the killing blow. Each hit is also
    // recorded as a floating-number event, at the enemy it landed on.
    let mut spent: Vec<usize> = Vec::new();
    for (pi, target) in hits {
      spent.push(pi);
      self.hits_since_send.push((self.slots[target as usize].enemy.pos, 1));
      self.damage(target, 1);
    }
    spent.sort_unstable();
    spent.dedup();
    for pi in spent.into_iter().rev() {
      if pi < self.projectiles.len() {
        self.projectiles.remove(pi);
      }
    }
  }

  /// The area-of-effect pulse: kills everything close to each player at once,
  /// which is the mass-despawn burst worth measuring.
  fn nova(&mut self) {
    let mut caught: Vec<EntityIndex> = Vec::new();
    for player in self.players.clone() {
      for (i, slot) in self.slots.iter().enumerate() {
        if slot.alive && slot.enemy.pos.dist(player) <= NOVA_RADIUS {
          caught.push(i as EntityIndex);
        }
      }
    }
    caught.sort_unstable();
    caught.dedup();

    // Damage, not instant death: the swarm is cleared and a brute walks out of
    // it wounded, which is what makes the kinds read differently.
    let before = self.kills;
    for idx in caught {
      self.damage(idx, NOVA_DAMAGE);
    }
    self.nova_kills_last = (self.kills - before) as usize;
    self.last_nova_ms = Some(self.clock_ms);
  }

  /// Applies damage, and announces a death only when one actually happens.
  fn damage(&mut self, idx: EntityIndex, amount: u8) {
    let (health, generation) = {
      let slot = &mut self.slots[idx as usize];
      if !slot.alive {
        return;
      }
      slot.enemy.health = slot.enemy.health.saturating_sub(amount);
      (slot.enemy.health, slot.generation)
    };
    if health == 0 {
      self.deaths_since_send.push((idx, generation));
      self.kill(idx);
    }
  }

  fn kill(&mut self, idx: EntityIndex) {
    let slot = &mut self.slots[idx as usize];
    if !slot.alive {
      return;
    }
    slot.alive = false;
    let died_at = slot.enemy.pos;
    // Bump on free: any handle naming the previous occupant no longer matches.
    slot.generation = slot.generation.wrapping_add(1);
    self.free.push(idx);
    self.kills += 1;
    if self.kills.is_multiple_of(COIN_DROP_IN as u64) {
      let id = self.next_coin_id;
      self.next_coin_id += 1;
      self.coins.push(Coin {
        id,
        pos: died_at,
        spawned_ms: self.clock_ms,
      });
    }
  }

  /// Each player's weapon auto-fires at the nearest enemy, Vampire Survivors
  /// style: you move, the weapons aim.
  fn fire_weapons(&mut self) {
    for player in self.players.clone() {
      let mut best: Option<(f32, Vec2)> = None;
      for slot in self.slots.iter().filter(|s| s.alive) {
        let d = slot.enemy.pos.dist(player);
        if d < VIEW_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
          best = Some((d, slot.enemy.pos));
        }
      }
      if let Some((d, target)) = best
        && d > 1.0
      {
        let (dx, dy) = (target.x - player.x, target.y - player.y);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        self.projectiles.push(Projectile {
          pos: player,
          vel: Vec2::new(dx / len * PROJECTILE_SPEED, dy / len * PROJECTILE_SPEED),
          ttl: PROJECTILE_TTL,
        });
      }
    }
  }

  /// Reinforcements arrive just outside somebody's view, reusing the slots the
  /// dead left behind.
  fn spawn_wave(&mut self) {
    let alive = self.alive_count();
    if alive >= self.target_population {
      return;
    }
    let want = (self.target_population - alive).min(40);
    for k in 0..want {
      let p = (self.clock_ms as usize / 97 + k) % self.players.len();
      let around = self.players[p];
      let angle = ((self.clock_ms as usize * 7 + k * 131) % 628) as f32 / 100.0;
      let r = VIEW_RADIUS * 1.15;
      let pos = Vec2::new((around.x + r * angle.cos()).clamp(0.0, ARENA_W), (around.y + r * angle.sin()).clamp(0.0, ARENA_H));
      let kind = EnemyKind::from_seed(self.clock_ms as u32 ^ (k as u32).wrapping_mul(7919));
      let enemy = Enemy {
        pos,
        target: p as PlayerId,
        kind,
        health: kind.max_health(),
      };

      if let Some(idx) = self.free.pop() {
        let slot = &mut self.slots[idx as usize];
        slot.alive = true;
        slot.enemy = enemy;
        self.announced_target[idx as usize] = enemy.target;
      } else {
        self.slots.push(Slot { generation: 0, alive: true, enemy });
        self.announced_target.push(enemy.target);
      }
    }
  }

  fn retarget(&mut self) {
    for slot in self.slots.iter_mut().filter(|s| s.alive) {
      let mut best = slot.enemy.target;
      let mut best_d = f32::MAX;
      for (p, pos) in self.players.iter().enumerate() {
        let d = slot.enemy.pos.dist(*pos);
        if d < best_d {
          best_d = d;
          best = p as PlayerId;
        }
      }
      slot.enemy.target = best;
    }
  }

  /// Folds in a client's acknowledgement, moving its baseline forward.
  ///
  /// Takes the **contiguous** frontier, not the newest bit set, and the
  /// distinction is the whole correctness of this.
  ///
  /// A bitmask answers "what arrived", which is what a retransmitting protocol
  /// wants: it names the holes to refill. A protocol that re-derives instead
  /// needs a state the client provably *reached*, and receiving packet N+1 after
  /// losing N does not put the client in the state N+1 implies, because whatever
  /// N announced and N+1 had no reason to repeat is simply gone. Taking the
  /// newest set bit hands the diff a state that never existed, and the client is
  /// then permanently short by the gap's contents. Measured: that mistake made
  /// recovery indistinguishable from no recovery, identical mismatch counts at
  /// every loss rate.
  pub fn receive_ack(&mut self, player: usize, newest: u64, mask: u64) {
    let window = AckWindow::from_encoded(newest, mask);
    let floor = self.acked_keys[player].as_ref().map(|(s, _)| *s);
    let mut frontier: Option<(u64, BTreeSet<u32>)> = None;
    for (seq, state) in self.sent_states[player].iter() {
      if floor.is_some_and(|f| *seq <= f) {
        continue;
      }
      if !window.contains(*seq) {
        break;
      }
      frontier = Some((*seq, state.clone()));
    }
    if let Some(found) = frontier {
      self.acked_keys[player] = Some(found);
    }
    self.unacked[player] = self.sent_states[player].iter().filter(|(seq, _)| !window.contains(*seq)).count();
  }

  /// Whether this client's acknowledged baseline has fallen out of the history,
  /// leaving nothing valid to diff against.
  fn baseline_is_stale(&self, player: usize) -> bool {
    let Some((acked_seq, _)) = &self.acked_keys[player] else {
      return false;
    };
    let history = &self.sent_states[player];
    history.len() >= SENT_HISTORY && history.front().is_some_and(|(oldest, _)| *acked_seq < *oldest)
  }

  fn build_packets(&mut self, controls: &Controls) -> Vec<(PlayerId, Packet)> {
    if controls.relevance {
      self.grid.clear();
      for (i, slot) in self.slots.iter().enumerate() {
        if slot.alive {
          self.grid.insert(i as EntityIndex, slot.enemy.pos.x, slot.enemy.pos.y);
        }
      }
    }

    let player_list: Vec<(PlayerId, Vec2)> = self.players.iter().enumerate().map(|(p, pos)| (p as PlayerId, *pos)).collect();
    // Everyone's health and shield, indexed by player id. Small, and sent to all
    // so a client draws a bar over a peer as well as itself.
    let health_list: Vec<u8> = self.player_health.iter().map(|h| h.round().clamp(0.0, PLAYER_MAX_HEALTH) as u8).collect();
    let invuln_list: Vec<bool> = self.player_shield_until_ms.iter().map(|&t| self.clock_ms < t).collect();
    let mut out = Vec::with_capacity(self.players.len());

    let seq = self.next_seq;
    self.next_seq += 1;

    // One tree over the whole live population, walked once per player. The build
    // is the expensive half and it is viewer independent, which is what makes
    // this affordable for a crowd this size.
    let crowd_tree = (controls.crowd_lod_theta > 0.0).then(|| {
      let points: Vec<WeightedPoint> = self
        .slots
        .iter()
        .filter(|s| s.alive)
        // Weight one per enemy: the quantity being summarised is a headcount, so
        // a summary's weight *is* how many are standing there.
        .map(|s| WeightedPoint::new(s.enemy.pos.x, s.enemy.pos.y, 1.0))
        .collect();
      AggregateTree::build_in(&points, (ARENA_W * 0.5, ARENA_H * 0.5), ARENA_W.max(ARENA_H), 10)
    });
    let mut summaries: Vec<plaza_server_utils::aggregate::Summary> = Vec::new();

    for p in 0..self.players.len() {
      let eye = self.players[p];

      // Re-anchor the baseline to what the client has actually confirmed. With
      // this off, `prev_vis` stays "what I last sent" and a dropped packet is
      // lost forever; with it on, the very next diff carries the difference.
      let mut baseline_seq = self.last_sent_seq[p];

      // A baseline that has aged out of the history is not recoverable by
      // re-derivation: whatever the missing packet carried is no longer
      // expressible as a difference from anything still known. The only honest
      // response is to stop pretending and send the whole visible set, which is
      // what every delta protocol does at this point and what this example was
      // missing. Without it the frontier simply steps over the gap when history
      // evicts it, and the client is left permanently short by that packet's
      // contents while every readout looks healthy.
      if controls.ack_recovery && self.baseline_is_stale(p) {
        self.acked_keys[p] = None;
        self.assumed_held[p].clear();
        // `prev_vis` too, and forgetting it is a quiet way to make this a no-op:
        // with the acknowledged baseline dropped the code falls back to the naive
        // diff, and diffing against a stale "what I last sent" emits nothing. The
        // resync counter still ticks, so the readout claims a recovery that never
        // happened. Clearing it is what makes the next diff the full set.
        self.prev_vis[p].clear();
        self.full_resends[p] += 1;
        baseline_seq = None;
      }

      let mut baseline_keys: BTreeSet<u32> = BTreeSet::new();
      let recovering = controls.ack_recovery && self.acked_keys[p].is_some();
      if let Some((acked_seq, acked)) = &self.acked_keys[p]
        && controls.ack_recovery
      {
        // Two baselines, and they have to be built by *opposite* operations.
        //
        // The client's true holdings are the acknowledged state plus everything
        // announced since, minus everything retracted since, and the middle terms
        // are exactly the packets whose fate is unknown. So neither bound is the
        // acknowledged state on its own:
        //
        // - What to **send** must assume the least: the intersection, the
        //   acknowledged state minus anything a later packet may have retracted.
        //   Using the raw acknowledged state instead claims the client still holds
        //   an entity we told it to drop, so when that entity becomes relevant
        //   again it is never re-sent and the client is permanently short of it.
        // - What to **retract** must assume the most: the union, everything the
        //   client could be holding.
        //
        // Getting the union right and leaving the other half as the raw baseline
        // trades one silent failure for its mirror image: corpses became
        // omissions, which no readout but a dedicated missing-entity count can
        // see.
        baseline_keys.clone_from(acked);
        self.assumed_held[p].clone_from(acked);
        for (sent_seq, state) in &self.sent_states[p] {
          if *sent_seq > *acked_seq {
            baseline_keys.retain(|key| state.contains(key));
            self.assumed_held[p].extend(state.iter().copied());
          }
        }
        baseline_seq = Some(*acked_seq);
      }

      let mut packet = Packet {
        server_time_ms: self.clock_ms,
        seq,
        baseline_seq,
        players: player_list.clone(),
        player_health: health_list.clone(),
        player_invuln: invuln_list.clone(),
        ..Default::default()
      };

      // Deaths are announced explicitly, before the visibility diff, so a slot
      // that is reused in the same interval reads as despawn-then-spawn rather
      // than silently becoming a different entity.
      // Filtered by what the client *might* hold, not by the baseline. An enemy
      // that appeared after the acknowledged state and then died is absent from
      // the baseline, so filtering on that would silently skip announcing its
      // death and leave the client holding a corpse it can never be told about.
      for &(idx, gen_at_death) in &self.deaths_since_send {
        // Under recovery the retraction is derived from the key sets below, which
        // name the right occupant even after the slot is refilled. This explicit
        // announcement stays for the naive path only.
        if !recovering && self.prev_vis[p].contains(idx) {
          // `gen_at_death` was recorded *before* `kill` bumped the slot, so it is
          // already the generation the client was told at spawn. Do not adjust it:
          // naming any other generation makes the client's lookup miss and the
          // dead entity linger forever.
          packet.left.push((Handle::new(idx, gen_at_death), LeaveReason::Died));
          self.prev_vis[p].remove(idx);
          // Deliberately *not* removed from `assumed_held`. A death is announced
          // once, out of band from the visibility diff, so if that packet is lost
          // nothing ever mentions the entity again and the client holds a corpse
          // forever. Leaving it in the assumed set means the next diff sees it as
          // something the client might hold and no longer should, and retracts it
          // again. Announcing is not the same as being heard, and only an
          // acknowledgement may retire the assumption.
        }
      }

      self.cur_vis[p].clear();
      if controls.relevance {
        self.candidates.clear();
        self.grid.query_radius(eye.x, eye.y, VIEW_RADIUS, &mut self.candidates);
        for &idx in &self.candidates {
          if self.slots[idx as usize].enemy.pos.dist(eye) <= VIEW_RADIUS {
            self.cur_vis[p].insert(idx);
          }
        }
      } else {
        for (i, slot) in self.slots.iter().enumerate() {
          if slot.alive {
            self.cur_vis[p].insert(i as EntityIndex);
          }
        }
      }

      // What this client should hold after applying, in the key space the digest
      // uses: index *and* generation, so "which occupant" is part of the answer.
      let cur_keys: BTreeSet<u32> = self.cur_vis[p].iter().map(|idx| slot_key(idx, self.slots[idx as usize].generation)).collect();

      self.entered_buf.clear();
      self.left_buf.clear();
      if recovering {
        // Two different baselines, because the two halves of a diff answer two
        // different questions.
        //
        // What to *send* is decided against the acknowledged state, the newest
        // one the client provably holds. What to *retract* is decided against
        // everything the client might be holding, which includes anything
        // announced since. An entity that entered and left inside that gap is in
        // neither the baseline nor the current set, so a single diff never
        // mentions it and the client keeps it forever.
        for key in cur_keys.difference(&baseline_keys) {
          let idx = *key >> 16;
          let slot = &self.slots[idx as usize];
          packet.entered.push(Spawn {
            handle: Handle::new(idx, (*key & 0xFFFF) as u16),
            pos: slot.enemy.pos,
            target: slot.enemy.target,
            kind: slot.enemy.kind,
          });
          self.entered_buf.push(idx);
        }
        for key in self.assumed_held[p].difference(&cur_keys) {
          let (idx, generation) = (*key >> 16, (*key & 0xFFFF) as u16);
          // Dead if the slot has moved on, gone out of view if it has not. Naming
          // the generation from the *key* rather than from the slot is the whole
          // point: a refilled slot must not have its new occupant retracted in
          // place of the corpse the client is actually holding.
          let reason = if self.slots[idx as usize].generation != generation || !self.slots[idx as usize].alive {
            LeaveReason::Died
          } else {
            LeaveReason::OutOfRange
          };
          packet.left.push((Handle::new(idx, generation), reason));
        }
        self.entered_buf.sort_unstable();
      } else {
        self.cur_vis[p].diff(&self.prev_vis[p], &mut self.entered_buf, &mut self.left_buf);
        for &idx in &self.entered_buf {
          let slot = &self.slots[idx as usize];
          packet.entered.push(Spawn {
            handle: Handle::new(idx, slot.generation),
            pos: slot.enemy.pos,
            target: slot.enemy.target,
            kind: slot.enemy.kind,
          });
        }
        for &idx in &self.left_buf {
          packet.left.push((Handle::new(idx, self.slots[idx as usize].generation), LeaveReason::OutOfRange));
        }
      }

      for idx in self.cur_vis[p].iter() {
        if self.entered_buf.binary_search(&idx).is_ok() {
          continue;
        }
        let slot = &self.slots[idx as usize];
        let target = (slot.enemy.target != self.announced_target[idx as usize]).then_some(slot.enemy.target);
        packet.samples.push(Sample {
          handle: Handle::new(idx, slot.generation),
          pos: slot.enemy.pos,
          target,
        });
      }

      // Everything too far to send individually, as crowds rather than as
      // nothing. Built once per tick over the whole population and walked once
      // per player, so the cost of knowing about the rest of the world does not
      // scale with how much of it there is.
      if let Some(tree) = &crowd_tree {
        tree.summarize(eye.x, eye.y, controls.crowd_lod_theta, &mut summaries);
        for summary in &summaries {
          // Anything inside the view radius is already being sent for real, at
          // full fidelity. A summary there would be a worse copy of what the
          // client is about to receive anyway.
          if summary.x.hypot(summary.y - eye.y).is_nan() {
            continue;
          }
          let d = Vec2::new(summary.x, summary.y).dist(eye);
          if d <= VIEW_RADIUS {
            continue;
          }
          packet.crowds.push(Crowd {
            pos: Vec2::new(summary.x, summary.y),
            count: summary.count.min(u16::MAX as u32) as u16,
          });
        }
      }

      // Coins near enough to race for, and everyone's wallet. Both are small
      // enough that partial knowledge would cost more in confusion than the bytes
      // save: a coin you cannot see is one you cannot contest.
      if controls.coins {
        packet.coins = self.coins.iter().filter(|c| c.pos.dist(eye) <= VIEW_RADIUS * 1.2).copied().collect();
        packet.wallets = self.wallets.clone();
        packet.claims = self.claims_since_send.clone();
        packet.denied_buys = std::mem::take(&mut self.denials_since_send[p]);
      }

      // Shots close enough to matter, and the hits near this player.
      packet.projectiles = self.projectiles.iter().filter(|pr| pr.pos.dist(eye) <= VIEW_RADIUS * 1.2).copied().collect();
      packet.hits = self.hits_since_send.iter().filter(|(pos, _)| pos.dist(eye) <= VIEW_RADIUS * 1.2).copied().collect();

      // Summarise what the client must hold once it applies this packet. Keyed by
      // index *and* generation, so agreeing on membership is not enough: both
      // sides have to agree on which occupant of each slot it is.
      packet.visible_digest = SetDigest::from_keys(
        self.cur_vis[p]
          .iter()
          .map(|idx| ((idx as u64) << 16) | self.slots[idx as usize].generation as u64),
      )
      .digest();

      std::mem::swap(&mut self.prev_vis[p], &mut self.cur_vis[p]);

      // Remember what this packet would leave the client holding, so a later
      // acknowledgement can name it as the baseline.
      self.last_sent_seq[p] = Some(seq);
      let history = &mut self.sent_states[p];
      history.push_back((seq, cur_keys));
      while history.len() > SENT_HISTORY {
        history.pop_front();
      }
      out.push((p as PlayerId, packet));
    }

    for (i, slot) in self.slots.iter().enumerate() {
      self.announced_target[i] = slot.enemy.target;
    }
    self.deaths_since_send.clear();
    self.claims_since_send.clear();
    self.hits_since_send.clear();
    out
  }
}

fn scatter(i: u32) -> Vec2 {
  let x = ((i.wrapping_mul(2_654_435_761) >> 8) % 10_000) as f32 / 10_000.0 * ARENA_W;
  let y = ((i.wrapping_mul(40_503) >> 4) % 10_000) as f32 / 10_000.0 * ARENA_H;
  Vec2::new(x, y)
}

fn player_start(p: usize, spread: bool) -> Vec2 {
  if spread {
    let fx = (p % 2) as f32;
    let fy = (p / 2) as f32;
    Vec2::new(ARENA_W * (0.25 + 0.5 * fx), ARENA_H * (0.25 + 0.5 * fy))
  } else {
    Vec2::new(ARENA_W * 0.5 + p as f32 * 40.0, ARENA_H * 0.5)
  }
}

fn player_drift(p: usize, t: f32, spread: bool) -> (f32, f32) {
  let phase = p as f32 * 1.9;
  if spread {
    ((t * 0.5 + phase).cos(), (t * 0.4 + phase).sin())
  } else {
    ((t * 0.5).cos() + 0.05 * phase.cos(), (t * 0.4).sin() + 0.05 * phase.sin())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_player_pressed_by_enemies_takes_hits_and_is_eventually_overrun() {
    let mut server = Server::new(200, 4, false);
    // Pin every enemy onto player 0 and point it at them, so the pile presses.
    let p0 = server.players[0];
    for slot in server.slots.iter_mut() {
      slot.enemy.pos = p0;
      slot.enemy.target = 0;
    }
    let controls = Controls::default();
    // Stand still in the pile for fifteen seconds.
    for _ in 0..(15 * 60) {
      server.advance(16, Vec2::new(0.0, 0.0), &controls);
    }
    assert!(server.player_deaths[0] > 0, "a player standing in the horde is overrun");
  }

  #[test]
  fn a_hit_buys_a_brief_invulnerability_so_a_pile_cannot_chain() {
    // One tick of contact should cost exactly one hit, not one per enemy per
    // tick: the i-frame is what stops a swarm deleting you in an instant.
    let mut server = Server::new(50, 1, false);
    let p0 = server.players[0];
    for slot in server.slots.iter_mut() {
      slot.enemy.pos = p0;
      slot.enemy.target = 0;
    }
    let before = server.player_health(0);
    // A single simulation step (advance runs the fixed-step loop once for 16ms).
    server.advance(16, Vec2::new(0.0, 0.0), &Controls::default());
    let dropped = before - server.player_health(0);
    assert!(dropped > 0, "contact hurt");
    assert!(dropped <= (CONTACT_HIT_DAMAGE.ceil() as u8) + 1, "but only one hit's worth, not one per enemy: {dropped}");
  }

  #[test]
  fn difficulty_ramps_with_time_and_is_capped() {
    assert!((difficulty(0) - 1.0).abs() < 1e-6, "starts at 1x");
    assert!(difficulty(120_000) > difficulty(30_000), "harder as minutes pass");
    assert!(difficulty(3_600_000) <= 8.0, "but bounded");
    assert!(enemy_speed_scale(600_000) > 1.0, "and enemies speed up with it");
  }
}
