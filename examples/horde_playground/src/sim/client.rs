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
use plaza_client_utils::{
  ease_in_quad, AckWindow, Admission, ArrivalMonitor, HeldInputConfig, HeldInputPredictor, InterpolationClock, PlayoutBuffer, RemoteView,
  RenderOpts, SlotKey,
};

use crate::sim::types::{PlayerFrame, dequantize_far, coin_pull, difficulty, enemy_speed_scale, repulsor_pulse, step_coin, Coin, CoinId, Crowd, Upgrade, Wallet, COIN_FLIGHT_MS, COIN_PICKUP_RADIUS, step_enemy, Controls, Enemy, EnemyKind, Handle, LeaveReason, Packet, PlayerId, RemoteMode, Shot, Vec2, PLAYER_MAX_HEALTH};

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

/// Snapshots buffered per player: enough history to interpolate at the deepest
/// render delay the panel can declare, with both streams feeding samples at
/// their maximum rate, plus slack for jitter.
///
/// **Derived from the thing it must cover, not picked.** This was a constant 8,
/// which at the default rates held roughly 200 ms of history: fine at the
/// default 150 ms render delay, and quietly wrong the moment the slider went
/// past what it covered. The view then clamped every render to the oldest
/// snapshot it still had, so the marker rode a couple of hundred milliseconds
/// behind "now" while the rest of the scene was faithfully at T, and the gap
/// between them scaled with the slider. Found by playing, at 600 ms: the
/// weapon appeared to fire from the player's past, and the shots were the
/// correctly placed half of the picture.
const PLAYER_VIEW_SNAPSHOTS: usize = ((crate::sim::types::RENDER_DELAY_MAX_MS as usize * 2 * crate::sim::types::SEND_RATE_MAX_HZ as usize) / 1000) + 8;
/// Unused in practice: peers are interpolated and never extrapolated, so the
/// view holds the newest snapshot rather than dead reckoning past it. Kept as the
/// bound `RemoteView` is constructed with, and deliberately short, so turning
/// extrapolation on for an experiment cannot fling a peer across the arena.
const PLAYER_EXTRAPOLATE_MS: u64 = 120;
/// The most packets a client will hold waiting for their moment.
///
/// A queue fed by a remote peer and drained by a local clock has to be bounded,
/// or a client that stops draining (a browser tab in the background, a stalled
/// frame loop) accumulates without limit. Sized well past any honest buffer:
/// at the maximum render delay and the fastest send rate this is several times
/// what a healthy client holds, so reaching it means something is wrong rather
/// than merely slow.
const MAX_QUEUED_PACKETS: usize = 256;

