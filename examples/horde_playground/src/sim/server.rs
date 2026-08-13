//! The authoritative server: owns every enemy, simulates them at full rate, runs
//! the combat, and sends each player only what is relevant to them, far less
//! often than it simulates.
//!
//! Entities live in **recycled slots**. A dead enemy frees its slot and bumps its
//! generation, so a handle naming the previous occupant is distinguishable from
//! one naming the new. Whether that generation actually earns its keep is
//! something this example measures rather than assumes.

use plaza_client_utils::{FixedTimestep, Periodic, SlotAllocator, SlotKey};
use std::collections::BTreeSet;

use plaza_server_utils::aggregate::{AggregateTree, WeightedPoint};
use plaza_server_utils::delta::{DeltaBaseline, RecoveryPolicy};
use plaza_server_utils::history::HistoricalStateBuffer;
use plaza_server_utils::input_schedule::{InputSchedule, InputWindow};
use plaza_server_utils::relevance::{GridQuantizer, SetDigest, SpatialGrid, TierBoundary, VisibilitySet};
use plaza_server_utils::subscription::{Audience, Subscriptions};

use crate::sim::types::{PlayerFrame, 
  coin_pull, difficulty, step_player, enemy_speed_scale, repulsor_pulse, quantize_far, step_coin, step_enemy, Coin, CoinId, Controls, Crowd, Enemy, EnemyKind, EntityIndex, Handle, LeaveReason, Packet, PlayerId, Projectile, Sample, Shot, ShotId, Spawn, Upgrade, Vec2, Wallet, COIN_PICKUP_RADIUS, COIN_DROP_IN, COIN_TTL_MS, ARENA_H, ARENA_W, CELL_SIZE, CONTACT_HIT_DAMAGE, FIRE_INTERVAL_MS, HIT_INVULN_MS, HIT_RADIUS, NOVA_INTERVAL_MS,
  NOVA_DAMAGE, NOVA_RADIUS, PLAYER_CONTACT_RADIUS, PLAYER_INVULN_MS, PLAYER_MAX_HEALTH, PROJECTILE_SPEED, PROJECTILE_TTL, SIM_DT, SIM_STEP_MS, VIEW_RADIUS, WAVE_INTERVAL_MS,
};

const RETARGET_INTERVAL_MS: u64 = 1000;

/// The near tier's boundary, with hysteresis: see
/// [`TierBoundary`](plaza_server_utils::relevance::TierBoundary) for why the
/// two radii differ.
const NEAR_TIER: TierBoundary = TierBoundary::new(VIEW_RADIUS * 1.3, VIEW_RADIUS * 1.5);

/// How many players are in one squad.
///
/// Small on purpose, and the reason the second channel is affordable: a
/// subscription set is a handful of long-lived entries where a grid query is a
/// constantly changing many.
pub const SQUAD_SIZE: usize = 4;

/// Everyone divided into squads, which is what a raid roster is.
///
/// Assigned rather than chosen, because this example has no interface for
/// choosing and the measurement does not need one: what is being priced is the
/// channel, not the social feature on top of it.
fn squads_of(player_count: usize) -> Subscriptions<PlayerId> {
  let mut squads = Subscriptions::new(SQUAD_SIZE);
  for group in (0..player_count as PlayerId).collect::<Vec<_>>().chunks(SQUAD_SIZE) {
    for other in group.iter().skip(1) {
      squads.pair(group[0], *other);
    }
  }
  squads
}
/// One far-tier update every this many player frames. At the default 8 Hz
/// player rate that is one every two seconds, which is a marker gliding on a
/// map rather than a position anybody aims with.
///
/// Halving it was measured and rejected: the worst placement error stayed at
/// 138 px and the far tier cost 20 KiB/s more at 128 players, so the error is
/// not bound by this rate at this point and the faster setting bought nothing.
const FAR_TIER_EVERY: u32 = 16;

/// Who is driving a player this tick.
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
  pub players: Vec<Vec2>,
  /// Which slots are occupied, and by which generation. The pool hands out
  /// [`SlotKey`]s in the same key space the digest hashes and `DeltaBaseline`
  /// diffs in, which is the point of taking it from here: an identity invented
  /// locally would have to agree with those by convention instead of by
  /// construction.
  pool: SlotAllocator,
  /// The enemies themselves, indexed by slot. Deliberately parallel to the pool
  /// rather than owned by it: `VisibilitySet` wants dense indices and so does
  /// this, and an allocator that owned the payload would compose with neither.
  enemies: Vec<Enemy>,
  pub projectiles: Vec<Projectile>,

  /// Where every enemy was, recently.
  ///
  /// Kept so an accuracy figure can ask where things were **at the instant a
  /// client was drawing**, rather than where they are now. Comparing a delayed
  /// client against the present charges it for a delay it chose, which is the
  /// distortion behind every render-error number this example has published.
  history: HistoricalStateBuffer<Handle, Vec2, u64>,

  grid: SpatialGrid<EntityIndex>,
  cur_vis: Vec<VisibilitySet>,
  /// Who each player has chosen to care about, wherever they are.
  ///
  /// The second channel, beside the spatial one. A squad is a handful of
  /// entries with a lifetime of a whole session, where a grid query is a fresh
  /// answer every round over a set that never stops changing, and neither
  /// expresses the other.
  squads: Subscriptions<PlayerId>,
  /// How many of each recipient's relevant players are there only because they
  /// were subscribed to, for the panel.
  squad_added: Vec<usize>,
  /// Each recipient's squad, so a frame can say why somebody is in it.
  squad_of: Vec<Vec<PlayerId>>,
  announced_target: Vec<PlayerId>,


  clock_ms: u64,
  /// Inputs waiting for the tick they name, per seat: the accepting window,
  /// the reject-not-correct rule and the counters all live in the block. What
  /// stays horde's is what an executed input *does* (become the held
  /// direction) and the naive path below.
  input_schedules: Vec<InputSchedule<Vec2>>,
  /// The direction each seat is currently holding, once an input for it has come
  /// due. Persists between inputs, which is what makes the steering continuous.
  held: Vec<Option<Vec2>>,
  /// Inputs taken by the naive apply-on-arrival path, which bypasses the
  /// schedule and so is counted here.
  naive_inputs: u64,
  /// Simulation time, spent in whole fixed steps. The step has to be the same one
  /// a client integrates by, which is why it is taken from here rather than
  /// passed in.
  sim: FixedTimestep,
  /// When to build the next round of entity packets. Its interval is a live
  /// setting, so dragging the send-rate slider takes effect from now rather than
  /// restarting the period.
  sync: Periodic,
  /// When to send the players, which is far more often. See
  /// [`Controls::player_sync_hz`] for why the two rates are separate.
  player_sync: Periodic,
  /// The player frame this tick produced, if it was due. Held rather than
  /// returned so `advance_seats` keeps its signature and every caller opts in.
  pending_players: Option<Vec<(PlayerId, PlayerFrame)>>,
  /// Re-aiming the horde. Idempotent within a tick, so it fires at most once
  /// however long the frame was.
  retarget: Periodic,
  /// Spawning, firing and the area pulse. Each occurrence matters (three
  /// intervals in one frame means three waves), so these count rather than
  /// answering yes or no.
  wave: Periodic,
  fire: Periodic,
  nova: Periodic,

  target_population: usize,
  pub kills: u64,
  pub nova_kills_last: usize,
  /// When the last area pulse fired, so the renderer can show it happening.
  pub last_nova_ms: Option<u64>,

  candidates: Vec<EntityIndex>,
  entered_buf: Vec<u32>,
  /// Spawns announced in the last send round, for the readout above.
  last_spawns: usize,
  /// Player frames until the far tier is sent again.
  far_tier_countdown: u32,
  /// Recently dead handles and when they died. Pruned to the deepest render
  /// delay, so it is bounded by the kill rate over that window rather than by
  /// the length of the session.
  recently_dead: Vec<(Handle, u64)>,
  /// Which players each client needs: the ones it can see, plus the ones its
  /// visible enemies are chasing. Recomputed on each entity round and reused by
  /// the player stream, which runs on its own clock.
  relevant_players: Vec<Vec<PlayerId>>,
  /// Wallets that changed since the last send round. A wallet is a rule input
  /// and changes only on a pickup or a purchase, so it is sent on change rather
  /// than restated in every packet.
  wallets_dirty: BTreeSet<PlayerId>,
  /// Shots fired since the last send, and shots that ended **early** because
  /// they hit something. Expiry is not in here: both sides can compute it.
  shots_fired_since_send: Vec<Shot>,
  /// Ends carry the shot's **origin**, not its death place, purely so they can
  /// be filtered by the same rule the fire event was: exactly the clients that
  /// were told a shot started are the ones told it stopped. Filtering on where
  /// it died would miss a client holding a shot that travelled out of its view,
  /// and that client would fly the shot on through what killed it.
  shots_ended_since_send: Vec<(ShotId, Vec2)>,
  next_shot_id: ShotId,

  next_seq: u64,
  /// Per client, the reliability half of the delta stream: what each packet
  /// would leave them holding, what they have acknowledged, and therefore what
  /// to send next.
  ///
  /// The block is set-theoretic and knows nothing about enemies, which is why
  /// it lives in `server_utils`: every game gets the same two bugs fixed for
  /// free, a joiner sent a difference against a baseline it never held, and a
  /// mirror that drifts and can never recover.
  baselines: Vec<DeltaBaseline>,
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

