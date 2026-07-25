//! One player's client: holds only the entities relevant to it, draws them by
//! one of three strategies, and counts how often a packet refers to an entity it
//! no longer holds.
//!
//! That last part is the experiment about generational handles. A handle names a
//! slot *and* its occupant; if the generation is discarded, a reference to a dead
//! entity silently lands on whatever now occupies its slot. Whether that actually
//! happens here is measured, not assumed.


use std::collections::HashMap;

use plaza_client_utils::mirror::{Agreement, DeltaMirror};
use plaza_client_utils::{ease_in_quad, AckWindow, HeldInputConfig, HeldInputPredictor, InterpolationClock, RemoteView, RenderOpts, SlotKey};

use crate::sim::types::{PlayerFrame, coin_pull, difficulty, enemy_speed_scale, repulsor_pulse, step_coin, Coin, CoinId, Crowd, Upgrade, Wallet, COIN_FLIGHT_MS, COIN_PICKUP_RADIUS, step_enemy, Controls, Enemy, EnemyKind, Handle, LeaveReason, Packet, PlayerId, Projectile, RemoteMode, Vec2, PLAYER_MAX_HEALTH};

/// A fading damage number floating up from where a shot landed.
#[derive(Clone, Copy, Debug)]
pub struct DamagePopup {
  pub pos: Vec2,
  pub amount: u8,
  pub age: f32,
}

/// How long a damage number lingers.
const POPUP_SECS: f32 = 0.7;
/// How fast it drifts upward, world units per second.
const POPUP_RISE: f32 = 90.0;

impl DamagePopup {
  /// Where to draw it now: risen from where the hit landed.
  pub fn world_pos(&self) -> Vec2 {
    Vec2::new(self.pos.x, self.pos.y - POPUP_RISE * self.age)
  }
  /// Fades to nothing over its life.
  pub fn alpha(&self) -> f32 {
    (1.0 - self.age / POPUP_SECS).clamp(0.0, 1.0)
  }
}

/// A brief flash: a small spark where a shot lands, or a bigger burst where an
/// enemy dies. Purely presentation, and purely client-side: the client already
/// learns of hits (`Packet::hits`) and deaths (`LeaveReason::Died`), so nothing
/// new crosses the wire for the world to visibly react.
#[derive(Clone, Copy, Debug)]
pub struct Burst {
  pub pos: Vec2,
  pub age: f32,
  /// A death explosion rather than a hit spark: bigger and longer.
  pub big: bool,
}

const SPARK_SECS: f32 = 0.16;
const BOOM_SECS: f32 = 0.40;
/// A ceiling so an area pulse that kills hundreds at once spreads a scatter of
/// explosions rather than hundreds of overlapping rings.
const MAX_BURSTS: usize = 120;

impl Burst {
  fn life(&self) -> f32 {
    if self.big { BOOM_SECS } else { SPARK_SECS }
  }
  pub fn alpha(&self) -> f32 {
    (1.0 - self.age / self.life()).clamp(0.0, 1.0)
  }
  /// Grows outward over its life.
  pub fn radius(&self) -> f32 {
    let t = (self.age / self.life()).clamp(0.0, 1.0);
    if self.big { 6.0 + t * 26.0 } else { 2.0 + t * 6.0 }
  }
}


/// How much of the remaining gap to the server an enemy closes per correction.
/// Continuous, so there is no ease duration to outlast the send interval, which
/// is the failure the old `ErrorSmoother` path had at high rates.
const CORRECT_BLEND: f32 = 0.35;

/// Only the position is corrected. Target, kind and health are discrete facts the
/// server states outright, and blending them would invent values between.
fn lerp_enemy(a: &Enemy, b: &Enemy, t: f32) -> Enemy {
  Enemy { pos: lerp(&a.pos, &b.pos, t), ..*b }
}