/// How far ahead of the render instant the queue may reach before the client
/// treats itself as *lost* rather than buffering.
///
/// A discontinuity, not a delay: the arithmetic that recovers a late packet has
/// nothing to say about a client that missed a minute. Snap, as with any other
/// discontinuity, rather than easing across a gap that has no intermediate
/// states to ease through.
const LOST_AHEAD_MS: u64 = 3_000;

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
  /// Where to draw it, given where its owner is *drawn this frame*.
  ///
  /// Re-aimed at the drawn player position every frame rather than at a
  /// remembered one, because the owner is moving: a path computed once at claim
  /// time would land where they used to be, and one aimed at the newest
  /// authoritative position lands ahead of the marker on screen.
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
  /// [`DeltaMirror`](plaza_client_utils::DeltaMirror) is the exact counterpart
  /// of the server's `DeltaBaseline`: the two have to agree, and the only way
  /// to be sure they do is for both to be one implementation rather than two
  /// that look alike.
  enemies: DeltaMirror<RemoteEnemy>,
  /// The authoritative player positions, as last received: the newest thing
  /// this client knows, which makes it the **future** relative to the instant
  /// being drawn.
  ///
  /// Three things read it, and each is deliberate: the ghost overlay (whose job
  /// is the future), the fallback before the timeline starts (when there is
  /// nothing else), and the per-player velocity derivation. The shared rules do
  /// **not** read it any more: they read [`Client::drawn_players`], the same
  /// authoritative state reconstructed at the render instant, because a rule
  /// evaluated at T reading a position from the newest packet is two timelines
  /// in one scene.
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
  /// Which players this client has ever been told about.
  ///
  /// Necessary once the player stream is relevance-limited: a player who has
  /// never been sent still occupies a slot in every per-player array, holding
  /// the arena-centre seed, and drawing that is a **phantom peer** standing in
  /// the middle of the map. The seed is right for a camera with nothing to
  /// follow and wrong for a marker, and the difference is exactly whether this
  /// client has heard anything.
  ///
  /// The enemy rule needs no such guard: an enemy is only sent to a client whose
  /// relevant set already contains that enemy's target, so a rule input is never
  /// read from a slot that was never filled.
  player_seen: Vec<bool>,
  /// The first instant this client ever heard about each player.
  ///
  /// Separates the join transient from real starvation. Until the render clock
  /// has caught up to a player's first sample, that player's history cannot
  /// cover the instant being drawn and there is nothing wrong with that; after
  /// it, the same condition means history was evicted or arrived too sparsely,
  /// which is worth reporting.
  player_first_ms: Vec<u64>,
  /// Each player's last derived velocity, kept so a pair of samples too close
  /// together in time does not produce a spike.
  player_velocity: Vec<Vec2>,
  /// How the player stream actually arrives: the measured terms of the
  /// render-delay budget. See [`ArrivalMonitor`] for the two measurement
  /// decisions (jitter as mean deviation, intervals between declared stamps).
  arrivals: ArrivalMonitor,
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
  /// Packets that have arrived but whose moment has not come, plus the
  /// underrun and restart accounting that used to be hand-rolled here.
  ///
  /// **Nothing is applied on arrival.** A packet describes the world at its own
  /// `server_time_ms`, and it is applied when the render clock reaches that
  /// instant, so a death, a spark, a coin claim, a spawn and a position all land
  /// at the time they actually happened rather than at the time the network got
  /// round to delivering them. Without this the players were on the render
  /// timeline and everything else was ahead of it, which means there is no single
  /// instant the world can be asked about, and a replay camera has nothing to
  /// drive.
  playout: PlayoutBuffer<Packet>,
  /// Shots in flight, as **events**: an origin, a velocity and a fire time each.
  ///
  /// The client flies them itself, which is what puts them on the same delayed
  /// timeline as everything else: a constant velocity from a known origin is
  /// exact at any instant, so a shot can be drawn at the render target rather
  /// than only where the last packet happened to say it was. They leave the list
  /// on their own expiry, which both sides compute, or on an explicit end when
  /// the server says one hit something.
  shots: Vec<Shot>,
  now_ms: u64,

  pub deaths_seen: u64,
  /// When the most recent area pulse fired, on the server clock, as the last
  /// applied packet declared it. The ring is derived from this and the frame
  /// clock, so it fires once, repeats are idempotent by construction, and a
  /// mid-pulse joiner draws the remainder of the ring it walked in on.
  nova_at_ms: Option<u64>,
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
  /// Health and shield samples waiting for their instant: the server time they
  /// describe, and the vitals as sent, which is a **subset** of players now.
  /// Applied when the render clock reaches them, like every other packet, so a
  /// bar does not drop one render delay before the hit that caused it is
  /// visible.
  health_queue: Vec<(u64, Vec<(PlayerId, u8, bool)>)>,
  /// The server time of the health state currently applied. Two streams carry
  /// health at different rates, and without this the slower one walks it
  /// backwards.
  health_at_ms: u64,
  /// Times a player was drawn off the render instant because its view could
  /// not reach it: history no longer covering the target (clamped to the
  /// oldest snapshot), or no samples at all (the newest authoritative copy
  /// stands in). Either way that player is silently on a different timeline
  /// from the rest of the scene for the frame, which is the pattern that hid
  /// both the corner-camera regression and the detached-marker bug, so it is
  /// counted rather than quiet.
  view_fallbacks: std::sync::atomic::AtomicU64,
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
      player_seen: vec![false; player_count],
      player_first_ms: vec![0; player_count],
      player_velocity: vec![Vec2::default(); player_count],
      arrivals: ArrivalMonitor::new(ARRIVAL_SMOOTHING),
      render_clock: InterpolationClock::new(0),
      render_delay_ms: 100,
      playout: PlayoutBuffer::new(MAX_QUEUED_PACKETS, LOST_AHEAD_MS),
      shots: Vec::new(),
      now_ms: 0,
      deaths_seen: 0,
      nova_at_ms: None,
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
      health_queue: Vec::new(),
      health_at_ms: 0,
      view_fallbacks: std::sync::atomic::AtomicU64::new(0),
      pending_buys: Vec::new(),
      wrong_rule_packets: 0,
      player_health: vec![PLAYER_MAX_HEALTH as u8; player_count],
      player_invuln: vec![false; player_count],
      popups: Vec::new(),
      bursts: Vec::new(),
      last_difficulty_tier: 1,
    }
  }

  /// The instant every shared-clock rule is evaluated at this frame: the render
  /// instant once the timeline has started, and the local arrival clock during
  /// the join transient, when there is nothing on screen for it to disagree with.
  ///
  /// This used to be a field with **two writers on two timelines**: the newest
  /// player frame's timestamp on arrival, and the packet's timestamp at
  /// play-out. The first is roughly `now - one_way`, the second is roughly the
  /// render instant, and the value oscillated between them by about the render
  /// delay, wobbling the pulse phase, the speed scale and the difficulty tier.
  /// Deriving it from the render clock makes it the same T everything else in
  /// the frame is already evaluated at, which is the one-instant principle
  /// rather than a third clock.
  fn frame_clock_ms(&self) -> u64 {
    self.render_clock.target().unwrap_or(self.now_ms)
  }

  /// Which players this client believes are repelling enemies.
  ///
  /// Its own entry comes from `believed_upgrades`, which under prediction can run
  /// ahead of the truth. That is deliberate: it is the path by which a wrong
  /// purchase becomes a wrong simulation rather than only a wrong number.
  fn repel_flags(&self) -> Vec<Option<f32>> {
    // The pulse phase at the frame instant, which is what the server's own
    // pulse looked like when it produced the state standing on screen. Still a
    // consumer of clock sync: a client whose clock is off places T wrongly,
    // fires at the wrong moment, and its enemies scatter when the server's do
    // not.
    let pulse = repulsor_pulse(self.frame_clock_ms());
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
  ///
  /// Aimed at the **drawn** owner, not the newest one. A flight is pure
  /// presentation, so its target is the marker the player can see; aiming at
  /// the newest authoritative position landed the animation ahead of the body
  /// it belonged to.
  pub fn flight_positions(&self) -> Vec<Vec2> {
    let drawn = self.drawn_players();
    self
      .flights
      .iter()
      .map(|f| f.at(drawn[f.to_player as usize % drawn.len()]))
      .collect()
  }

  /// The active pulse radius for one player, as this client believes it.
  pub fn repel_radius(&self, player: usize) -> Option<f32> {
    self.repel_flags().get(player).copied().flatten()
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
    // Distances measured between the drawn players and the coins, which are on
    // the same timeline. The newest player array was the obvious input and the
    // wrong one: the coins stand at the render instant, so measuring them
    // against players from the newest packet compared two different moments,
    // and a pickup could trigger before the marker visibly touched the coin.
    let drawn = self.drawn_players();
    let me = drawn[self.id as usize];
    let others: Vec<Vec2> = drawn.iter().enumerate().filter(|(p, _)| *p != self.id as usize).map(|(_, v)| *v).collect();
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

  /// The difficulty multiplier this client believes, at the instant on screen.
  pub fn difficulty(&self) -> f32 {
    difficulty(self.frame_clock_ms())
  }

  /// How long ago the last area pulse fired, at the instant on screen, while
  /// the ring is still worth drawing.
  ///
  /// A pure function of the declared timestamp and the frame clock: nothing to
  /// trigger, nothing to decay, nothing for a recovery repeat to re-fire. The
  /// same shape as the offline world's, which reads its server directly.
  pub fn nova_flash_age(&self) -> Option<f32> {
    let fired = self.nova_at_ms?;
    let age = self.frame_clock_ms().saturating_sub(fired) as f32 / 1000.0;
    (age <= crate::sim::types::NOVA_RING_SECS).then_some(age)
  }

  /// Whether this client has ever been told where a player is.
  ///
  /// False for anyone outside its relevance, whose slot still holds the seed. A
  /// renderer must not draw those: the seed sits at the arena centre and would
  /// appear as a peer standing there.
  pub fn knows_player(&self, p: usize) -> bool {
    self.player_seen.get(p).copied().unwrap_or(false)
  }

  /// How long ago this client last heard where a player is, at the instant on
  /// screen. `None` if it has never heard.
  ///
  /// A marker drawn from a position nobody has confirmed in a while is making a
  /// claim it cannot support, and the honest answer is to fade it out rather
  /// than to keep drawing it at full strength. Even with a far tier this
  /// happens: a player who disconnects stops arriving in either tier.
  pub fn player_age_secs(&self, p: usize) -> Option<f32> {
    if !self.knows_player(p) {
      return None;
    }
    let at = self.frame_clock_ms();
    Some(at.saturating_sub(*self.player_sample_ms.get(p)?) as f32 / 1000.0)
  }

  /// Every player position as last known: the newest authoritative copy, which
  /// is the *future* relative to the instant on screen. The ghost overlay's
  /// source, and the fallback before the timeline starts. The shared rules read
  /// the drawn positions instead; see [`Client::drawn_players`].
  pub fn players(&self) -> &[Vec2] {
    &self.players
  }

  /// Where each held enemy is **going** to be: the newest position received, from
  /// packets held but not yet due.
  ///
  /// The opposite of what the name suggests. The *actual* position is the delayed
  /// one, played out of the buffer at the render instant, and it is correct
  /// rather than approximate. This is the future, so the gap between them is the
  /// render delay made visible: where the marker is about to resolve to. An
  /// empty ghost means the buffer has run dry.
  pub fn ghost_enemies(&self) -> Vec<(Vec2, EnemyKind)> {
    let mut newest: HashMap<u64, (u64, Vec2)> = HashMap::new();
    for packet in self.playout.iter() {
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
    self.playout.underruns()
  }

  /// Times the timeline was abandoned and rebuilt because the client had fallen
  /// too far behind to play its way out. Zero on a healthy client; anything else
  /// is a stall worth knowing about.
  pub fn resyncs(&self) -> u64 {
    self.playout.restarts()
  }

  /// The render-delay budget as this client has actually measured it, for the
  /// panel to hold against the delay in force. A host can compute the budget
  /// from its sliders; a joiner can only measure, which is also what stays
  /// honest when the host changes a rate live.
  pub fn measured_arrivals(&self) -> &ArrivalMonitor {
    &self.arrivals
  }

  /// Every player at the instant this frame is drawn at: the interpolated
  /// authoritative positions once the timeline has started, and the newest
  /// authoritative copy during the join transient.
  ///
  /// This is what a rule or an effect evaluated at the render instant reads.
  /// It is still authoritative-derived state, only reconstructed at the right
  /// moment; what the presentation-isolation principle forbids feeding a shared
  /// rule is *predicted* state, and nothing here predicts.
  fn drawn_players(&self) -> Vec<Vec2> {
    self.render_at().map(|at| self.render_players(at)).unwrap_or_else(|| self.players.clone())
  }

  /// The world every enemy's rule reads this tick, built once.
  ///
  /// Everything in it is evaluated at the frame instant, because the enemies
  /// reading it stand there: the players as drawn, the pulse phase at T, the
  /// speed scale at T. The server stepped these enemies against its own players
  /// and its own clock at the moment it produced the state on screen, so this
  /// is the reconstruction of what the rule actually consumed, where the newest
  /// player array would be an input from a different timeline (the enemy-aim
  /// entry in IMPROVEMENTS, now closed). It is authoritative-derived either
  /// way; feeding a locally *predicted* position in here is the mistake that
  /// once made the whole horde lunge whenever the local player moved.
  fn enemy_ctx(&self) -> std::sync::Arc<EnemyCtx> {
    std::sync::Arc::new(EnemyCtx {
      players: self.drawn_players(),
      repels: self.repel_flags(),
      speed_scale: enemy_speed_scale(self.frame_clock_ms()),
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
  /// Evaluated from the event rather than carried forward from a reported
  /// position, so a shot is placed *exactly* at the instant being drawn instead
  /// of at whatever moment the last packet described. A shot therefore leaves
  /// the player who fired it, both being drawn at the same time, and one whose
  /// moment has not arrived yet is simply not drawn.
  pub fn render_projectiles(&self, at: RenderAt) -> Vec<Vec2> {
    let now = at.server_time_ms();
    self
      .shots
      .iter()
      .filter(|shot| now >= shot.fired_ms && !shot.expired(now))
      .map(|shot| shot.at(now))
      .collect()
  }

  /// How many shots this client is holding, live or not yet due. A readout, and
  /// the number that showed the old design dropping every shot at low rates.
  pub fn shots_held(&self) -> usize {
    self.shots.len()
  }

  /// Where to *draw* each player: interpolated between samples, dead reckoned a
  /// little way past the newest, and held after that.
  ///
  /// Rendered a send interval in the past, which is what lets two samples
  /// bracket the target and makes the motion continuous. The local player is
  /// drawn from here too, the same as everyone else: there is no predicted
  /// position to substitute any more.
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
          // The newest sample stands in when the view has nothing at all, which
          // is the join transient rather than a fault.
          .unwrap_or_else(|| self.players[p])
      })
      .collect()
  }

  /// Counts players being drawn off the render instant, once per frame.
  ///
  /// Deliberately not counted inside [`Client::render_players`]. That runs
  /// several times a frame (the camera, the enemy rule's context, the coin
  /// attractors, the claim prediction, the renderer), so counting there
  /// multiplied every occurrence by however many callers happened to ask, and a
  /// join transient of three unknown players read as a thousand faults.
  ///
  /// A player that has never been heard from is not counted: it is not being
  /// drawn at the wrong instant, it is not being drawn at all. What is counted
  /// is a view that *has* history and still cannot reach the instant, which is
  /// the genuine starvation the number exists to report.
  fn count_view_fallbacks(&self) {
    let Some(at) = self.render_at() else { return };
    for (p, view) in self.player_views.iter().enumerate() {
      if !self.knows_player(p) || at.0 < self.player_first_ms[p] {
        continue;
      }
      if view.oldest_timestamp().is_some_and(|oldest| at.0 < oldest) {
        self.view_fallbacks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      }
    }
  }

  /// Times a player was drawn off the render instant because its view could
  /// not reach it. Zero on a healthy link once the timeline has started (the
  /// join transient legitimately counts a few, until the history spans the
  /// render delay); climbing after that means somebody is being shown
  /// off-timeline.
  pub fn view_fallbacks(&self) -> u64 {
    self.view_fallbacks.load(std::sync::atomic::Ordering::Relaxed)
  }

  /// Folds in a player stream frame. See [`Controls::player_sync_hz`] for why
  /// this arrives separately from, and far more often than, the entity stream.
  pub fn on_player_frame(&mut self, frame: &PlayerFrame, recv_ms: u64) {
    self.now_ms = recv_ms;
    for (p, pos) in &frame.players {
      self.observe_player(*p as usize, *pos, frame.server_time_ms);
    }
    // The far tier, at map resolution. It feeds the same views as the near one,
    // so a peer crossing the boundary is interpolated across the change rather
    // than teleporting, and the error being absorbed is one quantisation step
    // plus one interval of movement. The main view never draws these, because
    // it culls at a radius they are by definition outside; the minimap does.
    for (p, x, y) in &frame.distant {
      self.observe_player(*p as usize, dequantize_far(*x, *y), frame.server_time_ms);
    }
    // Steer the render clock, and feed the budget monitor: this is the
    // interpolated stream, so its interval is the one that sizes the delay.
    self.observe_arrival(recv_ms);
    self.arrivals.observe(frame.server_time_ms, recv_ms);
    // Health rides the timeline like everything else. Applying it on arrival
    // put the bar one render delay ahead of the body it is drawn over, so a hit
    // showed before the contact that caused it was visible.
    if !frame.vitals.is_empty() {
      self.health_queue.push((frame.server_time_ms, frame.vitals.clone()));
    }
  }

  /// Applies one health sample, if it is not older than what is already shown.
  ///
  /// Both streams carry health at different rates and both land here, so the
  /// guard is what keeps the slower one from walking the bar backwards: the two
  /// writers still exist on the wire, but they meet a single monotonic clock.
  fn apply_health(&mut self, at_ms: u64, vitals: &[(PlayerId, u8, bool)]) {
    if at_ms < self.health_at_ms {
      return;
    }
    self.health_at_ms = at_ms;
    // Updated in place rather than replacing the array, because this is a
    // subset: a player the server did not mention is one this client cannot
    // see, and its last known bar is the right thing to keep.
    for &(p, health, shield) in vitals {
      let p = p as usize;
      if p < self.player_health.len() {
        self.player_health[p] = health;
        self.player_invuln[p] = shield;
      }
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
    if !self.player_seen[p] {
      self.player_seen[p] = true;
      self.player_first_ms[p] = server_time_ms;
    }
    self.players[p] = pos;
  }

  /// Notes that something arrived, and steers the render clock toward it.
  ///
  /// Both streams feed this. The clock has to track what has *arrived* to decide
  /// how far behind to sit, and the buffer it holds is sized from how irregular
  /// arrivals are rather than from any configured rate, so the server may send at
  /// whatever rate it likes and change it live.
  /// Both streams steer the clock, but only the *player* stream feeds the
  /// budget monitor: it is the interpolated one, so only its interval sizes
  /// the render delay. Fed from both, the merged ~40 ms gap understated the
  /// budget by most of a player interval and the readout blessed a delay the
  /// peers could not actually be bracketed at.
  fn observe_arrival(&mut self, recv_ms: u64) {
    // Tracks the synced server clock (`recv_ms`), not the packet's timestamp:
    // steering by the timestamp puts the estimate one trip behind and makes T
    // depend on latency, which is the conflation this design removes.
    //
    // Full strength is deliberate, and smooth, which is worth stating because
    // it looks like a snap: `recv_ms` is this client's own clock estimate,
    // which advances with frame time, so tracking it exactly tracks a smooth
    // value. A gentler strength was tried while hunting a movement stutter
    // (the stutter was the zero-margin render delay, not this) and it only
    // made the clock lag arrivals by several packets, which is a starved
    // buffer wearing a different hat.
    self.render_clock.resync(recv_ms, 1.0);
  }

  /// Takes delivery of a packet. Does **not** apply it: see [`Client::playout`].
  ///
  /// The render clock is steered here rather than at apply time, because it has
  /// to track what has *arrived* in order to decide how far behind to sit.
  /// Lateness, overflow and the buffering-or-lost decision are the
  /// [`PlayoutBuffer`]'s; what stays here is what a restart means for *this*
  /// client's state.
  pub fn receive_packet(&mut self, packet: Packet, recv_ms: u64) {
    self.now_ms = recv_ms;
    self.observe_arrival(recv_ms);
    let render_at = self.render_at().map(|at| at.server_time_ms());
    let (stamp, seq) = (packet.server_time_ms, packet.seq);
    match self.playout.push(stamp, seq, packet, render_at) {
      Admission::Queued => {}
      Admission::TimelineLost => self.restart_timeline(recv_ms),
    }
  }

  /// The transport's word that this client stopped draining and the gap was
  /// discarded unread: restart, once, deliberately.
  ///
  /// The net client calls this when one poll hands back a resume backlog (a
  /// tab coming out of the background) and it drops everything but the tail.
  /// Without this entry point the same restart still happened, but by the
  /// queue bound tripping repeatedly as the backlog played in, tearing down
  /// each partial rebuild the previous trip had paid for.
  pub fn timeline_lost(&mut self, recv_ms: u64) {
    self.playout.timeline_lost();
    self.restart_timeline(recv_ms);
  }

  /// What a lost timeline means for this client's own state. The queue is the
  /// buffer's business and has already been dropped to its newest packet; what
  /// goes with it here is everything derived from played-out packets, and the
  /// render clock re-anchors on what just arrived.
  ///
  /// Dropping the mirror is what makes the server rebuild it. Its next digest
  /// check finds a client holding nothing where it expected a world, and sends a
  /// full baseline, which is the same path a drifted mirror already takes.
  fn restart_timeline(&mut self, recv_ms: u64) {
    self.enemies.clear();
    self.shots.clear();
    self.health_queue.clear();
    self.render_clock = InterpolationClock::new(self.render_delay_ms);
    self.render_clock.resync(recv_ms, 1.0);
  }

  /// Applies every queued packet whose moment has arrived, oldest first, so
  /// deltas compose in the order the server built them.
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
    let mut applied = false;
    while let Some(packet) = self.playout.pop_due(now) {
      self.apply_packet(&packet, now, controls);
      applied = true;
    }
    applied
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
    let generational = controls.generational_ids;

    // The entity stream still carries the players, so a client is never without
    // them, and both streams feed the same buffer on the same server timeline.
    // Shots arrive as events and are flown locally, so this only folds in what
    // started and what ended early.
    self.shots.extend(packet.shots_fired.iter().copied());
    if !packet.shots_ended.is_empty() {
      self.shots.retain(|s| !packet.shots_ended.contains(&s.id));
    }
    if packet.nova_at_ms.is_some() {
      self.nova_at_ms = packet.nova_at_ms;
    }
    self.crowds.clone_from(&packet.crowds);
    // This packet is being applied because its instant has come, so its health
    // is due now; the guard only stops it undoing a newer player-stream sample.

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
    // A subset, and only what changed, so this updates in place. Replacing the
    // array would blank every player the server had no news about.
    for (id, wallet) in &packet.wallets {
      let id = *id as usize;
      if id < self.wallets.len() {
        self.wallets[id] = wallet.clone();
      }
    }
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
    let drawn = self.drawn_players();
    for (winner, coin) in &packet.claims {
      // Launch the flight from wherever it was last seen: the old list, or the
      // current position of a flight this client had already started on a
      // prediction. Re-aiming an in-flight coin rather than restarting it is what
      // keeps a denied prediction from teleporting. Evaluated against the drawn
      // owner, the same position `flight_positions` flies it toward.
      let from = self
        .flights
        .iter()
        .find(|f| f.id == *coin)
        .map(|f| f.at(drawn[f.to_player as usize % drawn.len()]))
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
      // A death gets an explosion where the client last had the enemy, so the
      // position is read before the removal. The mirror refuses a removal whose
      // generation names an occupant it no longer holds, and counts it: with
      // generations off the key matches and the wrong entity is deleted, which is
      // the whole demonstration.
      let pos = died.then(|| self.enemies.get(handle.into()).map(|e| e.sim.logical().pos)).flatten();
      if self.enemies.remove(handle.into()).is_some() {
        // Counted on the *removal*, not the announcement. Recovery deliberately
        // repeats an announcement until it is acknowledged, and the mirror
        // absorbs the repeats idempotently; a counter that read the wire
        // instead of the state counted one nova's deaths two or three times,
        // an RTT apart, and everything downstream of it (the inferred pulse
        // ring) fired again with each repeat.
        if died {
          self.deaths_seen += 1;
        }
        if let Some(pos) = pos {
          self.bursts.push(Burst { pos, age: 0.0, big: true });
        }
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
    self.count_view_fallbacks();

    // Health samples whose instant has come, oldest first so the guard in
    // `apply_health` sees them in order.
    if let Some(at) = self.render_at() {
      let now = at.server_time_ms();
      self.health_queue.sort_by_key(|(ts, _)| *ts);
      let due = self.health_queue.iter().take_while(|(ts, _)| *ts <= now).count();
      for (ts, vitals) in self.health_queue.drain(..due).collect::<Vec<_>>() {
        self.apply_health(ts, &vitals);
      }
    }

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
      // Attractors at the instant the coins stand at: the drawn players, not
      // the newest array, or a magnet bends coins toward a point ahead of the
      // player it is drawn on.
      let drawn = self.drawn_players();
      let attractors: Vec<(Vec2, f32, f32)> = (0..drawn.len())
        .map(|p| {
          let magnet = if p == self.id as usize {
            self.believed_upgrades.contains(&Upgrade::Magnet)
          } else {
            self.wallets.get(p).is_some_and(|w| w.has(Upgrade::Magnet))
          };
          let (radius, speed) = coin_pull(magnet);
          (drawn[p], radius, speed)
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

    // Announce a difficulty step-up once, at the instant on screen, so the
    // banner lands together with the visibly faster enemies it describes.
    let tier = difficulty(self.frame_clock_ms()).floor() as u32;
    if tier > self.last_difficulty_tier {
      self.last_difficulty_tier = tier;
      self.notices.push((format!("difficulty up  (x{tier})  enemies faster, hits harder"), 0.0));
    }
    // Expiry only. The server owns when a shot ends: it stops listing one that
    // hit something, and the list is replaced wholesale by each packet as it is
    // played out.
    // Expiry is computed, never announced: both sides hold the fire time and the
    // lifetime is a constant, so a message saying so would be paying twice.
    // Dropped once the *render* instant has passed it, not once now has, or a
    // shot would vanish a render delay before it was drawn arriving.
    if let Some(at) = self.render_at() {
      let now = at.server_time_ms();
      self.shots.retain(|shot| !shot.expired(now));
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

  /// Two player samples a second apart, so the drawn position at a target
  /// between them is their midpoint and the newest sample is measurably
  /// elsewhere.
  fn client_with_a_moving_player(render_delay_ms: u64) -> Client {
    let mut client = Client::new(0, 2);
    client.set_render_delay(render_delay_ms);
    let at = |t, x| PlayerFrame {
      server_time_ms: t,
      players: vec![(0, Vec2::new(x, 0.0)), (1, Vec2::new(0.0, 0.0))],
      vitals: vec![],
      distant: vec![],
    };
    client.on_player_frame(&at(1_000, 0.0), 1_000);
    client.on_player_frame(&at(2_000, 1_000.0), 2_000);
    // The delay reaches the clock on the next tick; a zero-length one, so the
    // render instant is exactly `newest - delay` with no frame time added.
    client.tick(0, &Controls::default());
    client
  }

  #[test]
  fn the_enemy_rule_reads_the_players_as_drawn_not_as_newest() {
    // The enemy-aim finding: enemies stand at the render instant, so the
    // context their rule reads has to be the players at that instant. Feeding
    // the newest array made an enemy chase a point up to a render delay ahead
    // of the marker it was visibly chasing.
    let client = client_with_a_moving_player(500);
    assert_eq!(client.render_at().map(|at| at.server_time_ms()), Some(1_500), "the render instant sits between the two samples");
    let aimed_at = client.enemy_ctx().players[0];
    assert!((aimed_at.x - 500.0).abs() < 1.0, "the rule reads the drawn midpoint, got {aimed_at:?}");
    assert!((client.players()[0].x - 1_000.0).abs() < 1.0, "while the newest sample is somewhere else entirely");
  }

  #[test]
  fn a_coin_flight_lands_on_the_marker_the_player_can_see() {
    // A flight is pure presentation, so its target is the drawn player. Aimed
    // at the newest authoritative position, the animation landed ahead of the
    // body it belonged to.
    let mut client = client_with_a_moving_player(500);
    client.flights.push(CoinFlight {
      id: 9,
      from: Vec2::new(0.0, 0.0),
      to_player: 0,
      elapsed_ms: COIN_FLIGHT_MS,
    });
    let landed = client.flight_positions()[0];
    assert!((landed.x - 500.0).abs() < 1.0, "landed on the drawn player, got {landed:?}");
  }

  #[test]
  fn health_changes_at_the_instant_on_screen_not_at_arrival() {
    // Applied on arrival, the bar dropped one render delay before the contact
    // that caused it was visible over the body it is drawn on.
    let mut client = Client::new(0, 2);
    client.set_render_delay(100);
    let hit = PlayerFrame {
      server_time_ms: 1_000,
      players: vec![(0, Vec2::new(50.0, 50.0)), (1, Vec2::new(60.0, 60.0))],
      vitals: vec![(0, 1, false), (1, 5, false)],
      distant: vec![],
    };
    client.on_player_frame(&hit, 1_000);
    let full = PLAYER_MAX_HEALTH as u8;
    assert_eq!(client.player_health, vec![full, full], "the hit's instant has not come yet");
    client.tick(50, &Controls::default());
    assert_eq!(client.player_health, vec![full, full], "still ahead of the render instant");
    client.tick(60, &Controls::default());
    assert_eq!(client.player_health, vec![1, 5], "applied once the render clock reaches it");
  }

  #[test]
  fn the_pulse_phase_is_read_at_the_render_instant() {
    // The frame clock had two writers on two timelines, and the pulse phase
    // wobbled between them. Now it is the same T everything else in the frame
    // is evaluated at: here the newest sample's clock has the pulse off while
    // the render instant has it on, and the render instant wins.
    let mut client = Client::new(0, 2);
    client.set_render_delay(500);
    client.believed_upgrades.push(Upgrade::Repulsor);
    let frame = PlayerFrame {
      server_time_ms: 6_900,
      players: vec![(0, Vec2::new(0.0, 0.0)), (1, Vec2::new(0.0, 0.0))],
      vitals: vec![],
      distant: vec![],
    };
    client.on_player_frame(&frame, 7_550);
    client.tick(0, &Controls::default());
    assert!(repulsor_pulse(6_900).is_none(), "no pulse at the newest sample's clock");
    assert_eq!(client.render_at().map(|at| at.server_time_ms()), Some(7_050), "but the render instant is inside one");
    assert_eq!(client.repel_radius(0), repulsor_pulse(7_050), "and the phase on screen is the render instant's");
  }

  #[test]
  fn a_player_never_heard_from_is_not_counted_as_a_fault() {
    // Being outside relevance is not starvation. A player with no samples is not
    // being drawn at the wrong instant, it is not being drawn at all, and the
    // renderer already skips it. Counting it made the join transient read as a
    // thousand faults on a healthy client.
    let controls = Controls::default();
    let mut client = Client::new(0, 2);
    let frame = PlayerFrame {
      server_time_ms: 1_000,
      players: vec![(0, Vec2::new(50.0, 50.0))],
      vitals: vec![],
      distant: vec![],
    };
    client.on_player_frame(&frame, 1_000);
    for _ in 0..30 {
      client.tick(16, &controls);
    }
    assert!(!client.knows_player(1), "player 1 was never sent");
    assert_eq!(client.view_fallbacks(), 0, "and is not a fault, merely absent");
  }

  #[test]
  fn the_fallback_count_is_per_frame_not_per_reader() {
    // `render_players` runs several times a frame: the camera, the enemy rule's
    // context, the coin attractors, the claim prediction, the renderer. Counting
    // inside it multiplied every occurrence by however many callers happened to
    // ask, so the number reported was a property of the call graph rather than
    // of the timeline.
    let controls = Controls::default();
    let mut client = client_with_a_moving_player(150);
    let before = client.view_fallbacks();
    let at = client.render_at().expect("started");
    for _ in 0..10 {
      let _ = client.render_players(at);
      let _ = client.drawn_players();
    }
    assert_eq!(client.view_fallbacks(), before, "reading does not count");
    client.tick(16, &controls);
    assert!(client.view_fallbacks() - before <= 2, "and a frame counts at most once per player");
  }

  #[test]
  fn a_repeated_death_announcement_is_counted_once() {
    // Recovery deliberately repeats an announcement until it is acknowledged,
    // and the mirror absorbs the repeats idempotently. The death counter did
    // not: it read the wire instead of the state, so one nova's burst was
    // counted again with every repeat, an RTT apart, and the pulse ring
    // inferred from it visibly re-fired.
    use crate::sim::types::Spawn;
    let mut client = Client::new(0, 1);
    let controls = Controls::default();
    let key = SlotKey { index: 3, generation: 1 };

    let mut spawn = Packet { server_time_ms: 100, seq: 1, ..Default::default() };
    spawn.entered.push(Spawn { handle: key.into(), pos: Vec2::new(10.0, 10.0), target: 0, kind: EnemyKind::from_seed(0) });
    client.apply_packet(&spawn, 100, &controls);
    assert_eq!(client.known_entities(), 1);

    let mut death = Packet { server_time_ms: 200, seq: 2, ..Default::default() };
    death.left.push((key.into(), LeaveReason::Died));
    client.apply_packet(&death, 200, &controls);
    assert_eq!(client.deaths_seen, 1, "the death was seen once");

    let mut repeat = Packet { server_time_ms: 260, seq: 3, ..Default::default() };
    repeat.left.push((key.into(), LeaveReason::Died));
    client.apply_packet(&repeat, 260, &controls);
    assert_eq!(client.deaths_seen, 1, "a recovery repeat is the same death, not a second one");
  }

  #[test]
  fn a_player_who_was_never_sent_is_not_drawn() {
    // A player outside this client's relevance still occupies a slot, holding
    // the seed a camera falls back to, which is the middle of the arena.
    // Drawing that is a peer standing in the centre of the map who is not
    // there: the same class of mistake as the corner camera, a default that is
    // right for one use and wrong for another.
    let mut client = Client::new(0, 4);
    let frame = PlayerFrame {
      server_time_ms: 1_000,
      // Only players 0 and 2 are relevant to this client.
      players: vec![(0, Vec2::new(100.0, 100.0)), (2, Vec2::new(140.0, 100.0))],
      vitals: vec![(0, 50, false), (2, 90, true)],
      distant: vec![],
    };
    client.on_player_frame(&frame, 1_000);

    assert!(client.knows_player(0) && client.knows_player(2), "the two that were sent are known");
    assert!(!client.knows_player(1) && !client.knows_player(3), "the two that were not are not");
    for p in [1usize, 3] {
      assert_eq!(
        (client.players()[p].x, client.players()[p].y),
        (crate::sim::types::ARENA_W * 0.5, crate::sim::types::ARENA_H * 0.5),
        "player {p} still holds the seed, which is exactly why it must not be drawn"
      );
    }
    // And the vitals that did arrive landed on the right players.
    client.tick(0, &Controls::default());
    client.tick(200, &Controls::default());
    assert_eq!(client.player_health[0], 50);
    assert_eq!(client.player_health[2], 90);
    assert!(client.player_invuln[2], "the shield rode with the id, not with a position in an array");
  }

  #[test]
  fn a_shot_is_flown_from_its_event_and_ends_when_told() {
    // Shots are events now: an origin, a velocity and a time, evaluated at the
    // render instant. Two things this has to get right, and the second is why
    // the idea was reverted the first time it was tried.
    let controls = Controls::default();
    let mut client = client_with_a_moving_player(500);
    let at = client.render_at().expect("timeline started").server_time_ms();

    let mut packet = Packet { server_time_ms: at, seq: 1, ..Default::default() };
    packet.shots_fired.push(Shot {
      id: 7,
      origin: Vec2::new(0.0, 0.0),
      vel: Vec2::new(100.0, 0.0),
      fired_ms: at.saturating_sub(500),
    });
    client.apply_packet(&packet, at, &controls);

    // Placed by evaluating the rule at the instant being drawn, not carried
    // forward from wherever the last packet happened to say it was.
    let drawn = client.render_projectiles(client.render_at().unwrap());
    assert_eq!(drawn.len(), 1);
    assert!((drawn[0].x - 50.0).abs() < 1.0, "half a second at 100 px/s from the origin: {drawn:?}");

    // An early end, which is the thing a client cannot derive and the reason
    // the first attempt at this flew shots through the enemies they killed.
    let mut ended = Packet { server_time_ms: at, seq: 2, ..Default::default() };
    ended.shots_ended.push(7);
    client.apply_packet(&ended, at, &controls);
    assert!(client.render_projectiles(client.render_at().unwrap()).is_empty(), "a shot that hit something stops being drawn");
  }

  #[test]
  fn a_shot_expires_without_being_told() {
    // Ordinary expiry is computed from the fire time and a constant both sides
    // hold, so saying it on the wire would be paying twice for one fact.
    let controls = Controls::default();
    let mut client = client_with_a_moving_player(500);
    let at = client.render_at().expect("timeline started").server_time_ms();
    let mut packet = Packet { server_time_ms: at, seq: 1, ..Default::default() };
    packet.shots_fired.push(Shot {
      id: 9,
      origin: Vec2::new(0.0, 0.0),
      vel: Vec2::new(100.0, 0.0),
      fired_ms: at,
    });
    client.apply_packet(&packet, at, &controls);
    assert_eq!(client.render_projectiles(client.render_at().unwrap()).len(), 1);

    // Past its lifetime, with nothing arriving to say so.
    client.tick((crate::sim::types::PROJECTILE_TTL * 1000.0) as u64 + 100, &controls);
    assert!(client.render_projectiles(client.render_at().unwrap()).is_empty(), "it expired on its own");
    assert_eq!(client.shots_held(), 0, "and was dropped rather than accumulating for ever");
  }

  #[test]
  fn a_client_that_stalled_restarts_its_timeline_instead_of_queueing_for_ever() {
    // A browser stops running frames for a hidden tab. The socket keeps
    // delivering, so packets arrive describing moments the client's clock has
    // not reached and cannot reach by playing: there is nothing between "a
    // minute ago" and "now" to play through. Queueing them is unbounded growth
    // and a world that never resumes.
    let controls = Controls::default();
    let mut client = client_with_a_moving_player(150);
    let at = client.render_at().expect("timeline started").server_time_ms();

    // The world moves on while this client's clock does not, which is what a
    // stalled frame loop looks like from inside: arrivals are stamped with the
    // client's own estimate of server time, and that estimate is only as fresh
    // as the last frame it ran.
    let stalled_clock = at + 150;
    for step in 1..=60u64 {
      let t = at + step * 1000;
      client.receive_packet(Packet { server_time_ms: t, seq: step, ..Default::default() }, stalled_clock);
    }

    assert!(client.resyncs() > 0, "a minute ahead is a discontinuity, not a buffer");
    assert!(client.playout.len() <= 2, "the queue is dropped rather than played through: {}", client.playout.len());
    assert_eq!(client.known_entities(), 0, "and the mirror goes too, so the server rebuilds it");

    // And it is playing again rather than stuck: the clock is anchored on what
    // just arrived instead of on a moment a minute gone.
    client.tick(16, &controls);
    assert!(client.render_at().is_some(), "the timeline restarted rather than stopping");
  }

  #[test]
  fn the_playout_queue_is_bounded() {
    // Fed by a remote peer and drained by a local clock, so it has to be
    // bounded on its own terms: whatever the reason a client stops draining, it
    // must not accumulate without limit.
    let mut client = client_with_a_moving_player(150);
    let at = client.render_at().expect("timeline started").server_time_ms();
    // All within the lost threshold, so only the count can stop this.
    for seq in 1..=(MAX_QUEUED_PACKETS as u64 * 2) {
      client.receive_packet(Packet { server_time_ms: at + 1, seq, ..Default::default() }, at + 1);
    }
    assert!(client.playout.len() <= MAX_QUEUED_PACKETS + 1, "held {} packets", client.playout.len());
  }

  #[test]
  fn lateness_past_the_discontinuity_threshold_is_not_an_underrun() {
    // A resumed tab's backlog is late by the whole stall. Charging each of
    // those packets as an underrun read one stall as a thousand link faults;
    // the stall is `resyncs`' business, and underruns are the link's.
    let controls = Controls::default();
    let mut client = client_with_a_moving_player(150);

    // Jump the timeline far forward, as the restart after a stall does, so
    // there is room below the render instant for both scales of lateness.
    client.receive_packet(Packet { server_time_ms: 100_000, seq: 50, ..Default::default() }, 100_000);
    client.tick(16, &controls);
    let at = client.render_at().expect("timeline running").server_time_ms();
    let before = client.underruns();

    // Late on the scale jitter produces: an underrun.
    client.receive_packet(Packet { server_time_ms: at - 20, seq: 51, ..Default::default() }, at + 150);
    assert_eq!(client.underruns(), before + 1, "jitter-scale lateness is an underrun");

    // Late by a stall: a discontinuity, not a thousand link faults.
    client.receive_packet(Packet { server_time_ms: at - 60_000, seq: 52, ..Default::default() }, at + 150);
    assert_eq!(client.underruns(), before + 1, "a packet from a lost timeline is not an underrun");
  }

  #[test]
  fn the_pulse_ring_is_a_pure_function_of_the_declared_timestamp() {
    // The ring used to be inferred from a burst of deaths per tick, and the
    // inference re-fired on every recovery repeat of the same announcements.
    // Declared, there is nothing to trigger: the age is the frame clock minus
    // the timestamp the packet carries, so a repeat is the same ring and the
    // fade needs no local timer to decay.
    let mut client = client_with_a_moving_player(500);
    let controls = Controls::default();
    let mut nova = Packet { server_time_ms: 1_400, seq: 1, nova_at_ms: Some(1_400), ..Default::default() };
    client.apply_packet(&nova, 1_400, &controls);
    let age = client.nova_flash_age().expect("the ring is live at the render instant");
    assert!((age - 0.1).abs() < 0.02, "the age is T minus the declared instant: {age}");

    nova.seq = 2;
    client.apply_packet(&nova, 1_450, &controls);
    let same = client.nova_flash_age().expect("still live");
    assert!((same - age).abs() < 1e-6, "a recovery repeat is the same ring, not a restart");

    client.tick(500, &controls);
    assert!(client.nova_flash_age().is_none(), "and it fades because the clock passed it, not because a timer ran down");
  }

  #[test]
  fn the_marker_stays_on_the_timeline_at_the_deepest_render_delay() {
    // The detached-marker bug, found by playing at 600 ms. The history buffer
    // held ~200 ms, so past that the view clamped every render to the oldest
    // snapshot it still had: the marker rode near now while the shots were
    // faithfully at T, and the weapon appeared to fire from the player's past.
    // The marker was the wrong half of that picture.
    let mut client = Client::new(0, 1);
    client.set_render_delay(crate::sim::types::RENDER_DELAY_MAX_MS);
    // Three seconds of 30 Hz samples of a player moving at a steady 0.19 px/ms.
    let mut t = 0;
    while t <= 3_000 {
      let frame = PlayerFrame {
        server_time_ms: t,
        players: vec![(0, Vec2::new(t as f32 * 0.19, 0.0))],
        vitals: vec![],
        distant: vec![],
      };
      client.on_player_frame(&frame, t);
      t += 33;
    }
    client.tick(0, &Controls::default());
    let at = client.render_at().expect("timeline started");
    let expected = at.server_time_ms() as f32 * 0.19;
    let drawn = client.render_players(at)[0];
    assert!((drawn.x - expected).abs() < 7.0, "the marker sits at T ({expected:.0}), not at the edge of a too-short history: {drawn:?}");
    assert_eq!(client.view_fallbacks(), 0, "and the history reached, so nothing was drawn off-timeline");
  }

  #[test]
  fn a_marker_drawn_past_its_history_is_counted() {
    // The degradation stays (something must be drawn), but it says so: the
    // clamp inside the view is exactly the silent compensation that made the
    // detached marker invisible to every readout.
    let mut client = Client::new(0, 1);
    client.set_render_delay(crate::sim::types::RENDER_DELAY_MAX_MS);
    // A client that has been running a while, whose buffer has evicted its way
    // to a history shorter than the render delay. That is the failure: not "it
    // has just joined", which cannot cover the instant either and is expected,
    // but "it has been here throughout and still cannot".
    let mut t = 0u64;
    while t <= 1_000 {
      let frame = PlayerFrame {
        server_time_ms: t,
        players: vec![(0, Vec2::new(t as f32, 0.0))],
        vitals: vec![],
        distant: vec![],
      };
      client.on_player_frame(&frame, t);
      t += 5;
    }
    client.tick(0, &Controls::default());
    let at = client.render_at().expect("timeline started");
    assert!(at.server_time_ms() >= client.player_first_ms[0], "the instant is inside this client's lifetime");
    assert!(client.view_fallbacks() > 0, "a clamped render is counted, not silent");
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