/// How many sent states to remember per client. Cover the packets that can be
/// in flight plus the acknowledgement's return trip; older than this cannot be
/// re-derived and forces a full rebuild.
const SENT_HISTORY: usize = 24;

/// Ticks of enemy positions kept, so an accuracy figure can be asked at a
/// client's render instant rather than at the present.
const TRUTH_HISTORY: usize = 80;

/// How long a seat may go silent before [`DeltaBaseline`]'s flow control
/// throttles it: see [`DeltaBaseline::with_flow`] for the pathology this
/// prevents (a hidden tab streamed full baselines at full rate). Matches the
/// client's own discontinuity rule (its `LOST_AHEAD_MS` is also 3 s), so both
/// sides agree on when a gap stops being jitter.
const STALLED_AFTER_MS: u64 = 3_000;
/// A stalled seat still gets one packet this often: the probe that lets a
/// client which quietly resumes reading rediscover the stream and
/// re-acknowledge.
const STALL_KEEPALIVE_MS: u64 = 1_000;

impl Server {
  pub fn new(enemy_count: usize, player_count: usize, spread: bool) -> Self {
    let players = (0..player_count).map(|p| player_start(p, player_count, spread)).collect::<Vec<_>>();
    let mut pool = SlotAllocator::with_capacity(enemy_count);
    let enemies = (0..enemy_count)
      .map(|i| {
        pool.alloc();
        let kind = EnemyKind::from_seed(i as u32);
        Enemy {
          pos: scatter(i as u32),
          target: (i % player_count.max(1)) as PlayerId,
          kind,
          health: kind.max_health(),
        }
      })
      .collect::<Vec<Enemy>>();
    let announced_target = enemies.iter().map(|e| e.target).collect();

    Self {
      players,
      pool,
      enemies,
      projectiles: Vec::new(),
      // A second of truth at the tick rate, which comfortably covers the
      // deepest render delay the panel offers.
      history: HistoricalStateBuffer::new(TRUTH_HISTORY),
      grid: SpatialGrid::new(GridQuantizer::new((0.0, 0.0), CELL_SIZE)),
      cur_vis: (0..player_count).map(|_| VisibilitySet::with_capacity(enemy_count as u32)).collect(),
      squads: squads_of(player_count),
      squad_added: vec![0; player_count],
      squad_of: vec![Vec::new(); player_count],
      announced_target,
      clock_ms: 0,
      input_schedules: (0..player_count).map(|_| InputSchedule::new()).collect(),
      held: vec![None; player_count],
      naive_inputs: 0,
      sim: FixedTimestep::from_step_ms(SIM_STEP_MS),
      sync: Periodic::new(1),
      player_sync: Periodic::new(1),
      pending_players: None,
      retarget: Periodic::new(RETARGET_INTERVAL_MS),
      wave: Periodic::new(WAVE_INTERVAL_MS),
      fire: Periodic::new(FIRE_INTERVAL_MS),
      nova: Periodic::new(NOVA_INTERVAL_MS),
      target_population: enemy_count,
      kills: 0,
      nova_kills_last: 0,
      last_nova_ms: None,
      candidates: Vec::new(),
      entered_buf: Vec::new(),
      last_spawns: 0,
      far_tier_countdown: 1,
      recently_dead: Vec::new(),
      relevant_players: vec![Vec::new(); player_count],
      wallets_dirty: (0..player_count as PlayerId).collect(),
      shots_fired_since_send: Vec::new(),
      shots_ended_since_send: Vec::new(),
      next_shot_id: 0,
      next_seq: 0,
      baselines: (0..player_count)
        .map(|_| DeltaBaseline::new(SENT_HISTORY).with_flow(STALLED_AFTER_MS, STALL_KEEPALIVE_MS))
        .collect(),
      coins: Vec::new(),
      next_coin_id: 0,
      wallets: vec![Wallet::default(); player_count],
      coins_claimed: vec![0; player_count],
      coins_expired: 0,
      claims_since_send: Vec::new(),
      denied_purchases: 0,
      denials_since_send: vec![Vec::new(); player_count],
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
    self.pool.index_space()
  }

  pub fn alive_count(&self) -> usize {
    self.pool.len()
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
    self.pool.iter().map(|key| (key.into(), &self.enemies[key.index as usize]))
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
    // The clock tracks *simulated* time, not wall time. Normally they are the
    // same thing, and they diverge exactly when the step cap refuses to catch up
    // on a long stall. A packet's `server_time_ms` says when its state is from,
    // and a client subtracts it from its own clock to project a sample forward,
    // so a clock that ran ahead of the state it describes would have every client
    // projecting the horde into a future the server never simulated.
    for step in self.sim.advance(dt_ms) {
      self.clock_ms += step.as_millis() as u64;
      // Per step, not per packet: an input is due at a *tick*, so consuming the
      // queue once per network frame would collapse everything that arrived
      // between two ticks onto whichever one happened to run next.
      self.execute_due_inputs();
      let occupants = self.occupants(seats);
      self.step(&occupants, controls);
    }

    if self.retarget.due(dt_ms) {
      self.retarget();
    }

    if controls.combat {
      for _ in 0..self.wave.advance(dt_ms) {
        self.spawn_wave();
      }
      for _ in 0..self.fire.advance(dt_ms) {
        self.fire_weapons();
      }
      for _ in 0..self.nova.advance(dt_ms) {
        self.nova();
      }
    }

    // The player stream, on its own clock and built per recipient, because it
    // carries a different subset to each one.
    self.player_sync.set_interval_ms(controls.player_sync_interval_ms());
    if self.player_sync.due(dt_ms) {
      // The far tier rides a slower clock than the near one. A distant peer is a
      // couple of pixels on a map, so a low rate is invisible there and is the
      // larger of the two savings: precision halves the bytes per sample, rate
      // removes whole samples.
      self.far_tier_countdown = self.far_tier_countdown.saturating_sub(1);
      let far_due = self.far_tier_countdown == 0;
      if far_due {
        self.far_tier_countdown = FAR_TIER_EVERY;
      }
      // Stalled seats get no player frames at all: unlike the entity stream
      // there is no baseline to probe, and the resumed client rebuilds its peer
      // views from the first frames after its acknowledgements return.
      self.pending_players = Some(
        (0..self.players.len())
          .filter(|&c| !self.seat_stalled(c))
          .map(|c| (c as PlayerId, self.build_player_frame(c, far_due, controls)))
          .collect(),
      );
    }

    self.sync.set_interval_ms(controls.sync_interval_ms());
    if self.sync.due(dt_ms) {
      return self.build_packets(controls);
    }
    Vec::new()
  }

  /// Truth, for anything that needs to ask where things were.
  pub fn history(&self) -> &HistoricalStateBuffer<Handle, Vec2, u64> {
    &self.history
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
      step_player(pos, Vec2::new(dx, dy), SIM_DT);
    }
    let speed_scale = enemy_speed_scale(self.clock_ms);
    for i in 0..self.enemies.len() {
      if !self.pool.is_occupied(i as u32) {
        continue;
      }
      let enemy = &mut self.enemies[i];
      let t_idx = enemy.target as usize % self.players.len();
      let repel = self.wallets[t_idx].has(Upgrade::Repulsor).then(|| repulsor_pulse(self.clock_ms)).flatten();
      step_enemy(enemy, self.players[t_idx], repel, speed_scale, SIM_DT);
    }
    self.resolve_contact_damage();
    if controls.combat {
      self.step_projectiles();
    }
    if controls.coins {
      self.step_coins();
    }
    self.record_truth();
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
      let touched = self
        .pool
        .iter()
        .map(|key| &self.enemies[key.index as usize])
        .any(|e| e.pos.dist(eye) <= PLAYER_CONTACT_RADIUS + e.kind.radius());
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

  /// Records where every enemy ended this tick.
  ///
  /// After everything has moved, so a lookup for a tick finds the world as that
  /// tick left it, which is the same instant a client drawing at that tick was
  /// shown.
  fn record_truth(&mut self) {
    let now = self.clock_ms;
    let snapshot: Vec<(Handle, Vec2)> = self.pool.iter().map(|key| (key.into(), self.enemies[key.index as usize].pos)).collect();
    for (handle, pos) in snapshot {
      self.history.record_state(handle, now, pos);
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
      self.wallets_dirty.insert(p as PlayerId);
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
    self.wallets_dirty.insert(player as PlayerId);
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
      for key in self.pool.iter() {
        let enemy = &self.enemies[key.index as usize];
        if enemy.pos.dist(proj.pos) <= HIT_RADIUS + enemy.kind.radius() {
          hits.push((pi, key.index as EntityIndex));
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
      self.hits_since_send.push((self.enemies[target as usize].pos, 1));
      self.damage(target, 1);
    }
    spent.sort_unstable();
    spent.dedup();
    for pi in spent.into_iter().rev() {
      if pi < self.projectiles.len() {
        // An early end, and the one thing about a shot a client cannot derive:
        // it holds the fire time and the lifetime, so ordinary expiry needs no
        // message, but a hit does or the shot flies on through what it killed.
        let ended = &self.projectiles[pi];
        self.shots_ended_since_send.push((ended.id, ended.origin));
        self.projectiles.remove(pi);
      }
    }
  }

  /// The area-of-effect pulse: kills everything close to each player at once,
  /// which is the mass-despawn burst worth measuring.
  fn nova(&mut self) {
    let mut caught: Vec<EntityIndex> = Vec::new();
    for player in self.players.clone() {
      for key in self.pool.iter() {
        if self.enemies[key.index as usize].pos.dist(player) <= NOVA_RADIUS {
          caught.push(key.index as EntityIndex);
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

  /// Applies damage, and kills only when the health actually reaches zero.
  ///
  /// A death needs no separate announcement list. The delta stream diffs in a key
  /// space carrying the generation, so a slot that died reads as a retraction of
  /// the occupant the client was told about, and a slot reused in the same
  /// interval reads as despawn-then-spawn on its own.
  fn damage(&mut self, idx: EntityIndex, amount: u8) {
    if !self.pool.is_occupied(idx as u32) {
      return;
    }
    let enemy = &mut self.enemies[idx as usize];
    enemy.health = enemy.health.saturating_sub(amount);
    if enemy.health == 0 {
      self.kill(idx);
    }
  }

  fn kill(&mut self, idx: EntityIndex) {
    // The pool bumps the generation on free, so any handle naming this occupant
    // stops matching the moment it dies rather than when something takes the
    // slot. A stale free is refused, which is why this reads the result.
    let Some(key) = self.pool.key(idx as u32) else {
      return;
    };
    let died_at = self.enemies[idx as usize].pos;
    self.pool.free(key);
    self.kills += 1;
    // Logged with the time, so a client drawing the past can be asked whether
    // this was dead *at the instant it is drawing* rather than whether it is
    // dead now.
    self.recently_dead.push((key.into(), self.clock_ms));
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
      for key in self.pool.iter() {
        let enemy = &self.enemies[key.index as usize];
        let d = enemy.pos.dist(player);
        if d < VIEW_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
          best = Some((d, enemy.pos));
        }
      }
      if let Some((d, target)) = best
        && d > 1.0
      {
        let (dx, dy) = (target.x - player.x, target.y - player.y);
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let vel = Vec2::new(dx / len * PROJECTILE_SPEED, dy / len * PROJECTILE_SPEED);
        let id = self.next_shot_id;
        self.next_shot_id = self.next_shot_id.wrapping_add(1);
        self.projectiles.push(Projectile {
          id,
          pos: player,
          origin: player,
          vel,
          ttl: PROJECTILE_TTL,
        });
        // The event, which is all a client is told. It carries the rule's
        // inputs, so a client places the shot at any instant exactly instead of
        // being handed a position it could have computed.
        self.shots_fired_since_send.push(Shot {
          id,
          origin: player,
          vel,
          fired_ms: self.clock_ms,
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
    // The per-wave cap exists so a deficit is filled over a few waves rather
    // than in one spike. It has to scale with the player count, because the
    // *kill* rate does: every player carries an auto-firing weapon and a nova
    // that clears a radius every 4.5 s. Fixed at 40 it let 128 players
    // annihilate a 3000-strong horde and hold it near 40 alive, which reads as
    // an empty arena and quietly turns every entity measurement into a
    // measurement of the overheads instead.
    let cap = 40 * self.players.len().div_ceil(crate::sim::types::DEFAULT_PLAYERS);
    let want = (self.target_population - alive).min(cap);
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

      // The pool reuses a freed index when it has one, so the id space stays
      // dense and settles at the high-water mark of simultaneously live enemies.
      let key = self.pool.alloc();
      let idx = key.index as usize;
      if idx < self.enemies.len() {
        self.enemies[idx] = enemy;
        self.announced_target[idx] = enemy.target;
      } else {
        self.enemies.push(enemy);
        self.announced_target.push(enemy.target);
      }
    }
  }

  fn retarget(&mut self) {
    for i in 0..self.enemies.len() {
      if !self.pool.is_occupied(i as u32) {
        continue;
      }
      let enemy = &mut self.enemies[i];
      let mut best = enemy.target;
      let mut best_d = f32::MAX;
      for (p, pos) in self.players.iter().enumerate() {
        let d = enemy.pos.dist(*pos);
        if d < best_d {
          best_d = d;
          best = p as PlayerId;
        }
      }
      enemy.target = best;
    }
  }

  /// Folds in a client's acknowledgement, moving its baseline forward.
  ///
  /// `digest` is the client's own view of its mirror. The block compares it to
  /// the state it believes the client reached and forces a clean rebuild when
  /// they disagree, which is the only cure for a mirror that has drifted: a
  /// drifted entity stays in view, is only ever sampled, and a sample for an
  /// entity you do not hold is discarded.
  pub fn receive_ack(&mut self, player: usize, newest: u64, mask: u64, digest: u64) {
    self.baselines[player].observe_ack_at(newest, mask, digest, self.clock_ms);
  }

  /// Resets one seat's relevance baseline so the next packet to it is a full
  /// dump rather than a delta.
  ///
  /// Called when a fresh client takes the seat. Every seat gets packets built for
  /// it from startup, occupied or not (an empty seat drifts as a bot), so by the
  /// time a real client connects the seat's baseline is already most of the
  /// world. Without this the joiner's first frame is a diff against a state it
  /// never held, and the visible world arrives only as the slow trickle of
  /// whatever happens to become newly relevant.
  pub fn reset_seat(&mut self, seat: usize) {
    // Also restarts the flow-control grace period: the joiner has acknowledged
    // nothing yet, and must not start life throttled for it.
    self.baselines[seat].reset();
  }

  /// Whether a seat has stopped acknowledging: see [`STALLED_AFTER_MS`].
  pub fn seat_stalled(&self, seat: usize) -> bool {
    self.baselines[seat].stalled(self.clock_ms)
  }

  /// How many seats are currently throttled for silence. On a healthy arena
  /// this is zero: bots are acknowledged by the arena and humans acknowledge
  /// every applied frame, so a nonzero reading names a client that stopped
  /// reading (a hidden tab, a stalled machine).
  pub fn stalled_seats(&self) -> usize {
    (0..self.players.len()).filter(|&p| self.seat_stalled(p)).count()
  }

  /// How often a client's baseline had to be rebuilt from nothing. The cost of
  /// recovery, and the number that says whether the history window is long
  /// enough for the loss and latency actually being seen.
  pub fn full_resends(&self) -> u64 {
    self.baselines.iter().map(|b| b.full_rebuilds()).sum()
  }

  /// Packets sent to each client whose fate is still unknown.
  pub fn unacked(&self, player: usize) -> usize {
    self.baselines[player].unacked()
  }

  /// Who is driving each seat this step: whatever the buffer has come due for,
  /// falling back to what the caller supplied (a bot, or an unoccupied seat).
  fn occupants(&self, seats: &[Seat]) -> Vec<Seat> {
    (0..self.players.len())
      .map(|p| match self.held.get(p).copied().flatten() {
        Some(dir) => Seat::Steered(dir),
        None => seats.get(p).copied().unwrap_or_default(),
      })
      .collect()
  }

  /// Clears a seat's buffered and held input, for somebody leaving it.
  pub fn clear_input(&mut self, seat: usize) {
    if let Some(schedule) = self.input_schedules.get_mut(seat) {
      schedule.clear();
    }
    if let Some(held) = self.held.get_mut(seat) {
      *held = None;
    }
  }

  /// Offers an input naming a tick. The server owns time: the accepting
  /// window, the reject-not-correct rule (the lag-switch defence) and the
  /// clamp against reordering are all [`InputSchedule`]'s; see its docs.
  pub fn submit_input(&mut self, seat: usize, tick: u64, dir: Vec2, controls: &Controls) -> bool {
    if seat >= self.input_schedules.len() {
      return false;
    }
    if !controls.input_playout {
      // The naive path, kept so the difference can be measured: whatever arrives
      // takes effect on the next tick.
      self.held[seat] = Some(dir);
      self.naive_inputs += 1;
      return true;
    }
    let window = InputWindow {
      max_late: controls.input_max_late_ticks,
      max_early: controls.input_max_early_ticks,
    };
    let current = self.tick();
    self.input_schedules[seat].submit(tick, dir, current, window).accepted()
  }

  /// Inputs that arrived after the tick they were scheduled for.
  pub fn late_inputs(&self) -> u64 {
    self.input_schedules.iter().map(|s| s.late()).sum()
  }

  /// Inputs the server accepted and scheduled. The denominator every other input
  /// count needs: rejections mean nothing without knowing how many arrived.
  pub fn accepted_inputs(&self) -> u64 {
    self.naive_inputs + self.input_schedules.iter().map(|s| s.accepted()).sum::<u64>()
  }

  /// Inputs named for a tick the server was not accepting, and dropped.
  pub fn rejected_inputs(&self) -> u64 {
    self.input_schedules.iter().map(|s| s.rejected()).sum()
  }

  /// Per-seat admission verdicts:
  /// `(accepted, late, rejected_closed, rejected_ahead, last margin in ticks)`.
  pub fn input_verdicts(&self) -> Vec<(u64, u64, u64, u64, Option<i64>)> {
    self
      .input_schedules
      .iter()
      .map(|s| {
        let (closed, ahead) = s.rejected_split();
        (s.accepted(), s.late(), closed, ahead, s.last_reject_margin())
      })
      .collect()
  }

  /// The tick the server is currently simulating. What a client aims at.
  ///
  /// **Derived from the clock, never counted alongside it.** A separate counter
  /// has to be kept in step with `clock_ms` through every path that touches
  /// either, and rebuilding the world is such a path: it preserves the clock so
  /// a client's packet-age estimate does not jump, and it reset the counter to
  /// zero. The clock then said thirty seconds and the tick said nought, so every
  /// input a client aimed was hundreds of ticks past the accepting window and was
  /// refused, permanently. The player simply stopped responding after a reset.
  pub fn tick(&self) -> u64 {
    self.clock_ms / (SIM_DT * 1000.0) as u64
  }

  /// Applies every input whose tick has arrived: each seat's newest due
  /// direction becomes its held one. Ordering and supersession are
  /// [`InputSchedule::execute_due`]'s.
  fn execute_due_inputs(&mut self) {
    let now = self.tick();
    for (seat, schedule) in self.input_schedules.iter_mut().enumerate() {
      if let Some(dir) = schedule.execute_due(now) {
        self.held[seat] = Some(dir);
      }
    }
  }

  /// Takes this tick's player frames, one per recipient, if the stream was due.
  ///
  /// **Per recipient, not one broadcast.** Everyone used to get the same frame
  /// listing every player, on the reasoning that players are few. That holds at
  /// four and fails at scale: it is `O(players^2)`, and measured at 128 it was
  /// the largest single line in the whole bandwidth budget. Each recipient now
  /// gets the players it can see or is being hunted on behalf of.
  pub fn take_player_frames(&mut self) -> Option<Vec<(PlayerId, PlayerFrame)>> {
    self.pending_players.take()
  }

  /// Handles that died recently, with when. Bounded to the deepest render delay
  /// the panel allows, which is exactly how far back any client can be drawing.
  ///
  /// For asking "was this dead *at the instant being drawn*" rather than "is it
  /// dead now". A client that renders in the past is holding entities the server
  /// has since killed, by construction, and a check against the present charges
  /// it for the delay instead of finding a fault.
  pub fn died_after(&self, handle: Handle, at_ms: u64) -> bool {
    self.recently_dead.iter().any(|(h, t)| *h == handle && *t > at_ms)
  }

  /// The death log itself, for an observer that has to make the same judgement.
  pub fn recently_dead_log(&self) -> Vec<(Handle, u64)> {
    self.recently_dead.clone()
  }

  /// Coins currently on the ground.
  pub fn coin_count(&self) -> usize {
    self.coins.len()
  }

  /// Entities announced as new in the most recent send round, across all seats.
  /// A delta stream in steady state should keep this near the real churn; a
  /// number close to the whole visible set means somebody's baseline is not
  /// advancing.
  pub fn last_spawn_count(&self) -> usize {
    self.last_spawns
  }

  /// How many players client `c` is currently being sent.
  ///
  /// The per-round set, which is what the bandwidth depends on. A client's
  /// *cumulative* knowledge is much larger and says nothing about cost: a
  /// freshly spawned enemy briefly chases whoever the wave assigned it, so over
  /// a minute almost every player passes through almost every client's set once.
  pub fn relevant_player_count(&self, c: usize) -> usize {
    self.relevant_players[c].len()
  }

  /// Puts a player somewhere, for tests that need a known arrangement.
  #[cfg(test)]
  pub fn place_player_for_test(&mut self, p: usize, at: Vec2) {
    self.players[p] = at;
  }

  /// Which player an enemy is chasing, if this handle still names a live one.
  /// For checking that a client was told where its enemies' targets are.
  pub fn enemy_target(&self, handle: Handle) -> Option<PlayerId> {
    let key = SlotKey::from(handle);
    self.pool.is_live(key).then(|| self.enemies[key.index as usize].target)
  }

  /// Which players client `c` actually needs.
  ///
  /// Two reasons a player is needed, and the second is the one that is easy to
  /// miss: it can be **seen**, or it is being **chased** by an enemy this client
  /// holds. `step_enemy` aims at a player, so a client that cannot place an
  /// enemy's target runs a different rule than the server and the horde drifts.
  ///
  /// Recomputed on each entity round, when the visible set is already in hand,
  /// and reused by the player stream on its own clock. At most one entity
  /// interval stale, which costs nothing: a player who has just come into view
  /// is one interval late rather than absent.
  fn recompute_relevant_players(&mut self, c: usize, controls: &Controls) {
    let eye = self.players[c];
    let mut near: Vec<PlayerId> = Vec::new();
    // Yourself, always: your own marker and health are not optional.
    near.push(c as PlayerId);
    for (p, pos) in self.players.iter().enumerate() {
      if p == c {
        continue;
      }
      // The threshold the renderer draws a peer at, so the wire carries exactly
      // what the screen can use. The previous set is the hysteresis memory, so
      // this costs no extra state.
      let was_near = self.relevant_players[c].contains(&(p as PlayerId));
      if NEAR_TIER.admits(was_near, pos.dist(eye)) {
        near.push(p as PlayerId);
      }
    }
    for idx in self.cur_vis[c].iter() {
      near.push(self.enemies[idx as usize].target);
    }
    near.sort_unstable();
    near.dedup();

    // The union of the two channels, and the count the second one actually
    // costs: `added` is the squadmates distance missed, and nothing at all for
    // the ones standing beside you.
    let empty = Subscriptions::new(SQUAD_SIZE);
    let chosen = if controls.squads { &self.squads } else { &empty };
    let audience = Audience::of(&near, chosen, &(c as PlayerId));
    self.squad_added[c] = audience.added;

    let out = &mut self.relevant_players[c];
    out.clear();
    out.extend(audience.entries.iter().map(|(p, _)| *p));
    self.squad_of[c].clear();
    self.squad_of[c].extend(chosen.of(&(c as PlayerId)).copied());
  }

  /// One recipient's player frame: the near tier every time, and the far tier
  /// only on the frames it is due.
  fn build_player_frame(&self, c: usize, far_due: bool, controls: &Controls) -> PlayerFrame {
    let relevant = &self.relevant_players[c];
    let distant = if far_due && controls.far_tier {
      (0..self.players.len())
        .map(|p| p as PlayerId)
        .filter(|p| !relevant.contains(p))
        .map(|p| {
          let (x, y) = quantize_far(self.players[p as usize]);
          (p, x, y)
        })
        .collect()
    } else {
      Vec::new()
    };
    PlayerFrame {
      server_time_ms: self.clock_ms,
      players: relevant.iter().map(|&p| (p, self.players[p as usize])).collect(),
      vitals: relevant
        .iter()
        .map(|&p| (p, self.player_health(p as usize), self.clock_ms < self.player_shield_until_ms[p as usize]))
        .collect(),
      // Which of them are there because this client chose them rather than
      // because it can see them. The same distinction gow_3d puts on its wire,
      // and for the same reason: absence means "walked away" for one and "left
      // the arena" for the other, so a client that cannot tell them apart
      // fades a squadmate off its map the moment they round a corner.
      squad: self.squad_of[c].clone(),
      distant,
    }
  }

  /// How many players this client is told about only because it subscribed to
  /// them, and how many the far tier is carrying. The two halves of the trade,
  /// for the panel.
  pub fn squad_cost(&self, c: usize) -> (usize, usize) {
    (self.squad_added[c], self.players.len().saturating_sub(self.relevant_players[c].len()))
  }

  fn build_packets(&mut self, controls: &Controls) -> Vec<(PlayerId, Packet)> {
    if controls.relevance {
      self.grid.clear();
      for key in self.pool.iter() {
        let pos = self.enemies[key.index as usize].pos;
        self.grid.insert(key.index as EntityIndex, pos.x, pos.y);
      }
    }

    // Positions, health and shields are the player stream's business, not this
    // one's. Carried on both streams, every packet holds every player twice
    // over: the single largest line in the budget at a large count, and none of
    // it anything the other stream is not already saying.
    let mut out = Vec::with_capacity(self.players.len());

    let seq = self.next_seq;
    self.next_seq += 1;

    // Round-robin over the visible set: each enemy is corrected at
    // `sample_hz`, not per packet, by taking one rotating slice of it per
    // round. Safe because a sample is a correction to a rule every client runs
    // locally, and because everything that is not a correction (spawns,
    // departures, target changes) still goes the round it happened.
    let sample_phases = (controls.sync_hz / controls.sample_hz.clamp(1, controls.sync_hz.max(1))).max(1) as u64;

    // One tree over the whole live population, walked once per player. The build
    // is the expensive half and it is viewer independent, which is what makes
    // this affordable for a crowd this size.
    let crowd_tree = (controls.crowd_lod_theta > 0.0).then(|| {
      let points: Vec<WeightedPoint> = self
        .pool
        .iter()
        // Weight one per enemy: the quantity being summarised is a headcount, so
        // a summary's weight *is* how many are standing there.
        .map(|key| {
          let pos = self.enemies[key.index as usize].pos;
          WeightedPoint::new(pos.x, pos.y, 1.0)
        })
        .collect();
      AggregateTree::build_in(&points, (ARENA_W * 0.5, ARENA_H * 0.5), ARENA_W.max(ARENA_H), 10)
    });
    let mut summaries: Vec<plaza_server_utils::aggregate::Summary> = Vec::new();

    for p in 0..self.players.len() {
      // Flow control: the block decides whether this seat is read at all. What
      // a skipped seat misses forever is only the per-round events (shots,
      // hits): cosmetic by construction, and describing moments the stalled
      // client will never render.
      if !self.baselines[p].should_send(self.clock_ms) {
        continue;
      }
      let eye = self.players[p];

      // What this client should hold after applying, in the key space the digest
      // uses: index *and* generation, so "which occupant" is part of the answer.
      // The block requires exactly that: its key must be the key the digest
      // hashes, or the drift check compares two unrelated numbers.
      self.cur_vis[p].clear();
      if controls.relevance {
        self.candidates.clear();
        self.grid.query_radius(eye.x, eye.y, VIEW_RADIUS, &mut self.candidates);
        for &idx in &self.candidates {
          if self.enemies[idx as usize].pos.dist(eye) <= VIEW_RADIUS {
            self.cur_vis[p].insert(idx);
          }
        }
      } else {
        for key in self.pool.iter() {
          self.cur_vis[p].insert(key.index as EntityIndex);
        }
      }
      // Which players this client needs, from the visible set just computed.
      // Both streams read it: the entity packet for wallets, and the player
      // stream on its own clock.
      self.recompute_relevant_players(p, controls);

      // The pool's own key, not one packed here: the digest, the delta baseline
      // and the client's mirror all key on `SlotKey::encode`, and a second
      // packing that agrees today is a disagreement waiting to happen.
      let cur_keys: BTreeSet<u64> = self
        .cur_vis[p]
        .iter()
        .filter_map(|idx| self.pool.key(idx).map(|key| key.encode()))
        .collect();

      // The whole reliability decision, in one call: what to send, what to
      // retract, whether this has to be a clean rebuild, and what baseline it is
      // all measured against.
      self.baselines[p].set_policy(if controls.ack_recovery { RecoveryPolicy::AckRecovery } else { RecoveryPolicy::Naive });
      let plan = self.baselines[p].plan(&cur_keys, seq);

      let mut packet = Packet {
        server_time_ms: self.clock_ms,
        seq,
        baseline_seq: plan.baseline_seq,
        full_baseline: plan.full_baseline,
        nova_at_ms: self.last_nova_ms,
        ..Default::default()
      };

      // Keys to payloads. This is the half that is actually this game's: the
      // block decided *which* entities, and only the application knows what an
      // entity looks like on the wire.
      self.entered_buf.clear();
      for key in &plan.entered {
        let slot = SlotKey::decode(*key);
        let enemy = &self.enemies[slot.index as usize];
        packet.entered.push(Spawn {
          handle: slot.into(),
          pos: enemy.pos,
          target: enemy.target,
          kind: enemy.kind,
        });
        self.entered_buf.push(slot.index as EntityIndex);
      }
      self.entered_buf.sort_unstable();

      for key in &plan.left {
        let slot = SlotKey::decode(*key);
        // Dead if the slot has moved on, gone out of view if it has not. Naming
        // the generation from the *key* rather than from the slot is the whole
        // point: a refilled slot must not have its new occupant retracted in
        // place of the corpse the client is actually holding.
        //
        // Deaths need no separate out-of-band announcement now. Diffing in a key
        // space that carries the generation means a slot that died and was
        // refilled reads as despawn-then-spawn on its own, which is what the
        // explicit death list used to be for back when the diff was index-only.
        let reason = if self.pool.is_live(slot) { LeaveReason::OutOfRange } else { LeaveReason::Died };
        packet.left.push((slot.into(), reason));
      }

      for idx in self.cur_vis[p].iter() {
        if self.entered_buf.binary_search(&idx).is_ok() {
          continue;
        }
        let Some(key) = self.pool.key(idx) else {
          continue;
        };
        let enemy = &self.enemies[idx as usize];
        let target = (enemy.target != self.announced_target[idx as usize]).then_some(enemy.target);
        // A target change rides immediately whatever the rotation says:
        // `announced_target` is cleared globally after this round, so a
        // deferred one would not just arrive late, it would never arrive.
        if target.is_none() && (idx as u64 + seq) % sample_phases != 0 {
          continue;
        }
        packet.samples.push(Sample {
          handle: key.into(),
          pos: enemy.pos,
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

      // Coins near enough to race for: a coin you cannot see is one you cannot
      // contest, so partial knowledge would cost more in confusion than the
      // bytes save.
      if controls.coins {
        packet.coins = self.coins.iter().filter(|c| c.pos.dist(eye) <= VIEW_RADIUS * 1.2).copied().collect();
        // Wallets for the players this client needs, and only the ones that
        // actually changed. A rebuild carries the lot, because a client that
        // has just dropped its world has no wallet to update either.
        packet.wallets = self.relevant_players[p]
          .iter()
          .filter(|id| plan.full_baseline || self.wallets_dirty.contains(id))
          .map(|&id| (id, self.wallets[id as usize].clone()))
          .collect();
        packet.claims = self.claims_since_send.clone();
        packet.denied_buys = std::mem::take(&mut self.denials_since_send[p]);
      }

      // Shots that *started* near this player, and the ones that ended early.
      // Events, not the live set: the flight itself is an equation both sides
      // solve, so re-sending a position for it every packet pays for arithmetic
      // twice and still cannot be evaluated between packets.
      packet.shots_fired = self
        .shots_fired_since_send
        .iter()
        .filter(|shot| shot.origin.dist(eye) <= VIEW_RADIUS * 1.2)
        .copied()
        .collect();
      packet.shots_ended = self
        .shots_ended_since_send
        .iter()
        .filter(|(_, origin)| origin.dist(eye) <= VIEW_RADIUS * 1.2)
        .map(|(id, _)| *id)
        .collect();
      packet.hits = self.hits_since_send.iter().filter(|(pos, _)| pos.dist(eye) <= VIEW_RADIUS * 1.2).copied().collect();

      // Summarise what the client must hold once it applies this packet. Keyed by
      // index *and* generation, so agreeing on membership is not enough: both
      // sides have to agree on which occupant of each slot it is.
      packet.visible_digest = SetDigest::from_keys(cur_keys.iter().copied()).digest();

      // The same keys spelled out, for a client to diff against on a mismatch.
      // Off the normal wire; a diagnostic paid for only while it is on.
      if controls.debug_digest {
        packet.debug_keys = cur_keys.iter().copied().collect();
      }

      out.push((p as PlayerId, packet));
    }

    for (i, enemy) in self.enemies.iter().enumerate() {
      self.announced_target[i] = enemy.target;
    }
    self.claims_since_send.clear();
    self.hits_since_send.clear();
    // These are per-send-round events, so they are consumed by the round that
    // carried them. A client that misses the packet misses the shot, which is
    // the trade an event makes against a re-sent set, and it costs one sprite.
    self.shots_fired_since_send.clear();
    self.shots_ended_since_send.clear();
    self.wallets_dirty.clear();
    self.last_spawns = out.iter().map(|(_, p)| p.entered.len()).sum();
    // Bounded by the deepest render delay: past that, no client can still be
    // drawing the moment this entity was alive.
    let keep_after = self.clock_ms.saturating_sub(crate::sim::types::RENDER_DELAY_MAX_MS);
    self.recently_dead.retain(|(_, t)| *t >= keep_after);
    out
  }
}

fn scatter(i: u32) -> Vec2 {
  let x = ((i.wrapping_mul(2_654_435_761) >> 8) % 10_000) as f32 / 10_000.0 * ARENA_W;
  let y = ((i.wrapping_mul(40_503) >> 4) % 10_000) as f32 / 10_000.0 * ARENA_H;
  Vec2::new(x, y)
}

/// Where player `p` of `count` starts.
///
/// Both layouts are **sized from the count**, which the fixed 2x2 and the single
/// row they replaced were not: past four players the grid's third row sat at
/// `1.25 * ARENA_H`, outside the world, and the cluster's row grew longer than a
/// view radius so the players it exists to gather could not see each other. At
/// four both formulations agree exactly, so the arena everything here was
/// measured in is unchanged.
fn player_start(p: usize, count: usize, spread: bool) -> Vec2 {
  if spread {
    // Cells of a grid just big enough for the count, each player at its centre.
    let cols = (count as f32).sqrt().ceil().max(1.0);
    let rows = (count as f32 / cols).ceil().max(1.0);
    let (fx, fy) = ((p % cols as usize) as f32, (p / cols as usize) as f32);
    Vec2::new(ARENA_W * (fx + 0.5) / cols, ARENA_H * (fy + 0.5) / rows)
  } else {
    // One knot in the middle, so the horde converges on a single place. A grid
    // rather than a row, and one whose spacing tightens once the default would
    // spread it further than anybody can see: a cluster whose members are not
    // in each other's view is not a cluster, and the setting stops meaning
    // anything. A row of four at the usual spacing spans well inside a view, so
    // the small counts keep the exact layout they always had.
    const ROW: usize = 4;
    const SPACING: f32 = 40.0;
    let cols = ROW.max((count as f32).sqrt().ceil() as usize);
    let rows = count.div_ceil(cols);
    // Player 0 sits at a corner, so the span to cover is the grid's diagonal.
    let diagonal = ((((cols - 1) * (cols - 1)) + ((rows - 1) * (rows - 1))) as f32).sqrt().max(1.0);
    let spacing = SPACING.min(VIEW_RADIUS * 0.9 / diagonal);
    let (fx, fy) = ((p % cols) as f32, (p / cols) as f32);
    Vec2::new(ARENA_W * 0.5 + fx * spacing, ARENA_H * 0.5 + fy * spacing)
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

  /// A server where player zero stands alone and everyone else is across the
  /// arena, so nothing reaches player zero by distance.
  ///
  /// One corner rather than a scatter: an arena this size holds only a handful
  /// of positions further apart than the near tier, so spreading 128 players
  /// out puts them back inside each other's radius and measures nothing.
  fn player_zero_alone(players: usize) -> (Server, Controls) {
    let controls = Controls {
      player_count: players,
      ..Controls::default()
    };
    // No enemies: an enemy chasing somebody is a third reason a player is
    // relevant, and this is measuring the other two.
    let mut sim = Server::new(0, players, true);
    sim.place_player_for_test(0, Vec2::new(ARENA_W * 0.04, ARENA_H * 0.04));
    for p in 1..players {
      sim.place_player_for_test(p, Vec2::new(ARENA_W * 0.96, ARENA_H * 0.96));
    }
    (sim, controls)
  }

  #[test]
  fn a_squadmate_is_told_about_across_the_arena_and_a_stranger_is_not() {
    // The second channel, and the reason a radius cannot express it: these
    // players are as far apart as the arena allows, and four of them are still
    // in each other's frames because they chose to be.
    let (mut sim, mut controls) = player_zero_alone(16);

    controls.squads = true;
    sim.recompute_relevant_players(0, &controls);
    let with = sim.relevant_players[0].clone();
    assert!(
      with.len() > 1,
      "nobody but yourself, so the subscription never reached anyone"
    );
    for mate in 1..SQUAD_SIZE as PlayerId {
      assert!(with.contains(&mate), "squadmate {mate} was not in the frame");
    }
    assert!(
      !with.contains(&(SQUAD_SIZE as PlayerId)),
      "somebody outside the squad arrived by subscription: {with:?}"
    );
    assert_eq!(sim.squad_added[0], SQUAD_SIZE - 1, "the added count is what the channel costs");

    controls.squads = false;
    sim.recompute_relevant_players(0, &controls);
    let without = sim.relevant_players[0].clone();
    assert_eq!(without.len(), 1, "only yourself, once the channel is off: {without:?}");
  }

  #[test]
  fn what_the_second_channel_costs_against_the_far_tier() {
    // The trade this example did not have a way to state: a far tier is a
    // broadcast wearing relevance's clothes, and it costs every player on
    // every frame it is due. A subscription costs the handful you chose.
    println!("\n  one client's player frame, standing alone:\n");
    println!("{:>10} {:>14} {:>14} {:>12}", "players", "far tier B", "squad only B", "ratio");
    let mut costs = Vec::new();

    for players in [8usize, 32, 64, 128] {
      let (mut sim, mut controls) = player_zero_alone(players);

      controls.squads = false;
      controls.far_tier = true;
      sim.recompute_relevant_players(0, &controls);
      let far = sim.build_player_frame(0, true, &controls).bytes();

      controls.squads = true;
      controls.far_tier = false;
      sim.recompute_relevant_players(0, &controls);
      let squad = sim.build_player_frame(0, true, &controls).bytes();

      println!("{players:>10} {far:>14} {squad:>14} {:>11.1}x", far as f32 / squad as f32);
      costs.push((players, far, squad));
    }

    println!("\n  the far tier grows with the arena and the squad does not. Below the");
    println!("  crossover the broadcast is simply cheaper, which is worth stating:");
    println!("  a second channel is not free and does not pay at every size.\n");

    // The claim, at both ends. A small arena is cheaper to broadcast; a large
    // one is not, and the whole point is that only one of the two grows.
    let (_, small_far, small_squad) = costs[0];
    let (_, big_far, big_squad) = costs[costs.len() - 1];
    assert!(
      small_squad >= small_far,
      "the far tier should still win at eight players: {small_squad} against {small_far}"
    );
    assert!(
      big_squad * 3 < big_far,
      "at 128 players the squad channel cost {big_squad} against the far tier's {big_far}"
    );
    assert_eq!(
      big_squad, small_squad,
      "the subscription cost moved with the player count, so it is not a subscription"
    );
  }

  #[test]
  fn every_player_starts_inside_the_arena_however_many_there_are() {
    // The layout used to be a fixed 2x2 grid and a single row, both of which
    // were fine at four and wrong at anything else: the grid's third row sat at
    // 1.25 * ARENA_H, outside the world, so a fifth player spawned in the void
    // with the horde unable to reach it.
    for count in [1usize, 2, 4, 5, 7, 16, 64, crate::sim::types::MAX_PLAYERS] {
      for spread in [true, false] {
        for p in 0..count {
          let at = player_start(p, count, spread);
          assert!(
            at.x >= 0.0 && at.x <= ARENA_W && at.y >= 0.0 && at.y <= ARENA_H,
            "player {p} of {count} (spread={spread}) starts at {at:?}, outside the arena"
          );
        }
      }
    }
  }

  #[test]
  fn the_four_player_layout_is_exactly_what_it_always_was() {
    // The generalisation has to be a superset, not a replacement: every
    // measurement in the README was taken in the four-player arena, and a
    // layout change would quietly invalidate all of them.
    for spread in [true, false] {
      for p in 0..4 {
        let now = player_start(p, 4, spread);
        let before = if spread {
          Vec2::new(ARENA_W * (0.25 + 0.5 * (p % 2) as f32), ARENA_H * (0.25 + 0.5 * (p / 2) as f32))
        } else {
          Vec2::new(ARENA_W * 0.5 + p as f32 * 40.0, ARENA_H * 0.5)
        };
        assert_eq!((now.x, now.y), (before.x, before.y), "player {p} (spread={spread}) moved");
      }
    }
  }

  #[test]
  fn a_clustered_lobby_stays_inside_one_view_however_big_it_is() {
    // The point of clustering is that the players can see each other and the
    // horde converges on one place. A single row of 128 spanned 5000 px, which
    // is not a cluster, and the setting silently stopped meaning anything.
    let count = crate::sim::types::MAX_PLAYERS;
    let first = player_start(0, count, false);
    for p in 1..count {
      let at = player_start(p, count, false);
      assert!(at.dist(first) <= VIEW_RADIUS, "player {p} of {count} is {:.0} px away, outside a view radius", at.dist(first));
    }
  }

  /// Runs a server for `ms`, with nobody steering except the buffer.
  fn idle(server: &mut Server, ms: u64, controls: &Controls) {
    // Seats 0 and 1 stand still rather than drifting: an unoccupied seat takes
    // the scripted drift, which would move the two players under test apart for
    // reasons that have nothing to do with when their input executed.
    let mut seats = vec![Seat::Bot; server.players.len()];
    seats[0] = Seat::Steered(Vec2::default());
    seats[1] = Seat::Steered(Vec2::default());
    for _ in 0..(ms / 16) {
      server.advance_seats(16, &seats, controls);
    }
  }

  /// The tick an honest client aims at: the server's current one, plus the
  /// playout depth it advertised, which is the same arithmetic every client does.
  fn aimed_tick(server: &Server, controls: &Controls) -> u64 {
    server.tick() + controls.playout_delay_ms / (SIM_DT * 1000.0) as u64
  }

  #[test]
  fn a_seat_that_stops_acknowledging_is_throttled_to_a_keepalive() {
    // The hidden-tab pathology, cut off at its source. A silent seat's
    // baseline goes stale within the history window, after which every plan
    // for it is a full baseline: the whole visible set, sixteen times a
    // second, into a buffer the client must parse in its first frame back.
    // Throttled, a stalled seat costs about a packet a second instead.
    let controls = Controls::default();
    let mut server = Server::new(200, 2, false);
    let seats = vec![Seat::Bot; 2];

    let mut counts = [0usize; 2];
    let mut last_to_1 = None;
    let mut elapsed = 0u64;
    while elapsed < 12_000 {
      let packets = server.advance_seats(16, &seats, &controls);
      elapsed += 16;
      for (p, packet) in &packets {
        let seat = *p as usize;
        // Seat 0 acknowledges everything, like a live client. Seat 1 falls
        // silent after the first second, like a tab going to the background.
        if seat == 0 || elapsed <= 1_000 {
          server.receive_ack(seat, packet.seq, u64::MAX, packet.visible_digest);
        }
        if seat == 1 {
          last_to_1 = Some((packet.seq, packet.visible_digest));
        }
        if elapsed > 6_000 {
          counts[seat] += 1;
        }
      }
    }

    assert!(server.seat_stalled(1), "three silent seconds is stalled");
    assert!(!server.seat_stalled(0), "an acknowledging seat is not");
    assert!(counts[0] >= 80, "the live seat streams at full rate: {} packets in 6 s", counts[0]);
    assert!(
      (2..=8).contains(&counts[1]),
      "the silent seat gets about one keepalive a second: {} packets in 6 s",
      counts[1]
    );

    // The player stream skips a stalled seat outright: there is no baseline to
    // probe, and the resumed client rebuilds its peer views from first frames.
    let frames = server.take_player_frames().expect("the player stream was due within the last interval");
    assert!(frames.iter().all(|(p, _)| *p != 1), "no player frames for a stalled seat");

    // One acknowledgement ends the throttle: the keepalive is what makes the
    // stream discoverable again, and acknowledging it resumes full rate.
    let (seq, digest) = last_to_1.expect("the keepalive reached the silent seat");
    server.receive_ack(1, seq, u64::MAX, digest);
    assert!(!server.seat_stalled(1));
    let mut resumed = 0usize;
    for _ in 0..62 {
      for (p, packet) in server.advance_seats(16, &seats, &controls) {
        server.receive_ack(p as usize, packet.seq, u64::MAX, packet.visible_digest);
        if p == 1 {
          resumed += 1;
        }
      }
    }
    assert!(resumed >= 12, "an acknowledged seat is streamed to again: {resumed} packets in a second");
  }

  #[test]
  fn two_players_who_pressed_together_execute_together_whatever_their_ping() {
    // The point of the playout buffer, as a number.
    //
    // Both press at the same instant and therefore name the same tick. One is on
    // a fast link and one is not, so their packets arrive four ticks apart.
    // Applied on arrival the near player gets those four ticks of movement free,
    // and anything decided by who was where first is decided by ping. Named for a
    // tick, they execute on the same one and move together.
    fn spread(controls: &Controls) -> f32 {
      let mut server = Server::new(50, 4, false);
      idle(&mut server, 480, controls);

      let middle = Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
      server.players[0] = middle;
      server.players[1] = middle;
      let start = server.players.clone();
      let target = aimed_tick(&server, controls);

      // The near player's packet lands almost at once.
      server.submit_input(0, target, Vec2::new(1.0, 0.0), controls);
      idle(&mut server, 64, controls);
      // The far player's lands four ticks later, naming the same tick.
      server.submit_input(1, target, Vec2::new(1.0, 0.0), controls);
      idle(&mut server, 400, controls);

      (server.players[0].dist(start[0]) - server.players[1].dist(start[1])).abs()
    }

    let scheduled = spread(&Controls::default());
    let on_arrival = spread(&Controls { input_playout: false, ..Controls::default() });

    assert!(on_arrival > 8.0, "applying on arrival should hand the near player a head start: {on_arrival:.1} px");
    assert!(scheduled < 1.0, "the buffer should erase it, got {scheduled:.1} px against {on_arrival:.1} px");
  }

  #[test]
  fn a_tick_that_has_already_been_simulated_is_closed() {
    // The lag switch, and the reason this rejects rather than corrects. Buffer
    // your packets, dump them late, and you would be asking the server to reopen
    // ticks it has already run. Correcting the tick into the window would still
    // execute the input, so the lie costs nothing and the input still lands.
    // Dropping it means backdating costs you the input.
    let controls = Controls::default();
    let mut server = Server::new(50, 4, false);
    idle(&mut server, 480, &controls);

    let middle = Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5);
    server.players[0] = middle;
    server.players[1] = middle;
    let start = server.players.clone();

    // Seat 0 aims honestly. Seat 1 names a tick from five seconds ago.
    server.submit_input(0, aimed_tick(&server, &controls), Vec2::new(1.0, 0.0), &controls);
    let stale = server.tick().saturating_sub(300);
    assert!(!server.submit_input(1, stale, Vec2::new(1.0, 0.0), &controls), "a closed tick must be refused");
    assert_eq!(server.rejected_inputs(), 1);

    idle(&mut server, 400, &controls);
    assert!(server.players[0].dist(start[0]) > 10.0, "the honest input took effect");
    assert!(server.players[1].dist(start[1]) < 1.0, "the rejected one did nothing at all");
  }

  #[test]
  fn a_rebuilt_world_keeps_accepting_the_inputs_a_client_is_already_aiming() {
    // Regression, and it made the player simply stop responding.
    //
    // Changing the enemy count rebuilds the world, and the rebuild deliberately
    // preserves the clock so a client's packet-age estimate does not jump. When
    // the tick was a separate counter it was *not* preserved: the clock said
    // thirty seconds and the tick said nought, so every input a client aimed was
    // hundreds of ticks beyond the accepting window and was refused for good.
    let controls = Controls::default();
    let mut warm = Server::new(50, 4, false);
    idle(&mut warm, 30_000, &controls);
    let clock = warm.now_ms();
    assert!(warm.tick() > 1_000, "the warm server has a real tick: {}", warm.tick());

    // Exactly what `Arena::reconfigure` does.
    let mut rebuilt = Server::new(80, 4, false);
    rebuilt.set_clock(clock);

    assert_eq!(rebuilt.tick(), warm.tick(), "the rebuilt world kept its place in time");
    let aimed = aimed_tick(&rebuilt, &controls);
    assert!(
      rebuilt.submit_input(0, aimed, Vec2::new(1.0, 0.0), &controls),
      "a client aiming normally was refused after the rebuild"
    );
    assert_eq!(rebuilt.rejected_inputs(), 0);
  }

  #[test]
  fn a_tick_too_far_ahead_is_refused() {
    // Otherwise a client could park inputs minutes into the future.
    let controls = Controls::default();
    let mut server = Server::new(50, 4, false);
    idle(&mut server, 480, &controls);

    let far = server.tick() + controls.input_max_early_ticks + 1;
    assert!(!server.submit_input(0, far, Vec2::new(1.0, 0.0), &controls));
    assert_eq!(server.rejected_inputs(), 1);
    // And the edge of the window is still accepted.
    let edge = server.tick() + controls.input_max_early_ticks;
    assert!(server.submit_input(0, edge, Vec2::new(1.0, 0.0), &controls));
  }

  #[test]
  fn an_input_inside_the_window_but_past_its_tick_is_counted_and_still_applied() {
    // The window forgives a little lateness on purpose, because a jittery link
    // produces it honestly. It is counted, because a steady stream of these is
    // the signal that the window is too tight for who is connected.
    let controls = Controls::default();
    let mut server = Server::new(50, 4, false);
    idle(&mut server, 480, &controls);

    let just_late = server.tick().saturating_sub(controls.input_max_late_ticks);
    assert!(server.submit_input(0, just_late, Vec2::new(1.0, 0.0), &controls), "inside the window");
    assert_eq!(server.rejected_inputs(), 0);
    assert_eq!(server.late_inputs(), 1, "and counted as late");
  }

  #[test]
  fn a_player_pressed_by_enemies_takes_hits_and_is_eventually_overrun() {
    let mut server = Server::new(200, 4, false);
    // Pin every enemy onto player 0 and point it at them, so the pile presses.
    let p0 = server.players[0];
    for enemy in server.enemies.iter_mut() {
      enemy.pos = p0;
      enemy.target = 0;
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
    for enemy in server.enemies.iter_mut() {
      enemy.pos = p0;
      enemy.target = 0;
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