fn lerp(a: &Vec2, b: &Vec2, t: f32) -> Vec2 {
  Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// The world an enemy's rule reads. Shared by every enemy in a tick, so it is
/// built once and handed round behind an `Arc` rather than cloned per entity.
#[derive(Clone, Debug, Default)]
struct EnemyCtx {
  players: Vec<Vec2>,
  repels: Vec<Option<f32>>,
  speed_scale: f32,
}

/// One enemy, simulated locally under the server's own rule and corrected by the
/// sparse samples that arrive.
///
/// The `Input` is the enemy's **target**, which is exactly what the wire carries:
/// the intent, sent only when it changes, not the position it produces.
type EnemySim = HeldInputPredictor<Enemy, PlayerId, std::sync::Arc<EnemyCtx>>;

/// The shared rule, in the shape a predictor wants. `step_enemy` itself is
/// untouched and is still what the server calls.
fn advance_enemy(enemy: &mut Enemy, target: &PlayerId, dt: f32, ctx: &std::sync::Arc<EnemyCtx>) {
  if ctx.players.is_empty() {
    return;
  }
  let t = *target as usize % ctx.players.len();
  step_enemy(enemy, ctx.players[t], ctx.repels[t], ctx.speed_scale, dt);
}

struct RemoteEnemy {
  /// Kind came in the spawn and never changes, so it is never re-sent.
  kind: EnemyKind,
  sim: EnemySim,
  last_pos: Vec2,
  last_ms: u64,
  prev_pos: Vec2,
  prev_ms: u64,
}

/// Snapshots buffered per player. Two is the minimum to interpolate between; a
/// few more absorb a jittery arrival without the buffer starving.
const PLAYER_VIEW_SNAPSHOTS: usize = 8;
/// Unused in practice: peers are interpolated and never extrapolated, so the
/// view holds the newest snapshot rather than dead reckoning past it. Kept as the
/// bound `RemoteView` is constructed with, and deliberately short, so turning
/// extrapolation on for an experiment cannot fling a peer across the arena.
const PLAYER_EXTRAPOLATE_MS: u64 = 120;
/// How fast the arrival-lateness statistics follow the link.
const ARRIVAL_SMOOTHING: f32 = 0.05;
/// The shortest gap between two samples that yields a usable velocity. Below it
/// the division amplifies noise into a spike.
const MIN_VELOCITY_GAP_MS: u64 = 8;

/// The single instant a frame is drawn at.
///
/// A newtype with a private field, so the only way to obtain one is
/// [`Client::render_at`]. That is the whole point: every remote thing on screen
/// has to be evaluated at the *same* time or the picture contradicts itself, and
/// the way that goes wrong is somebody reaching for `now_ms` in one draw path
/// because it is right there. This makes that unavailable rather than
/// discouraged.
///
/// Why one shared timeline at all, rather than drawing each thing as fresh as its
/// data allows: **a uniform delay is imperceptible and an inconsistent one is
/// not.** A world entirely 40 ms old looks right, because everything in it agrees
/// with everything else. A world where the enemies are at now, the peers are 25 ms
/// back and the shots are somewhere between shows its seams as muzzles detaching
/// from shooters and bullets passing through enemies. This is what every online
/// shooter does: one interpolation timeline for all remote state, and prediction
/// only for the entity you control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RenderAt(u64);

impl RenderAt {
  pub fn server_time_ms(self) -> u64 {
    self.0
  }
}

/// How long an announcement stays on screen.
const NOTICE_SECS: f32 = 3.0;

/// A coin travelling to whoever won it.
#[derive(Clone, Copy, Debug)]
pub struct CoinFlight {
  pub id: CoinId,
  /// Where it was when the claim landed. Kept so the curve has an origin; a
  /// formulation without one can only ever arrive linearly.
  from: Vec2,
  pub to_player: PlayerId,
  elapsed_ms: f32,
}

impl CoinFlight {
  /// Where to draw it, given where its owner is *now*.
  ///
  /// Re-aimed at the live player position every frame rather than at a remembered
  /// one, because the owner is moving: a path computed once at claim time would
  /// land where they used to be.
  pub fn at(&self, owner_now: Vec2) -> Vec2 {
    // Quadratic, not cubic. Both accelerate into the player; cubic covers 12.5%
    // of the distance by half time against quadratic's 25%, and over a flight
    // this short that difference is the whole readability of the effect.
    let t = ease_in_quad((self.elapsed_ms / COIN_FLIGHT_MS).clamp(0.0, 1.0));
    Vec2::new(self.from.x + (owner_now.x - self.from.x) * t, self.from.y + (owner_now.y - self.from.y) * t)
  }

  pub fn done(&self) -> bool {
    self.elapsed_ms >= COIN_FLIGHT_MS
  }
}

pub struct Client {
  pub id: PlayerId,
  /// This client's copy of the entity stream, and everything that says whether
  /// the copy is right: the generation checks, the sequence gaps, the
  /// acknowledgement window, and the digest the server compares against.
  ///
  /// All of that used to live here as loose fields. It is
  /// [`DeltaMirror`](plaza_client_utils::DeltaMirror) now, which is the exact
  /// counterpart of the server's `DeltaBaseline`: the two have to agree, and the
  /// only way to be sure they do is for both to be one implementation rather
  /// than two that look alike.
  enemies: DeltaMirror<RemoteEnemy>,
  /// The authoritative player positions, as last received.
  ///
  /// **This is what the shared rules read**, deliberately: `step_enemy` aims at
  /// a player, and the server aims at its own authoritative copy, so the client
  /// must too. Feeding a locally massaged position in here is the mistake that
  /// once made the whole horde lunge whenever the local player moved.
  players: Vec<Vec2>,
  /// The same players, buffered for *drawing* them smoothly between samples.
  ///
  /// Separate from `players` for the reason above: this is presentation, and it
  /// never reaches a rule. `RemoteView` is exactly the block for it, so a peer
  /// gets interpolation with dead reckoning on starvation rather than the raw
  /// newest sample, which is what made other players visibly teleport at low
  /// send rates.
  player_views: Vec<RemoteView<Vec2, Vec2>>,
  /// Server time of each player's last sample, for deriving a velocity and for
  /// rejecting a sample that arrives out of order.
  player_sample_ms: Vec<u64>,
  /// Each player's last derived velocity, kept so a pair of samples too close
  /// together in time does not produce a spike.
  player_velocity: Vec<Vec2>,
  /// Smoothed gap between player frame arrivals, in ms. Measured rather than
  /// taken from the configured rate, so the server may send at any rate it likes.
  arrival_interval_ms: f32,
  last_player_frame_ms: u64,
  /// Smoothed spread in how late player frames arrive, in ms.
  ///
  /// The render delay has to cover the *irregularity* of arrivals, not their
  /// average: a steady 200 ms link needs no more buffer than a steady 20 ms one,
  /// because a constant delay just shifts the whole timeline. What eats the
  /// buffer is one frame arriving later than its neighbours. Mean deviation
  /// rather than variance, which is what RFC 6298 uses for the same job and is
  /// cheaper and less spike-prone.
  arrival_jitter_ms: f32,
  /// The mean arrival lateness the deviation is measured against.
  arrival_mean_ms: f32,
  /// The clock peers are drawn against.
  ///
  /// It cannot be `now_ms - delay`, which is what the first version of this did.
  /// `now_ms` is an estimate of server time *now*, and a sample is a link delay
  /// old by the time it arrives, so that target sits *ahead* of the newest
  /// sample and the view spends its whole life extrapolating and then clamping.
  /// On a host it is worse still, because pongs come back instantly while frames
  /// go through the impairment link, so the two disagree by the whole latency.
  ///
  /// [`InterpolationClock::resync`] steers toward the newest sample instead, so
  /// the target self-calibrates to whatever the link is doing without knowing
  /// the latency, the jitter, or how good the clock sync is.
  render_clock: InterpolationClock<u64>,
  /// How far behind the server clock this client displays the world. Declared by
  /// the server, identical on every client, and never moved by the link.
  render_delay_ms: u64,
  /// Packets that arrived after the instant they describe had already gone past.
  /// The honest form of what an adaptive buffer used to hide by widening the
  /// delay until late packets fitted.
  underruns: u64,
  /// Packets that have arrived but whose moment has not come.
  ///
  /// **Nothing is applied on arrival.** A packet describes the world at its own
  /// `server_time_ms`, and it is applied when the render clock reaches that
  /// instant, so a death, a spark, a coin claim, a spawn and a position all land
  /// at the time they actually happened rather than at the time the network got
  /// round to delivering them. Without this the players were on the render
  /// timeline and everything else was ahead of it, which means there is no single
  /// instant the world can be asked about, and a replay camera has nothing to
  /// drive.
  queued: Vec<Packet>,
  /// The live shots, as the last packet reported them, and the server time that
  /// packet described.
  ///
  /// Authoritative: a shot that hit something stops appearing, which is the whole
  /// reason this is the live set rather than a spawn event the client flies
  /// itself. Carried forward to now for drawing, since a constant velocity
  /// evaluated at a time is exact.
  pub projectiles: Vec<Projectile>,
  projectiles_at_ms: u64,
  now_ms: u64,

  pub deaths_seen: u64,
  /// The distant crowds this client was told about, in place of the entities
  /// themselves. What lets it draw a whole-arena map from its own knowledge
  /// rather than from the server's.
  pub crowds: Vec<Crowd>,
  /// Coins this client can see.
  pub coins: Vec<Coin>,
  /// Coins in flight to the player who won them.
  ///
  /// **Presentation only, and deliberately client-side.** Magnet drift has to be
  /// a shared rule because it changes which player ends up nearest, and therefore
  /// decides the authoritative outcome. The flight happens *after* the claim is
  /// settled, so nothing about it can change the result: simulating it on the
  /// server would spend bandwidth and coupling on an animation, and would create
  /// a third state between "on the field" and "banked" that the loss-recovery
  /// machinery would then have to reason about. As pure presentation, a lost
  /// packet costs an animation rather than a currency inconsistency.
  pub flights: Vec<CoinFlight>,
  /// Every player's wallet as last confirmed.
  pub wallets: Vec<Wallet>,
  /// This client's *believed* balance, which equals the authoritative one unless
  /// balance prediction is on.
  ///
  /// Kept separate rather than overwriting `wallets[id].balance`, so the two can
  /// be compared. A single field would make the error unmeasurable, which is the
  /// mistake that has hidden four separate defects in this project already.
  pub believed_balance: u32,
  /// How many pickups this client has predicted, ever. The denominator the
  /// denial count means nothing without: no denials because every guess was
  /// right and no denials because it never guessed look identical otherwise.
  pub predicted_total: u64,
  /// Coins this client predicted it won. Cleared when the server confirms or
  /// denies.
  predicted_claims: Vec<CoinId>,
  /// Predictions the server gave to somebody else. The cost of predicting a
  /// discrete event: unlike a position, this correction cannot be eased.
  pub denied_claims: u64,
  /// Upgrades this client believes it owns, which may run ahead of the wallet
  /// while a purchase is in flight.
  pub believed_upgrades: Vec<Upgrade>,
  /// Packets received while this client believed it owned upgrades the server
  /// disagreed about.
  ///
  /// Cumulative, because the interesting quantity is the *window* during which
  /// the client was simulating under the wrong rule, and an end-of-run snapshot
  /// cannot see a transient that has since resolved. Sampling a settled state and
  /// reporting zero is the same mistake as checking `in_sync` mid-recovery.
  pub wrong_rule_packets: u64,
  /// Recent things worth telling the player about, newest last, with the age of
  /// each in seconds.
  ///
  /// Client-side and derived from what arrives, not sent by the server. An
  /// announcement is presentation: the server already communicated the fact by
  /// changing the wallet, and spending wire bytes to also say it in words would
  /// be paying twice for one event.
  pub notices: Vec<(String, f32)>,
  /// Estimated server clock, advanced locally between packets.
  est_server_ms: u64,
  /// Purchases asked for and not yet answered.
  ///
  /// Tracked so a request is made once rather than every frame until it lands.
  /// Without this the client spams the same request for a whole round trip, and
  /// the refusal count measures its own impatience instead of any real
  /// disagreement about the balance.
  pending_buys: Vec<Upgrade>,
  /// The newest sequence this client has actually *applied*. Only these are
  /// acknowledged, because that is what the server needs to know: not what
  /// arrived, but which state the client is in.

  /// Every player's health and respawn shield, from the last packet.
  pub player_health: Vec<u8>,
  pub player_invuln: Vec<bool>,
  /// Floating damage numbers from recent shots.
  pub popups: Vec<DamagePopup>,
  /// Hit sparks and death explosions.
  pub bursts: Vec<Burst>,
  /// The highest difficulty tier announced, so a step-up is announced once.
  last_difficulty_tier: u32,
}

impl Client {
  pub fn new(id: PlayerId, player_count: usize) -> Self {
    Self {
      id,
      enemies: DeltaMirror::new(),
      // The middle of the arena, not its corner. This is what a camera follows
      // before the first frame arrives, and the origin of a world measured from
      // one corner is a view of the outside of it.
      players: vec![Vec2::new(crate::sim::types::ARENA_W * 0.5, crate::sim::types::ARENA_H * 0.5); player_count],
      player_views: (0..player_count).map(|_| RemoteView::new(PLAYER_VIEW_SNAPSHOTS, PLAYER_EXTRAPOLATE_MS)).collect(),
      player_sample_ms: vec![0; player_count],
      player_velocity: vec![Vec2::default(); player_count],
      arrival_interval_ms: 0.0,
      last_player_frame_ms: 0,
      arrival_jitter_ms: 0.0,
      arrival_mean_ms: 0.0,
      render_clock: InterpolationClock::new(0),
      render_delay_ms: 100,
      underruns: 0,
      queued: Vec::new(),
      projectiles: Vec::new(),
      projectiles_at_ms: 0,
      now_ms: 0,
      deaths_seen: 0,
      crowds: Vec::new(),
      coins: Vec::new(),
      flights: Vec::new(),
      wallets: vec![Wallet::default(); player_count],
      believed_balance: 0,
      predicted_total: 0,
      predicted_claims: Vec::new(),
      denied_claims: 0,
      believed_upgrades: Vec::new(),
      notices: Vec::new(),
      est_server_ms: 0,
      pending_buys: Vec::new(),
      wrong_rule_packets: 0,
      player_health: vec![PLAYER_MAX_HEALTH as u8; player_count],
      player_invuln: vec![false; player_count],
      popups: Vec::new(),
      bursts: Vec::new(),
      last_difficulty_tier: 1,
    }
  }

  /// Which players this client believes are repelling enemies.
  ///
  /// Its own entry comes from `believed_upgrades`, which under prediction can run
  /// ahead of the truth. That is deliberate: it is the path by which a wrong
  /// purchase becomes a wrong simulation rather than only a wrong number.
  fn repel_flags(&self) -> Vec<Option<f32>> {
    // Evaluated against this client's *estimate* of server time, which is what
    // makes the pulse a consumer of clock sync as well as of the upgrade flag: a
    // client whose clock is off fires at the wrong moment and its enemies scatter
    // when the server's do not.
    let pulse = repulsor_pulse(self.est_server_ms);
    (0..self.players.len())
      .map(|p| {
        let owns = if p == self.id as usize {
          self.believed_upgrades.contains(&Upgrade::Repulsor)
        } else {
          self.wallets.get(p).is_some_and(|w| w.has(Upgrade::Repulsor))
        };
        if owns { pulse } else { None }
      })
      .collect()
  }

  /// Coins in flight, with where to draw each one this frame.
  pub fn flight_positions(&self) -> Vec<Vec2> {
    self
      .flights
      .iter()
      .map(|f| f.at(self.players[f.to_player as usize % self.players.len()]))
      .collect()
  }

  /// The active pulse radius for one player, as this client believes it.
  pub fn repel_radius(&self, player: usize) -> Option<f32> {
    self.repel_flags().get(player).copied().flatten()
  }

  /// This client's estimate of the server clock, which the pulse phase is read
  /// from.
  pub fn est_server_ms(&self) -> u64 {
    self.est_server_ms
  }

  /// Guesses which coins this client just won, and takes the credit immediately.
  ///
  /// The rule is the server's own: nearest player inside the radius claims it. The
  /// catch is the inputs. Remote player positions are a latency out of date, so
  /// when two players converge on one coin both can conclude they were nearest,
  /// and one of them is about to be told otherwise. That is the whole point of
  /// the toggle: the prediction is *usually* right, and being wrong costs a snap
  /// that cannot be smoothed away.
  fn predict_claims(&mut self) {
    let me = self.players[self.id as usize];
    let others: Vec<Vec2> = self.players.iter().enumerate().filter(|(p, _)| *p != self.id as usize).map(|(_, v)| *v).collect();
    let id = self.id;
    let mut won: Vec<(CoinId, Vec2)> = Vec::new();
    self.coins.retain(|coin| {
      let mine = coin.pos.dist(me);
      if mine > COIN_PICKUP_RADIUS {
        return true;
      }
      if others.iter().any(|o| coin.pos.dist(*o) < mine) {
        return true;
      }
      let _ = id;
      won.push((coin.id, coin.pos));
      false
    });
    for (coin, from) in won {
      self.predicted_total += 1;
      self.predicted_claims.push(coin);
      self.flights.push(CoinFlight {
        id: coin,
        from,
        to_player: self.id,
        elapsed_ms: 0.0,
      });
    }
    self.believed_balance = self.wallets[self.id as usize].balance + self.predicted_claims.len() as u32;
  }

  /// The cheapest upgrade this client believes it can afford and does not own.
  ///
  /// Judged against `believed_balance`, which is the point: with prediction on
  /// that number can be running ahead of the truth, so the client will ask for
  /// something it cannot actually pay for and the server will refuse.
  pub fn wants_to_buy(&mut self) -> Option<Upgrade> {
    let choice = Upgrade::ALL
      .iter()
      .filter(|u| !self.believed_upgrades.contains(u) && !self.pending_buys.contains(u) && self.believed_balance >= u.cost())
      .min_by_key(|u| u.cost())
      .copied();
    // Records the request and nothing else. What the client *believes* it owns is
    // derived in one place, on packet receipt, from the confirmed wallet plus
    // whatever is pending. Optimistically claiming it here as well made the
    // belief run ahead even with prediction switched off, which showed up as a
    // wrong-rule window in the control configuration.
    if let Some(u) = choice {
      self.pending_buys.push(u);
    }
    choice
  }

  pub fn known_entities(&self) -> usize {
    self.enemies.len()
  }

  /// Drop one held enemy, standing in for the silent mirror drift a real socket
  /// produces: an entity the server still believes this client holds, so it is
  /// only ever sampled again and the sample discarded. The incremental stream can
  /// never re-send it; only the acknowledged-digest check recovers it.
  #[cfg(test)]
  pub fn force_drop_an_enemy(&mut self) -> bool {
    let victim = self.enemies.keys().next();
    if let Some(key) = victim {
      self.enemies.remove(key);
      return true;
    }
    false
  }

  /// The difficulty multiplier this client believes, from its clock estimate.
  pub fn difficulty(&self) -> f32 {
    difficulty(self.est_server_ms)
  }

  /// Every player position as last known. What the shared rules read.
  pub fn players(&self) -> &[Vec2] {
    &self.players
  }

  /// Where each held enemy is **going** to be: the newest position received, from
  /// packets held but not yet due.
  ///
  /// The opposite of what the name suggests. The *actual* position is the delayed
  /// one, played out of the buffer at the render instant, and it is correct
  /// rather than approximate. This is the future, so the gap between them is the
  /// playout delay made visible: where the marker is about to resolve to. An
  /// empty ghost means the buffer has run dry.
  pub fn ghost_enemies(&self) -> Vec<(Vec2, EnemyKind)> {
    let mut newest: HashMap<u64, (u64, Vec2)> = HashMap::new();
    for packet in &self.queued {
      for sample in &packet.samples {
        let key = SlotKey::from(sample.handle).encode();
        let slot = newest.entry(key).or_insert((0, sample.pos));
        if packet.server_time_ms >= slot.0 {
          *slot = (packet.server_time_ms, sample.pos);
        }
      }
    }
    newest
      .into_iter()
      .filter_map(|(key, (_, pos))| self.enemies.get(SlotKey::decode(key)).map(|e| (pos, e.kind)))
      .collect()
  }

  /// Adopt the server's declared render delay. See
  /// [`ServerPolicy::render_delay_ms`](crate::sim::protocol::ServerPolicy::render_delay_ms).
  pub fn set_render_delay(&mut self, ms: u64) {
    self.render_delay_ms = ms;
  }

  pub fn render_delay_ms(&self) -> u64 {
    self.render_delay_ms
  }

  /// How many packets arrived too late to be played at the instant they describe.
  pub fn underruns(&self) -> u64 {
    self.underruns
  }

  /// The world every enemy's rule reads this tick, built once.
  ///
  /// The rule is a *shared* one, so it must read the authoritative player
  /// positions rather than the local prediction: feeding the prediction in makes
  /// enemies chase a point the server is not, and every packet snaps them back.
  fn enemy_ctx(&self) -> std::sync::Arc<EnemyCtx> {
    std::sync::Arc::new(EnemyCtx {
      players: self.players.clone(),
      repels: self.repel_flags(),
      speed_scale: enemy_speed_scale(self.est_server_ms),
    })
  }

  /// The instant this frame should be drawn at, once the stream has started.
  ///
  /// The only way to get a [`RenderAt`], and therefore the only way to draw
  /// anything: every remote accessor demands one, so a frame is consistent by
  /// construction rather than by everybody remembering to use the same clock.
  pub fn render_at(&self) -> Option<RenderAt> {
    self.render_clock.target().map(RenderAt)
  }

  /// Where to draw each shot, at the render instant.
  ///
  /// The packet that reported these was applied when the clock reached *its*
  /// timestamp, so the list is the set of shots alive at that moment, and this
  /// carries them the small remaining distance to the current render instant. A
  /// shot therefore leaves the player who fired it, both being drawn at the same
  /// time, and a shot the server has destroyed simply is not in the list.
  pub fn render_projectiles(&self, at: RenderAt) -> Vec<Vec2> {
    let age = at.server_time_ms().saturating_sub(self.projectiles_at_ms) as f32 / 1000.0;
    self
      .projectiles
      .iter()
      .map(|p| Vec2::new(p.pos.x + p.vel.x * age, p.pos.y + p.vel.y * age))
      .collect()
  }

  /// Where to *draw* each player: interpolated between samples, dead reckoned a
  /// little way past the newest, and held after that.
  ///
  /// Rendered a send interval in the past, which is what lets two samples
  /// bracket the target and makes the motion continuous. A caller substitutes
  /// its own predicted position for the local player, which is neither
  /// interpolated nor late.
  pub fn render_players(&self, at: RenderAt) -> Vec<Vec2> {
    let target = Some(at.0);
    self
      .player_views
      .iter()
      .enumerate()
      // Interpolate, never extrapolate. This is Gambetta's entity interpolation
      // exactly: render in the past by enough that two real snapshots bracket the
      // target, and accept a fixed, invisible display lag instead of guessing at
      // where a peer went. Dead reckoning a *player* is guessing at a human's
      // intention, which nothing on the wire carries, so it overshoots on every
      // direction change and then snaps back when the truth lands.
      .map(|(p, view)| {
        view
          .render(target, RenderOpts { interpolate: true, extrapolate: false })
          .unwrap_or_else(|| self.players[p])
      })
      .collect()
  }

  /// Folds in a player stream frame. See [`Controls::player_sync_hz`] for why
  /// this arrives separately from, and far more often than, the entity stream.
  pub fn on_player_frame(&mut self, frame: &PlayerFrame, recv_ms: u64) {
    self.now_ms = recv_ms;
    self.est_server_ms = frame.server_time_ms;
    for (p, pos) in &frame.players {
      self.observe_player(*p as usize, *pos, frame.server_time_ms);
    }
    // Steer the render clock toward the stream rather than toward a clock
    // estimate. Gently, so it glides instead of snapping on every frame.
    self.observe_arrival(frame.server_time_ms, recv_ms);
    if !frame.player_health.is_empty() {
      self.player_health.clone_from(&frame.player_health);
    }
    if !frame.player_invuln.is_empty() {
      self.player_invuln.clone_from(&frame.player_invuln);
    }
  }

  /// Records one authoritative player position, on whichever stream carried it.
  ///
  /// The velocity handed to the view is derived from the previous sample rather
  /// than sent: a player's direction is a human's input and nothing on the wire
  /// predicts it, so the last observed motion is the only honest guess.
  fn observe_player(&mut self, p: usize, pos: Vec2, server_time_ms: u64) {
    if p >= self.players.len() {
      return;
    }
    // Two streams carry players, and they interleave: the entity packet is built
    // less often and travels the same delayed link, so one can arrive *after* a
    // newer player frame. Taking it anyway walks the authoritative position
    // backwards in time, and `players` is what the enemy rule reads.
    let gap_ms = server_time_ms.saturating_sub(self.player_sample_ms[p]);
    if self.player_sample_ms[p] > 0 && gap_ms == 0 {
      return;
    }

    // A velocity is only meaningful over a real interval. Two samples a couple of
    // milliseconds apart divide a small position difference by a smaller time and
    // produce a spike, which the view then dead reckons along: that is what made
    // peers look worse than the teleporting they replaced.
    if gap_ms >= MIN_VELOCITY_GAP_MS {
      let previous = self.players[p];
      let elapsed = gap_ms as f32 / 1000.0;
      self.player_velocity[p] = Vec2::new((pos.x - previous.x) / elapsed, (pos.y - previous.y) / elapsed);
    }
    self.player_views[p].push(server_time_ms, pos, self.player_velocity[p]);
    self.player_sample_ms[p] = server_time_ms;
    self.players[p] = pos;
  }

  /// Overrides the local player's position with a locally predicted one.
  ///
  /// The networked client predicts its own movement (the server confirms it a
  /// round trip later), and the coin and repulsor rules this client runs read the
  /// local player position, so they should read the predicted one rather than the
  /// stale authoritative copy the last packet carried.
  pub fn set_local_pos(&mut self, pos: Vec2) {
    if (self.id as usize) < self.players.len() {
      self.players[self.id as usize] = pos;
    }
  }

  /// Notes that something arrived, and steers the render clock toward it.
  ///
  /// Both streams feed this. The clock has to track what has *arrived* to decide
  /// how far behind to sit, and the buffer it holds is sized from how irregular
  /// arrivals are rather than from any configured rate, so the server may send at
  /// whatever rate it likes and change it live.
  fn observe_arrival(&mut self, server_time_ms: u64, recv_ms: u64) {
    // Tracks the synced server clock (`recv_ms`), not the packet's timestamp:
    // steering by the timestamp puts the estimate one trip behind and makes T
    // depend on latency, which is the conflation this design removes.
    self.render_clock.resync(recv_ms, 1.0);

    let lateness = recv_ms.saturating_sub(server_time_ms) as f32;
    if self.last_player_frame_ms > 0 && server_time_ms > self.last_player_frame_ms {
      let gap = (server_time_ms - self.last_player_frame_ms) as f32;
      self.arrival_interval_ms = if self.arrival_interval_ms == 0.0 {
        gap
      } else {
        self.arrival_interval_ms + (gap - self.arrival_interval_ms) * ARRIVAL_SMOOTHING
      };
    }
    if server_time_ms > self.last_player_frame_ms {
      self.last_player_frame_ms = server_time_ms;
    }
    if self.arrival_mean_ms == 0.0 {
      self.arrival_mean_ms = lateness;
    } else {
      let deviation = (lateness - self.arrival_mean_ms).abs();
      self.arrival_mean_ms += (lateness - self.arrival_mean_ms) * ARRIVAL_SMOOTHING;
      self.arrival_jitter_ms += (deviation - self.arrival_jitter_ms) * ARRIVAL_SMOOTHING;
    }
  }

  /// Takes delivery of a packet. Does **not** apply it: see [`Client::queued`].
  ///
  /// The render clock is steered here rather than at apply time, because it has
  /// to track what has *arrived* in order to decide how far behind to sit.
  pub fn receive_packet(&mut self, packet: Packet, recv_ms: u64) {
    self.now_ms = recv_ms;
    self.observe_arrival(packet.server_time_ms, recv_ms);
    // Late: its instant has passed, so it can never be played at the right
    // moment. Counted rather than compensated for.
    if self.render_at().is_some_and(|at| packet.server_time_ms < at.server_time_ms()) {
      self.underruns += 1;
    }
    self.queued.push(packet);
  }

  /// Applies every queued packet whose moment has arrived, oldest first.
  ///
  /// Returns whether anything was applied, which is when a client has something
  /// new to acknowledge: the acknowledgement carries the digest of the mirror,
  /// so it has to describe the state actually *reached*, not the packets merely
  /// received.
  fn apply_due(&mut self, controls: &Controls) -> bool {
    let Some(at) = self.render_at() else {
      return false;
    };
    let now = at.server_time_ms();
    // Oldest first, so deltas compose in the order the server built them.
    self.queued.sort_by_key(|p| p.seq);
    let due = self.queued.iter().take_while(|p| p.server_time_ms <= now).count();
    if due == 0 {
      return false;
    }
    for packet in self.queued.drain(..due).collect::<Vec<_>>() {
      self.apply_packet(&packet, now, controls);
    }
    true
  }

  fn apply_packet(&mut self, packet: &Packet, recv_ms: u64, controls: &Controls) {
    // Every packet is applied, whatever baseline it names.
    //
    // Worth stating because the instinct is the opposite, and the instinct is
    // what a strict delta protocol requires: if you cannot reach the baseline,
    // discard. That is right when deltas are *relative* (add three, rotate by
    // ten). These are not. `entered` carries the entity in full, `left` names it
    // outright, and a sample is an absolute position, so applying them is
    // idempotent and applying a superset is harmless. Discarding instead starves
    // the client, and measurably: an earlier version of this did, and at 25% loss
    // the mirror emptied out while every agreement check read perfect.
    // A gap in the sequence is a dropped frame: every frame is numbered, the link
    // is ordered, so a jump of more than one means the wire lost the ones in
    // between. This is the direct measure of whether frames are being dropped.
    // Opening the packet: the mirror notes what the wire lost, acknowledges the
    // sequence, and tears itself down if this is a full baseline (which the
    // server only sends when it can no longer reach this mirror by deltas, so
    // merging into the old contents would keep the very drift it is repairing).
    // Only the enemy set is torn down; players, coins and wallets are absolute in
    // every packet already.
    // The panel can turn generations off to demonstrate what they prevent. The
    // mirror rebuilds when it changes, because the key space itself changes.
    self.enemies.set_generations(controls.generational_ids);
    self.enemies.begin(packet.seq, packet.full_baseline);
    self.now_ms = recv_ms;
    self.est_server_ms = packet.server_time_ms;
    let generational = controls.generational_ids;

    // The entity stream still carries the players, so a client is never without
    // them, and both streams feed the same buffer on the same server timeline.
    for (p, pos) in &packet.players {
      self.observe_player(*p as usize, *pos, packet.server_time_ms);
    }
    self.projectiles = packet.projectiles.clone();
    self.projectiles_at_ms = packet.server_time_ms;
    self.crowds.clone_from(&packet.crowds);
    if !packet.player_health.is_empty() {
      self.player_health.clone_from(&packet.player_health);
    }
    if !packet.player_invuln.is_empty() {
      self.player_invuln.clone_from(&packet.player_invuln);
    }
    for &(pos, amount) in &packet.hits {
      self.popups.push(DamagePopup { pos, amount, age: 0.0 });
      // A bright spark right at the hit, so the enemy there lights up.
      self.bursts.push(Burst { pos, age: 0.0, big: false });
    }
    // Where each coin was *before* this packet overwrites the list, so a claim can
    // launch its flight from where the player last saw it rather than from
    // nowhere. The server has already removed a claimed coin, so by the time the
    // claims are read its position is only recoverable from the old list.
    let previously_owned = self.wallets[self.id as usize].upgrades.clone();
    let was_at: std::collections::BTreeMap<CoinId, Vec2> = self.coins.iter().map(|c| (c.id, c.pos)).collect();
    self.coins.clone_from(&packet.coins);
    self.wallets.clone_from(&packet.wallets);
    // With coins off the server sends no wallets, so this list is empty and the
    // `self.wallets[self.id]` reads below would panic (and did: the first joiner
    // takes seat 3, so the index was 3 into a length of 0). Pad to cover this
    // player with a default wallet, which is exactly the right answer when there
    // is no currency: a zero balance and nothing owned.
    if self.wallets.len() <= self.id as usize {
      self.wallets.resize(self.id as usize + 1, Wallet::default());
    }

    // The authoritative claims. A prediction that is not in this list, for a coin
    // that is no longer on the field, went to somebody else.
    for (winner, coin) in &packet.claims {
      // Launch the flight from wherever it was last seen: the old list, or the
      // current position of a flight this client had already started on a
      // prediction. Re-aiming an in-flight coin rather than restarting it is what
      // keeps a denied prediction from teleporting.
      let from = self
        .flights
        .iter()
        .find(|f| f.id == *coin)
        .map(|f| f.at(self.players[f.to_player as usize % self.players.len()]))
        .or_else(|| was_at.get(coin).copied());
      self.flights.retain(|f| f.id != *coin);
      if let Some(from) = from {
        self.flights.push(CoinFlight {
          id: *coin,
          from,
          to_player: *winner,
          elapsed_ms: 0.0,
        });
      }

      if let Some(pos) = self.predicted_claims.iter().position(|c| c == coin) {
        self.predicted_claims.remove(pos);
        if *winner != self.id {
          // The correction that cannot be eased. A position can be smoothed
          // toward the truth over a few frames; a coin you already showed the
          // player collecting can only be taken back. Dropping the prediction is
          // the whole correction, because the balance is derived from the list.
          self.denied_claims += 1;
        }
      }
    }
    // The believed balance is **the confirmed value plus what is outstanding**,
    // never a number maintained independently.
    //
    // The independent version was the first attempt and it drifted by 115 coins
    // over a run, because it modelled income and not spending: every purchase the
    // server approved decremented the authoritative balance and left the local
    // one untouched. Deriving it instead makes prediction an *offset on confirmed
    // state*, which is the same shape as replaying unacknowledged inputs over an
    // authoritative snapshot, and it cannot drift because there is nothing to
    // drift from. Anything the server does that the client did not model is
    // absorbed for free.
    // A prediction is only outstanding while the coin is still unresolved. Once
    // the server's list no longer carries it, it is settled one way or another
    // and the prediction has to be retired, or the outstanding count grows
    // without bound.
    //
    let still_there: std::collections::BTreeSet<CoinId> = packet.coins.iter().map(|c| c.id).collect();
    self.predicted_claims.retain(|c| still_there.contains(c));
    // Hide coins already predicted, or the next tick predicts them again. The
    // server keeps sending them because from its point of view nothing has
    // happened yet.
    self.coins.retain(|c| !self.predicted_claims.contains(&c.id));

    // Announce what changed, by diffing the wallet against what we last held
    // rather than from anything the server says explicitly.
    for upgrade in &self.wallets[self.id as usize].upgrades {
      if !previously_owned.contains(upgrade) {
        self.notices.push((format!("{} acquired", upgrade.label()), 0.0));
      }
    }
    for upgrade in &packet.denied_buys {
      self.notices.push((format!("{} refused, not enough coins", upgrade.label()), 0.0));
    }

    // Refusals and confirmations both retire a pending request.
    for upgrade in &packet.denied_buys {
      self.pending_buys.retain(|u| u != upgrade);
    }
    let owned = &self.wallets[self.id as usize].upgrades;
    self.pending_buys.retain(|u| !owned.contains(u));

    let authoritative = self.wallets[self.id as usize].balance;
    let pending_cost: u32 = self.pending_buys.iter().map(|u| u.cost()).sum();
    self.believed_balance = if controls.predict_balance {
      (authoritative + self.predicted_claims.len() as u32).saturating_sub(pending_cost)
    } else {
      authoritative
    };

    // Under prediction the client shows an upgrade the moment it asks for it.
    //
    // This is the coupling worth having in the tree: `believed_upgrades` feeds
    // `step_enemy`, so an optimistic purchase that the server refuses leaves the
    // client simulating enemies under a rule the server is not using, for as long
    // as the refusal takes to arrive. A mispredicted *number* is a cosmetic wrong;
    // a mispredicted *rule* diverges the world.
    self.believed_upgrades.clone_from(owned);
    if controls.predict_balance {
      for u in &self.pending_buys {
        if !self.believed_upgrades.contains(u) {
          self.believed_upgrades.push(*u);
        }
      }
      self.believed_upgrades.sort_unstable();
    }

    // Counted *after* the belief is settled, not before. Checking beforehand
    // compares last packet's belief against this packet's truth, which measures
    // an unavoidable one-packet lag and reports a wrong-rule window even with
    // prediction switched off. What matters is the rule this client is about to
    // simulate under.
    if self.believed_upgrades != self.wallets[self.id as usize].upgrades {
      self.wrong_rule_packets += 1;
    }

    for (handle, reason) in &packet.left {
      let died = *reason == LeaveReason::Died;
      if died {
        self.deaths_seen += 1;
      }
      // A death gets an explosion where the client last had the enemy, so the
      // position is read before the removal. The mirror refuses a removal whose
      // generation names an occupant it no longer holds, and counts it: with
      // generations off the key matches and the wrong entity is deleted, which is
      // the whole demonstration.
      let pos = died.then(|| self.enemies.get(handle.into()).map(|e| e.sim.logical().pos)).flatten();
      if self.enemies.remove(handle.into()).is_some()
        && let Some(pos) = pos
      {
        self.bursts.push(Burst { pos, age: 0.0, big: true });
      }
    }

    for spawn in &packet.entered {
      self.enemies.insert(
        spawn.handle.into(),
        RemoteEnemy {
          kind: spawn.kind,
          sim: {
            let mut sim = HeldInputPredictor::new(
              Enemy {
                pos: spawn.pos,
                target: spawn.target,
                kind: spawn.kind,
                // Health is the server's business; a client only learns of death.
                health: spawn.kind.max_health(),
              },
              HeldInputConfig { blend: if controls.smooth { CORRECT_BLEND } else { 1.0 } },
              advance_enemy,
              lerp_enemy,
            );
            sim.hold(spawn.target);
            sim
          },
          last_pos: spawn.pos,
          last_ms: recv_ms,
          prev_pos: spawn.pos,
          prev_ms: recv_ms,
        },
      );
    }

    // How far this sample has to be carried to reach the instant being drawn.
    //
    // The **render target**, not now. Every other remote thing on screen is drawn
    // at the target, so simulating an enemy to now puts it on a different clock
    // from the peers and the shots, and the picture contradicts itself: a shot
    // leaves a player who is 25 ms in the past and arrives at an enemy who is
    // not. One timeline for all remote state is what an online shooter does, and
    // the delay is invisible precisely because it is uniform.
    //
    // Before the stream starts there is no target, so fall back to the arrival
    // clock; that is the join transient and it lasts one render delay.
    let render_now = self.render_clock.target().unwrap_or(recv_ms);
    let age_ms = render_now.saturating_sub(packet.server_time_ms);
    // Built once for the whole packet, before the loop borrows the mirror.
    let ctx = self.enemy_ctx();

    for sample in &packet.samples {
      // A sample for an occupant this mirror no longer holds is refused and
      // counted, rather than moving whoever inherited the slot.
      let Some(entity) = self.enemies.get_mut(sample.handle.into()) else {
        continue;
      };
      if let Some(t) = sample.target {
        entity.sim.hold(t);
      }

      entity.prev_pos = entity.last_pos;
      entity.prev_ms = entity.last_ms;
      entity.last_pos = sample.pos;
      entity.last_ms = packet.server_time_ms;

      if controls.mode == RemoteMode::Simulate {
        // The sample describes an instant slightly before the one being drawn, so
        // it is advanced by its own age under the same rule before the correction
        // eases toward it. `project` inside `reconcile` is that advance.
        entity.sim.set_context(ctx.clone());
        let mut settled = *entity.sim.logical();
        settled.pos = sample.pos;
        entity.sim.reconcile(settled, age_ms as f32 / 1000.0);
      }
    }

    // Everything in this packet is applied, so the mirror must now match what the
    // server said it should be. This is the check that a lost or malformed
    // despawn cannot hide from, and the digest it settles on rides the next
    // acknowledgement so the server can see the same disagreement and repair it.
    if generational {
      if let Agreement::Diverged { held, expected } = self.enemies.settle(packet.visible_digest) {
        // A digest detects a divergence and cannot diagnose one, so under
        // `debug_digest` the server ships its own key set alongside. Which side
        // the difference falls on names the bug: `missing` was lost or never
        // sent, `extra` is a removal that never landed.
        if !packet.debug_keys.is_empty() {
          let divergence = self.enemies.divergence_from(packet.debug_keys.iter().copied());
          let spell = |k: &SlotKey| (k.index, k.generation);
          eprintln!(
            "digest mismatch seq={} baseline_seq={:?} held={held:016x} server={expected:016x} extra(slot,gen)={:?} missing(slot,gen)={:?}",
            packet.seq,
            packet.baseline_seq,
            divergence.extra.iter().map(spell).collect::<Vec<_>>(),
            divergence.missing.iter().map(spell).collect::<Vec<_>>(),
          );
        }
      }
    }
  }

  /// Advances the render clock, plays out every packet whose moment has come,
  /// and steps everything that moves between packets.
  ///
  /// Returns whether a packet was applied, which is when there is something new
  /// to acknowledge.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) -> bool {
    self.now_ms += dt_ms;
    self.est_server_ms += dt_ms;
    let dt = dt_ms as f32 / 1000.0;

    // One observed send interval, which is the minimum for two samples to bracket
    // the target, plus enough to cover how irregular arrivals actually are.
    //
    // **Observed, not configured.** The client never asks what rate the server
    // was set to: it measures the gap between arrivals and the spread in their
    // lateness, so the server is free to send at whatever rate it likes, change
    // it live, or have the number mean something different by the time it
    // arrives. A client that trusts a configured rate is wrong exactly when the
    // rate is being changed, which is when it matters.
    //
    // Only the *interpolated* stream constrains this. Enemies are simulated
    // forward from one sample, so however slowly they arrive they do not widen
    // the buffer; the peers need two samples to interpolate between, so they do.
    self.render_clock.set_delay(self.render_delay_ms);
    self.render_clock.advance(dt_ms);

    // The clock has moved, so anything it has now reached is played out before
    // the world is stepped forward from it.
    let applied = self.apply_due(controls);

    if controls.mode == RemoteMode::Simulate {
      // Built once and shared: the rule reads the same world for every enemy, so
      // the per-entity cost is one `Arc` clone rather than a copy of the world.
      let ctx = self.enemy_ctx();
      for entity in self.enemies.values_mut() {
        entity.sim.set_context(ctx.clone());
        entity.sim.advance(dt);
      }
    }

    // Coins move under the same shared rule the server runs, so they stay put
    // between packets instead of stuttering, and a magnet looks like a magnet.
    if controls.coins {
      let attractors: Vec<(Vec2, f32, f32)> = (0..self.players.len())
        .map(|p| {
          let magnet = if p == self.id as usize {
            self.believed_upgrades.contains(&Upgrade::Magnet)
          } else {
            self.wallets.get(p).is_some_and(|w| w.has(Upgrade::Magnet))
          };
          let (radius, speed) = coin_pull(magnet);
          (self.players[p], radius, speed)
        })
        .collect();
      for coin in &mut self.coins {
        step_coin(coin, &attractors, dt);
      }
      if controls.predict_balance {
        self.predict_claims();
      }

      // Flights are advanced by frame time and dropped on arrival. Nothing here
      // touches the balance: that moved when the claim was decided, so the coin
      // sprite is only catching up with a number the player already has.
      for flight in &mut self.flights {
        flight.elapsed_ms += dt_ms as f32;
      }
      self.flights.retain(|f| !f.done());
    }

    for notice in &mut self.notices {
      notice.1 += dt;
    }
    self.notices.retain(|(_, age)| *age < NOTICE_SECS);

    // Damage numbers rise and fade; sparks and explosions expand and fade.
    for popup in &mut self.popups {
      popup.age += dt;
    }
    self.popups.retain(|p| p.age < POPUP_SECS);
    for burst in &mut self.bursts {
      burst.age += dt;
    }
    self.bursts.retain(|b| b.alpha() > 0.0);
    // A pulse can queue hundreds of deaths at once; keep the newest so the field
    // is a scatter of explosions rather than one solid flash.
    if self.bursts.len() > MAX_BURSTS {
      let excess = self.bursts.len() - MAX_BURSTS;
      self.bursts.drain(0..excess);
    }

    // Announce a difficulty step-up once, derived from the shared clock, so a
    // client whose clock is off announces it at a slightly different moment.
    let tier = difficulty(self.est_server_ms).floor() as u32;
    if tier > self.last_difficulty_tier {
      self.last_difficulty_tier = tier;
      self.notices.push((format!("difficulty up  (x{tier})  enemies faster, hits harder"), 0.0));
    }
    // Expiry only. The server owns when a shot ends: it stops listing one that
    // hit something, and the list is replaced wholesale by each packet as it is
    // played out.
    if let Some(at) = self.render_at() {
      let age = at.server_time_ms().saturating_sub(self.projectiles_at_ms) as f32 / 1000.0;
      self.projectiles.retain(|p| p.ttl - age > 0.0);
    }
    applied
  }

  /// Which packets have arrived. Twelve bytes back up the wire, and the whole
  /// input to the server's recovery: it needs to know which of its deltas the
  /// client is actually holding, and nothing else.
  pub fn acks(&self) -> &AckWindow {
    self.enemies.acks()
  }

  /// The digest of this client's mirror after the most recent packet, sent up on
  /// the next acknowledgement so the server can detect a drifted mirror the delta
  /// stream cannot itself recover.
  pub fn last_digest(&self) -> u64 {
    self.enemies.digest()
  }

  /// A reference whose generation did not match what we hold. With generations
  /// on these are rejected; with them off the same reference would have been
  /// applied to the wrong entity.
  pub fn stale_refs(&self) -> u64 {
    self.enemies.stale_refs()
  }

  /// Packets after which this client's mirror did not match the server's digest.
  pub fn digest_mismatches(&self) -> u64 {
    self.enemies.divergences()
  }

  /// Frames the wire dropped, detected as gaps in the sequence number. The direct
  /// cause of a digest mismatch: a lost frame is a hole recovery must re-derive.
  pub fn frames_lost(&self) -> u64 {
    self.enemies.frames_lost()
  }

  /// Where this client draws each enemy it knows about.
  pub fn render(&self, controls: &Controls, at: RenderAt) -> Vec<(Handle, Vec2, EnemyKind)> {
    self
      .enemies
      .iter()
      .map(|(key, e)| {
        let pos = match controls.mode {
          RemoteMode::Simulate => e.sim.render().pos,
          RemoteMode::DeadReckon => {
            let span = e.last_ms.saturating_sub(e.prev_ms) as f32 / 1000.0;
            let ahead = at.server_time_ms().saturating_sub(e.last_ms) as f32 / 1000.0;
            if span > 1e-3 {
              let vx = (e.last_pos.x - e.prev_pos.x) / span;
              let vy = (e.last_pos.y - e.prev_pos.y) / span;
              Vec2::new(e.last_pos.x + vx * ahead, e.last_pos.y + vy * ahead)
            } else {
              e.last_pos
            }
          }
          RemoteMode::Interpolate => {
            let target = at.server_time_ms();
            let span = e.last_ms.saturating_sub(e.prev_ms) as f32;
            if span > 1e-3 {
              let t = (target.saturating_sub(e.prev_ms) as f32 / span).clamp(0.0, 1.0);
              lerp(&e.prev_pos, &e.last_pos, t)
            } else {
              e.last_pos
            }
          }
        };
        (key.into(), pos, e.kind)
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{ARENA_H, ARENA_W, MAX_PLAYERS};

  #[test]
  fn a_client_with_no_packets_yet_looks_at_the_middle_of_the_arena() {
    // What a camera follows before the first frame, and it regressed silently
    // once: deleting the local prediction removed a predictor seeded at the
    // centre and left a zeroed array behind it. The arena is measured from a
    // corner, so the origin is a view of the outside of the world.
    for player_count in [1, MAX_PLAYERS] {
      let client = Client::new(0, player_count);
      assert!(client.render_at().is_none(), "nothing has started the timeline yet");
      for (p, pos) in client.players().iter().enumerate() {
        assert_eq!(
          (pos.x, pos.y),
          (ARENA_W * 0.5, ARENA_H * 0.5),
          "player {p} of {player_count} starts at the centre, not the corner"
        );
      }
    }
  }

  fn flight(from: Vec2) -> CoinFlight {
    CoinFlight {
      id: 1,
      from,
      to_player: 0,
      elapsed_ms: 0.0,
    }
  }

  #[test]
  fn a_flight_takes_the_same_time_however_far_it_has_to_go() {
    // The property the whole formulation exists for. A constant-speed move gives
    // a fixed *speed*, so arrival lags with distance and a coin taken from the rim
    // of the pickup radius trails one taken from underfoot. Interpolating over a
    // normalized duration gives a fixed *time* instead.
    let near = flight(Vec2::new(10.0, 0.0));
    let far = flight(Vec2::new(4000.0, 0.0));
    assert_eq!(near.done(), far.done());
    for elapsed in [COIN_FLIGHT_MS * 0.5, COIN_FLIGHT_MS] {
      let (mut a, mut b) = (near, far);
      a.elapsed_ms = elapsed;
      b.elapsed_ms = elapsed;
      assert_eq!(a.done(), b.done(), "both are equally finished at {elapsed}ms");
    }
  }

  #[test]
  fn a_flight_lands_on_the_owner_even_though_the_owner_moved() {
    // Re-aimed every frame rather than following a path computed at claim time,
    // because the target is a player who is still running. A precomputed path
    // would land where they used to be.
    let mut f = flight(Vec2::new(0.0, 0.0));
    f.elapsed_ms = COIN_FLIGHT_MS;
    let moved = Vec2::new(900.0, -400.0);
    let landed = f.at(moved);
    assert!((landed.x - moved.x).abs() < 0.001 && (landed.y - moved.y).abs() < 0.001, "landed at {landed:?}");
  }

  #[test]
  fn the_curve_starts_slow_and_arrives_fast() {
    // Ease-in rather than ease-out: a coin under a pull that grows as it closes,
    // not a correction that should begin at once and settle gently.
    let mut f = flight(Vec2::new(0.0, 0.0));
    let owner = Vec2::new(100.0, 0.0);
    f.elapsed_ms = COIN_FLIGHT_MS * 0.5;
    let halfway = f.at(owner).x;
    assert!(halfway < 50.0, "less than half the distance covered in half the time: {halfway:.1}");
  }
}
