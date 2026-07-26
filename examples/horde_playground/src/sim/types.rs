//! The shared vocabulary: geometry, the entities, the behaviour rules both sides
//! run, the packets that cross the wire, and the byte accounting that makes
//! bandwidth claims measurable rather than asserted.

use plaza_client_utils::extrapolation::Extrapolatable;
use plaza_client_utils::interpolation::Interpolatable;
use plaza_client_utils::SlotKey;
use serde::{Deserialize, Serialize};

pub const ARENA_W: f32 = 3000.0;
pub const ARENA_H: f32 = 3000.0;

/// How far a player can see. Entities outside this are irrelevant to them.
pub const VIEW_RADIUS: f32 = 420.0;

/// Relevance grid cell, sized near the view so a query sweeps a few cells.
pub const CELL_SIZE: f32 = 256.0;

pub const PLAYER_SPEED: f32 = 190.0;
pub const ENEMY_SPEED: f32 = 62.0;

/// Local simulation rate, on both the server and every client.
pub const SIM_HZ: u32 = 60;
pub const SIM_DT: f32 = 1.0 / SIM_HZ as f32;

/// The deepest render delay the panel can ask for. One constant, because two
/// things must agree on it: the slider's range, and how much player history a
/// client keeps. When they were sized independently, the slider could ask for
/// an instant the history no longer held, and the view silently clamped to the
/// oldest snapshot it had: the marker detached from the timeline everything
/// else was drawn on, and shots visibly left from "a point in the past", which
/// was in fact the only correctly placed thing on screen.
pub const RENDER_DELAY_MAX_MS: u64 = 600;
/// The fastest either stream can be asked to send, for the same reason: the
/// history buffer must cover [`RENDER_DELAY_MAX_MS`] at this rate.
pub const SEND_RATE_MAX_HZ: u32 = 60;

// Combat.
pub const FIRE_INTERVAL_MS: u64 = 220;
pub const PROJECTILE_SPEED: f32 = 620.0;
pub const PROJECTILE_TTL: f32 = 1.4;
pub const HIT_RADIUS: f32 = 14.0;
/// The area-of-effect nova each player periodically emits: the mass-kill event
/// that produces a despawn burst. Its damage is enough to clear the swarm but
/// not a brute, so a pulse thins the crowd and leaves the heavies walking.
pub const NOVA_INTERVAL_MS: u64 = 4500;
pub const NOVA_RADIUS: f32 = 190.0;
pub const NOVA_DAMAGE: u8 = 3;
/// How long the pulse ring is drawn for, everywhere a pulse is drawn: the
/// clients, the offline world and the host view all derive the ring from a
/// timestamp and this one duration.
pub const NOVA_RING_SECS: f32 = 0.45;
/// New enemies arrive in waves, just outside somebody's view.
pub const WAVE_INTERVAL_MS: u64 = 500;

// The player as a target rather than an invulnerable camera.
/// Full health. Kept small and integer so it rides the wire as one byte per
/// player and a bar is easy to read.
pub const PLAYER_MAX_HEALTH: f32 = 100.0;
/// How close an enemy has to be to a player to be doing damage.
pub const PLAYER_CONTACT_RADIUS: f32 = 22.0;
/// Damage from a single hit, before difficulty scaling. Damage is discrete, not a
/// continuous drain: touching an enemy costs one hit, and then a brief window of
/// invulnerability before the next, so a whole pile lands one hit per window
/// rather than one per tick and cannot destroy you in a single instant.
pub const CONTACT_HIT_DAMAGE: f32 = 9.0;
/// The invulnerability a hit buys, long enough that a swarm cannot chain.
pub const HIT_INVULN_MS: u64 = 600;
/// The longer invulnerability a respawn buys, enough to walk out of the pile that
/// got you. Drawn as a shield; the brief hit window is not.
pub const PLAYER_INVULN_MS: u64 = 2000;

/// The survivor-style difficulty ramp, as a multiplier that grows with elapsed
/// time. Deliberately scaled in **minutes**, so the short headless tests and the
/// bandwidth case study, which run for seconds, see it at ~1.0 and are unchanged.
///
/// Derived from the clock, like [`repulsor_pulse`], so both the server and every
/// client compute the same value against their own estimate of server time. It is
/// therefore a third consumer of clock sync: a client whose clock is off ramps to
/// a slightly different difficulty and its enemies move at a subtly wrong speed.
pub fn difficulty(now_ms: u64) -> f32 {
  let minutes = now_ms as f32 / 60_000.0;
  (1.0 + minutes * 0.6).min(8.0)
}

/// How much faster enemies move at the current difficulty. Gentler than the
/// damage ramp, because speed is the most *felt* difficulty and a small change
/// reads as a large one.
pub fn enemy_speed_scale(now_ms: u64) -> f32 {
  1.0 + (difficulty(now_ms) - 1.0) * 0.10
}

/// A dense entity index, the slot an entity occupies.
pub type EntityIndex = u32;
pub type PlayerId = u8;
/// The most players an arena will seat: the ceiling on
/// [`Controls::player_count`], not the count itself.
///
/// A player is a *viewer*, so it costs a relevance query and a packet of its own
/// every send round, and a pass over the live enemies in both `fire_weapons` and
/// `nova`, making the server `O(players * enemies)`. At 3000 enemies,
/// `examples/players.rs` reports:
///
/// | players | server CPU per simulated second | downstream |
/// |---|---|---|
/// | 4 | 6 ms | 136 KiB/s |
/// | 128 | 75 ms | 607 KiB/s |
///
/// Bandwidth used to be what bounded this, at 3.9 MiB/s for that second row,
/// because everything per-player went to everybody: positions and vitals on both
/// streams and every wallet in every packet, all `O(players^2)` and 81% of the
/// total. Relevance now applies to players as well as enemies, so the term is
/// flat and CPU is the honest limit again.
///
/// Re-run it before moving this. The hard limit above it is the wire, where
/// `PlayerId` is a `u8`.
pub const MAX_PLAYERS: usize = 128;
/// The count an arena runs with unless told otherwise, and what every
/// measurement in the README was taken at.
pub const DEFAULT_PLAYERS: usize = 4;

/// So a remote player can be smoothed with [`RemoteView`], which is what it is
/// for. Both are the obvious implementations; they exist here rather than in the
/// library because a wire type should not have to adopt somebody's vector.
///
/// [`RemoteView`]: plaza_client_utils::RemoteView
impl Interpolatable<u64> for Vec2 {
  fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
    Vec2::new(self.x + (other.x - self.x) * t, self.y + (other.y - self.y) * t)
  }
}

impl Extrapolatable<Vec2, f32> for Vec2 {
  fn extrapolate_with_velocity(&self, velocity: &Vec2, dt: f32) -> Self {
    Vec2::new(self.x + velocity.x * dt, self.y + velocity.y * dt)
  }
}

/// A handle to an entity: which slot, and which *occupant* of that slot.
///
/// The generation is what makes a recycled slot detectable. Without it, a packet
/// still in flight that refers to slot 5 will be applied to whatever now lives in
/// slot 5, which is the whole reason this type is not just an index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Handle {
  pub idx: EntityIndex,
  pub generation: u16,
}

impl Handle {
  pub fn new(idx: EntityIndex, generation: u16) -> Self {
    Self { idx, generation }
  }
  /// The key a client files this under. With generations disabled the generation
  /// is discarded, which is exactly how stale references become corruption.
  pub fn key(self, generational: bool) -> Handle {
    if generational { self } else { Handle { idx: self.idx, generation: 0 } }
  }
}

/// The wire handle and the library's key are the same pair, so this is a
/// widening and never a decision.
///
/// Worth having as a conversion rather than an open-coded shift at each call
/// site: the client's mirror and the server's baseline both key on
/// `SlotKey::encode`, and two hand-written packings that agree today are a
/// disagreement waiting to happen. It would present as a digest mismatch over a
/// world both sides actually hold identically, which is a genuinely bad afternoon.
impl From<Handle> for SlotKey {
  fn from(handle: Handle) -> Self {
    SlotKey::new(handle.idx as u32, handle.generation)
  }
}

impl From<&Handle> for SlotKey {
  fn from(handle: &Handle) -> Self {
    (*handle).into()
  }
}

impl From<SlotKey> for Handle {
  fn from(key: SlotKey) -> Self {
    Handle::new(key.index as EntityIndex, key.generation)
  }
}

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

/// What kind of enemy this is. **Static for its whole life**, which is why it
/// rides the spawn message once (one byte) and never appears in a correction.
/// Size, speed, and toughness all follow from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnemyKind {
  /// The horde: small, ordinary speed, dies to anything.
  Swarm,
  /// Fast and fragile, reaches you first.
  Runner,
  /// Big and slow, and survives an area pulse.
  Brute,
}

impl EnemyKind {
  pub fn speed(self) -> f32 {
    match self {
      EnemyKind::Swarm => ENEMY_SPEED,
      EnemyKind::Runner => ENEMY_SPEED * 1.8,
      EnemyKind::Brute => ENEMY_SPEED * 0.55,
    }
  }

  /// Draw radius, and the radius a shot has to reach.
  pub fn radius(self) -> f32 {
    match self {
      EnemyKind::Swarm => 4.0,
      EnemyKind::Runner => 3.4,
      EnemyKind::Brute => 9.5,
    }
  }

  pub fn max_health(self) -> u8 {
    match self {
      EnemyKind::Swarm => 1,
      EnemyKind::Runner => 1,
      EnemyKind::Brute => 5,
    }
  }

  /// A cheap deterministic mix: mostly swarm, some runners, a few brutes.
  pub fn from_seed(seed: u32) -> Self {
    match seed.wrapping_mul(2_654_435_761) % 100 {
      0..=74 => EnemyKind::Swarm,
      75..=93 => EnemyKind::Runner,
      _ => EnemyKind::Brute,
    }
  }
}

/// One enemy. `target` is which player it chases: the *intent*, which changes
/// rarely and is synced only when it does. `health` is **server-side only**, see
/// the note on [`Spawn`].
#[derive(Clone, Copy, Debug)]
pub struct Enemy {
  pub pos: Vec2,
  pub target: PlayerId,
  pub kind: EnemyKind,
  pub health: u8,
}

/// A shot in flight. Few enough to send outright.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Projectile {
  pub id: ShotId,
  pub pos: Vec2,
  /// Where it started. Kept so an early end can be sent to exactly the clients
  /// that were told it started, rather than to everybody.
  pub origin: Vec2,
  pub vel: Vec2,
  pub ttl: f32,
}

/// A shot's identity, so an early end can name it.
pub type ShotId = u32;

/// A shot as an **event**: where it started, how fast, and when.
///
/// Everything a client needs to place it at any instant, which is what lets it
/// live on the same delayed timeline as everything else. The alternative, a
/// position re-sent every packet, cannot be evaluated at an instant between
/// packets and costs an entry per packet for the whole flight.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shot {
  pub id: ShotId,
  pub origin: Vec2,
  pub vel: Vec2,
  pub fired_ms: u64,
}

impl Shot {
  /// Where this shot is at `at_ms`, on the server clock.
  ///
  /// The shared rule, in the only form a shot needs: constant velocity from a
  /// known origin is exact at any instant, so both sides agree without either
  /// of them sending a position.
  pub fn at(&self, at_ms: u64) -> Vec2 {
    let age = at_ms.saturating_sub(self.fired_ms) as f32 / 1000.0;
    Vec2::new(self.origin.x + self.vel.x * age, self.origin.y + self.vel.y * age)
  }

  /// Whether it has outlived [`PROJECTILE_TTL`] by `at_ms`. Derived, never sent:
  /// both sides hold the fire time and the lifetime is a constant.
  pub fn expired(&self, at_ms: u64) -> bool {
    at_ms.saturating_sub(self.fired_ms) as f32 / 1000.0 >= PROJECTILE_TTL
  }
}

/// **The shared movement rule for a player.** The server integrates a held
/// direction every tick, and a client predicting its own player runs exactly
/// this, so the two cannot disagree.
///
/// Sharing it is the point. This rule lived in two places for a while, the
/// server's `step` and the client's local prediction, and every divergence bug
/// in this example was in an entity whose rule was written twice rather than
/// called twice. A player is *unforced*, nothing pushes it but its own input, so
/// this really is the whole of it and a client running it is exact.
pub fn step_player(pos: &mut Vec2, dir: Vec2, dt: f32) {
  pos.x = (pos.x + dir.x * PLAYER_SPEED * dt).clamp(0.0, ARENA_W);
  pos.y = (pos.y + dir.y * PLAYER_SPEED * dt).clamp(0.0, ARENA_H);
}

/// **The shared behaviour rule.** Both the authoritative server and every client
/// run exactly this, which is what lets a client simulate an enemy forward in the
/// *present* instead of rendering it interpolated in the past.
pub fn step_enemy(enemy: &mut Enemy, target_pos: Vec2, repel_radius: Option<f32>, speed_scale: f32, dt: f32) {
  let (dx, dy) = (target_pos.x - enemy.pos.x, target_pos.y - enemy.pos.y);
  let len = (dx * dx + dy * dy).sqrt();
  if len > 1.0 {
    // Scaled by the difficulty ramp, which both sides evaluate against the clock.
    let speed = enemy.kind.speed() * speed_scale;
    // A repulsor *pulses*. Enemies inside the pulse are pushed out, weakly, and
    // only for as long as it lasts.
    //
    // The first version was a permanent aura with a hard sign flip at a fixed
    // radius, and it produced a perfect motionless ring of enemies at exactly
    // that radius. Not a bug in the netcode, an equilibrium in the rule: step
    // inward and you are pushed out, step outward and you are pulled in, so
    // everything converges there and stops. It also made the player invulnerable
    // and quietly flattered every accuracy readout, because stationary entities
    // are trivially easy to predict.
    //
    // Pulsing removes the equilibrium rather than softening it. Between pulses
    // there is no outward force at all, so nothing can settle at a radius, and
    // the push being weaker than the chase means a pulse buys distance rather
    // than a wall.
    //
    // This is still the coupling the coin feature exists for: whether a player
    // owns the upgrade is a *discrete, purchased* fact feeding the rule every
    // client runs locally, so a mispredicted purchase simulates the wrong world
    // rather than merely displaying the wrong number.
    let repelled = repel_radius.is_some_and(|r| len < r);
    let (sign, scale) = if repelled { (-1.0, REPULSOR_STRENGTH) } else { (1.0, 1.0) };
    enemy.pos.x += (dx / len) * speed * scale * dt * sign;
    enemy.pos.y += (dy / len) * speed * scale * dt * sign;
  }
}

/// The repulsor fires this often, for this long.
pub const REPULSOR_INTERVAL_MS: u64 = 7_000;
pub const REPULSOR_PULSE_MS: u64 = 700;
/// A pulse reaches somewhere in this range, chosen per pulse.
pub const REPULSOR_MIN_RADIUS: f32 = 28.0;
pub const REPULSOR_MAX_RADIUS: f32 = 95.0;
/// How hard it pushes, as a fraction of an enemy's chase speed. Below 1 on
/// purpose: a pulse should buy you room, not a wall.
pub const REPULSOR_STRENGTH: f32 = 0.6;

/// The active pulse radius at `now_ms`, or `None` between pulses.
///
/// The radius is random per pulse and **derived**, not sampled. Both the server
/// and every client evaluate this same function against their own clock, so a
/// shared rule with a random parameter stays a shared rule. Drawing from a local
/// RNG instead would give each side a different radius, and the two simulations
/// would diverge for reasons no correction stream could explain.
///
/// It also makes the pulse a second consumer of clock sync: the phase depends on
/// agreeing what time it is, so a client whose clock estimate is off pulses at
/// the wrong moment and its enemies scatter when the server's do not.
pub fn repulsor_pulse(now_ms: u64) -> Option<f32> {
  if now_ms % REPULSOR_INTERVAL_MS >= REPULSOR_PULSE_MS {
    return None;
  }
  // splitmix64 finalizer over the pulse index: cheap, and identical everywhere.
  let mut x = (now_ms / REPULSOR_INTERVAL_MS).wrapping_mul(0x9E37_79B9_7F4A_7C15);
  x ^= x >> 33;
  x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
  x ^= x >> 33;
  let t = (x % 1_000) as f32 / 1_000.0;
  Some(REPULSOR_MIN_RADIUS + t * (REPULSOR_MAX_RADIUS - REPULSOR_MIN_RADIUS))
}

pub type CoinId = u32;

/// Currency, dropped where an enemy died.
///
/// Deliberately currency and not score. A score is monotonic and write-only, so
/// a client that briefly believes the wrong number is harmlessly corrected by the
/// next packet. A balance you *spend* has neither property: drift up and a
/// purchase that looked affordable fails, drift down and the player is being
/// denied money they earned, and neither resolves on its own because the error
/// only surfaces at the transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Coin {
  pub id: CoinId,
  pub pos: Vec2,
  /// When it dropped, so an uncollected coin expires rather than silting up the
  /// field. Server-side only; a client infers nothing from it.
  pub spawned_ms: u64,
}

/// How close a player must be to claim a coin. **Nearest player wins**, which is
/// what makes a pickup contested rather than first-come.
///
/// It is also what makes misprediction natural rather than contrived: a client
/// judges "am I nearest?" against remote player positions that are a latency out
/// of date, so two players converging on one coin will sometimes both conclude
/// they won.
pub const COIN_PICKUP_RADIUS: f32 = 46.0;
/// Share of kills that drop a coin.
pub const COIN_DROP_IN: u32 = 6;
/// A coin left uncollected eventually disappears, so the field does not silt up.
pub const COIN_TTL_MS: u64 = 12_000;

/// What currency buys. Both change a rule the client runs locally, which is the
/// point: an upgrade that only changed a server-side number could not corrupt a
/// client's simulation, and corrupting it is the behaviour worth demonstrating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Upgrade {
  /// Enemies within [`REPULSOR_RADIUS`] flee the owner instead of chasing.
  Repulsor,
  /// Coins drift toward the owner, widening the effective pickup radius.
  Magnet,
}

impl Upgrade {
  pub fn cost(self) -> u32 {
    match self {
      Upgrade::Repulsor => 25,
      Upgrade::Magnet => 10,
    }
  }
  pub fn label(self) -> &'static str {
    match self {
      Upgrade::Repulsor => "repulsor",
      Upgrade::Magnet => "magnet",
    }
  }
  pub const ALL: [Upgrade; 2] = [Upgrade::Repulsor, Upgrade::Magnet];
}

/// How long a collected coin takes to reach the player it was awarded to.
///
/// A **fixed duration**, not a fixed speed, which is why the flight cannot be a
/// constant-velocity move: at constant speed the arrival time would grow with the
/// distance, and a coin taken from the far edge of the pickup radius would lag
/// behind one taken from under your feet. Fixed time against a target that is
/// itself moving means interpolating afresh every frame rather than following a
/// path computed once.
///
/// Long enough to be watchable. The first version ran for 320 ms with a cubic
/// ease-in and read as no motion at all: nineteen frames, of which the first ten
/// covered 15% of the distance and the last nine covered the rest. An
/// acceleration nobody can see is indistinguishable from a teleport, which is
/// what it looked like.
pub const COIN_FLIGHT_MS: f32 = 900.0;

/// Every player draws coins in a little, upgrade or not.
///
/// The base reach is tuned against [`NOVA_RADIUS`], because that is what
/// *produces* the coins: an area attack that kills out to 190px and a pickup rule
/// that reaches 46 is a source four times wider than its sink. Measured before
/// this existed, 35% of all coins expired where they fell and an average of 112
/// sat on the ground at any moment, which made the magnet upgrade compulsory
/// rather than optional. An upgrade should widen a rule that already works.
pub const COIN_ATTRACT_RADIUS: f32 = 200.0;
/// Above [`PLAYER_SPEED`] on purpose. A pull slower than a player is one you
/// outrun: the coin falls behind, leaves the radius, and stops where it was
/// abandoned. Measured at 115 against a player speed of 190, that alone left 18%
/// of coins expiring where they fell.
pub const COIN_ATTRACT_SPEED: f32 = 235.0;
/// What the magnet upgrade widens it to. Mostly *reach*, since the base pull
/// already keeps up: an upgrade should extend a rule that works rather than
/// rescue one that does not.
pub const MAGNET_RADIUS: f32 = 400.0;
pub const MAGNET_SPEED: f32 = 300.0;

/// The one shared rule for coin motion, run by the server and by every client.
///
/// Takes each player's reach and pull rather than a list of magnet owners, so the
/// upgrade is a change of degree inside one rule instead of a second rule that
/// only some players run.
pub fn step_coin(coin: &mut Coin, attractors: &[(Vec2, f32, f32)], dt: f32) {
  let mut nearest: Option<(f32, Vec2, f32)> = None;
  for (owner, radius, speed) in attractors {
    let d = coin.pos.dist(*owner);
    if d < *radius && nearest.is_none_or(|(best, _, _)| d < best) {
      nearest = Some((d, *owner, *speed));
    }
  }
  if let Some((d, owner, speed)) = nearest
    && d > 1.0
  {
    coin.pos.x += (owner.x - coin.pos.x) / d * speed * dt;
    coin.pos.y += (owner.y - coin.pos.y) / d * speed * dt;
  }
}

/// One player's coin pull, given whether they own the magnet.
pub fn coin_pull(has_magnet: bool) -> (f32, f32) {
  if has_magnet { (MAGNET_RADIUS, MAGNET_SPEED) } else { (COIN_ATTRACT_RADIUS, COIN_ATTRACT_SPEED) }
}

/// A newly-relevant enemy: enough for a client to start simulating it.
///
/// Carries the **kind**, because a client cannot run the behaviour rule without
/// it (speed comes from the kind) and because it never changes, so one byte here
/// replaces it ever appearing in a correction.
///
/// It deliberately does **not** carry health. Health only changes when the server
/// decides a hit landed, and a client needs to know the *outcome* (this enemy
/// died) rather than the running total. Streaming health for thousands of
/// entities would spend the whole budget on a number the player infers from the
/// enemy still being alive.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Spawn {
  pub handle: Handle,
  pub pos: Vec2,
  pub target: PlayerId,
  pub kind: EnemyKind,
}

/// A periodic authoritative correction.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Sample {
  pub handle: Handle,
  pub pos: Vec2,
  /// Sent only when it changed: the intent, not the output.
  pub target: Option<PlayerId>,
}

/// Why an entity left a client's view. Deaths are worth distinguishing because
/// they arrive in bursts and a client may want to play an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaveReason {
  OutOfRange,
  Died,
}

/// A stand-in for a group of enemies too far away to be worth sending
/// individually: where they are, as a crowd, and how many.
///
/// This is the third answer to "what does this client need to know about that
/// entity?", after sending it and dropping it. Relevance culling is binary, so
/// beyond the view radius a client knows nothing at all and any map it draws is
/// either blank or a lie borrowed from the server. A summary costs six bytes for
/// an arbitrary number of enemies and is enough to draw a crowd.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Crowd {
  pub pos: Vec2,
  pub count: u16,
}

/// The player stream: a handful of entities, sent far more often than the
/// entity stream because everything else is computed *from* them.
///
/// Deliberately its own message rather than a field on [`Packet`]. The entity
/// stream carries a sequence number, an acknowledgement window and a digest,
/// and all three describe the delta-compressed enemy set; numbering these would
/// put packets carrying no deltas into that machinery for nothing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlayerFrame {
  pub server_time_ms: u64,
  /// **Only the players this recipient needs**, which is what keeps the player
  /// stream from being the dominant cost of a large arena.
  ///
  /// It used to be everybody, to everybody, on this stream *and* inside every
  /// entity packet. That is `O(players^2)`, and measured at 128 players it was
  /// 81% of all downstream traffic while the three thousand enemies, which do
  /// get relevance, were 9%. The one thing this example never applied its own
  /// technique to was the thing that dominated it.
  ///
  /// A player is needed here if this recipient can see them, or if an enemy the
  /// recipient holds is chasing them: `step_enemy` aims at a player, so a target
  /// the client cannot place is a rule it cannot run.
  pub players: Vec<(PlayerId, Vec2)>,
  /// Health and shield for the same set, paired with the id rather than
  /// positional, because the set is a subset now.
  pub vitals: Vec<(PlayerId, u8, bool)>,
}

impl PlayerFrame {
  /// Roughly what this costs on the wire: a timestamp, then an id and a
  /// quantized position each, plus an id, a health byte and a shield bit.
  pub fn bytes(&self) -> usize {
    8 + self.players.len() * (1 + POS_BYTES) + self.vitals.len() * 2
  }
}

/// Everything the server sends downstream, so one impaired link carries both
/// streams and they arrive interleaved exactly as they would on a real socket.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Downstream {
  Frame(Box<Packet>),
  Players(PlayerFrame),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Packet {
  pub server_time_ms: u64,
  /// This packet's sequence number, so a client can say which ones landed.
  pub seq: u64,
  /// The sequence this packet's deltas were computed against, or `None` for a
  /// diff from nothing.
  ///
  /// A client that does not hold this baseline **cannot apply the packet at
  /// all**, and must discard it rather than apply the deltas to whatever state it
  /// happens to have. Getting this wrong is subtle and silent: acknowledging a
  /// packet you received is not the same as acknowledging the state it implies,
  /// and a server that believes the difference will diff against a baseline the
  /// client never reached.
  pub baseline_seq: Option<u64>,
  /// This packet is a clean rebuild: the client must drop its whole mirror before
  /// applying, so the full visible set that follows lands exactly rather than
  /// merged onto a state that had drifted. Set when the server forces a resync,
  /// either because the baseline aged out of history or an acknowledged digest
  /// proved the mirror wrong.
  pub full_baseline: bool,
  pub entered: Vec<Spawn>,
  pub left: Vec<(Handle, LeaveReason)>,
  pub samples: Vec<Sample>,
  /// When the most recent area pulse fired, on the server clock. An event
  /// declared outright rather than inferred from the death burst it causes: the
  /// inference re-fired on recovery repeats, and a mid-pulse joiner had nothing
  /// to infer from at all. The ring is a pure function of this timestamp and
  /// the render instant, so repeats are naturally idempotent.
  #[serde(default)]
  pub nova_at_ms: Option<u64>,
  /// Shots that **started** near this player since the last packet.
  ///
  /// An event, not a live set. The set was re-sent in full every packet for the
  /// whole 1.4 s flight, which is one entry in roughly twenty packets per shot,
  /// and it is sending the output of an equation both sides can solve: a shot is
  /// an origin, a velocity and a time, and a client can evaluate that at any
  /// instant exactly.
  ///
  /// This was tried once and reverted, for two stated reasons. The first was
  /// real and is fixed by [`Packet::shots_ended`]: without it a shot flies on
  /// through the enemy it killed. The second, that the client draws shots in the
  /// past while its enemy mirror holds the present, stopped being true when the
  /// whole scene moved to one render instant, and it is why this is worth
  /// revisiting rather than a decision being re-litigated.
  #[serde(default)]
  pub shots_fired: Vec<Shot>,
  /// Shots that ended **early**, because they hit something.
  ///
  /// Expiry is not in here: a client can compute that from the fire time and the
  /// fixed lifetime, so saying it again would be paying twice for a fact both
  /// sides already hold. Only a hit is information the client cannot derive.
  #[serde(default)]
  pub shots_ended: Vec<ShotId>,
  /// An order-independent digest of exactly what this client should hold once it
  /// has applied this packet. Eight bytes that turn a silent mirror divergence
  /// into a detected one.
  pub visible_digest: u64,
  /// The server's exact visible key set (`(idx << 16) | generation`), populated
  /// only under [`Controls::debug_digest`]. Lets a client that sees the digest
  /// disagree name the precise entities it holds wrongly rather than only tally
  /// them. Empty on the normal wire.
  pub debug_keys: Vec<u64>,
  /// Stand-ins for the enemies outside this client's view radius. Empty unless
  /// crowd LOD is on.
  pub crowds: Vec<Crowd>,
  /// Coins near this player, sent outright: there are few and they are the thing
  /// a race is fought over, so partial knowledge would be worse than the bytes.
  pub coins: Vec<Coin>,
  /// Wallets this recipient needs and does not already have: the relevant
  /// players' balances and upgrades, sent **when they change** rather than in
  /// every packet.
  ///
  /// Paired with the id because it is a subset twice over. A wallet is a rule
  /// input, not just a readout (the repulsor and the magnet both come from it),
  /// so it is needed for any player whose upgrades could reach an enemy this
  /// client holds, and it changes only on a pickup or a purchase. Re-sending
  /// every wallet in every packet was three bytes per player per packet, which
  /// at 128 players was a third of the per-player traffic and none of it new.
  pub wallets: Vec<(PlayerId, Wallet)>,
  /// Purchases this recipient asked for and did not get.
  ///
  /// Told rather than inferred, for the same reason claims are. A client that
  /// optimistically showed the player an upgrade needs to know it was refused;
  /// waiting for a timeout would leave it simulating the wrong world for longer
  /// than the truth took to arrive.
  pub denied_buys: Vec<Upgrade>,
  /// Who won each coin claimed since the last packet.
  ///
  /// Announced explicitly rather than inferred from the coin's absence, for the
  /// same reason enemy deaths are: a client that predicted a pickup has to be
  /// able to tell "you lost that one" from "you never saw it", and an absence
  /// says neither.
  pub claims: Vec<(PlayerId, CoinId)>,
  /// Weapon hits near this player since the last packet: where, and how much
  /// damage, so a client can float a fading number like a bullet-heaven does.
  ///
  /// Sent outright, and only for shots (not the mass nova), for the same reason
  /// coins and projectiles are: they are few and near, and unlike enemy *health*
  /// (which is never streamed for thousands of entities) a hit is a discrete
  /// event the client cannot infer from a position sample.
  pub hits: Vec<(Vec2, u8)>,
}

/// One player's currency and what they have bought, as the server sees it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Wallet {
  pub balance: u32,
  pub upgrades: Vec<Upgrade>,
}

impl Wallet {
  pub fn has(&self, upgrade: Upgrade) -> bool {
    self.upgrades.contains(&upgrade)
  }
}

/// What a client sends up: an acknowledgement, and the one request it is allowed
/// to make.
#[derive(Clone, Copy, Debug)]
pub enum ClientMsg {
  /// `digest` is the client's view of its own mirror, so the server can catch a
  /// mirror that has silently drifted from the state it acknowledges.
  Ack { newest: u64, mask: u64, digest: u64 },
  /// A purchase *request*. Naming it a request rather than a purchase is the
  /// whole protocol: the client proposes, and only the server can spend.
  Buy(Upgrade),
}

// Byte accounting. Approximate but consistent, so comparisons are meaningful.
/// A compact handle: `u16` slot index + `u8` generation.
pub const ID_BYTES: usize = 3;
/// A position quantized to two `u16`s over the arena extent.
pub const POS_BYTES: usize = 4;
/// Id, position, target, and the one static byte of kind.
pub const SPAWN_BYTES: usize = ID_BYTES + POS_BYTES + 2;
/// Bytes a LEB128-style varint takes.
fn varint_len(v: u32) -> usize {
  match v {
    0..=127 => 1,
    128..=16_383 => 2,
    16_384..=2_097_151 => 3,
    _ => 4,
  }
}

/// What a sorted set of dense ids costs as varint deltas.
///
/// Measured against the real despawn bursts as the best of four candidates: 55%
/// under three-bytes-per-id overall and 67% on a mass-death burst, beating both a
/// flat presence bitmask (which pays for the whole id space on every packet) and
/// run-length encoding (which needs runs, and slot recycling scatters ids badly
/// enough that a 233-id burst is 204 runs).
fn delta_varint_bytes(mut ids: Vec<u32>) -> usize {
  ids.sort_unstable();
  ids.dedup();
  let mut total = varint_len(ids.len() as u32); // a count prefix
  let mut prev = 0u32;
  for id in ids {
    total += varint_len(id - prev);
    prev = id;
  }
  total
}
pub const SAMPLE_BYTES: usize = ID_BYTES + POS_BYTES;
/// An acknowledgement travelling back up: a sequence number, the 64-bit mask,
/// and the mirror's own digest so the server can catch a drift it cannot see.
pub const ACK_BYTES: usize = 2 + 8 + 8;
/// A crowd stand-in: a quantized position and a count.
pub const CROWD_BYTES: usize = POS_BYTES + 2;
pub const PROJECTILE_BYTES: usize = POS_BYTES + POS_BYTES;
/// A fire event: an id, a quantized origin and velocity, and a fire time as a
/// small offset from the packet's own timestamp.
pub const SHOT_BYTES: usize = ID_BYTES + POS_BYTES + POS_BYTES + 2;
/// What the same data costs with a 16-byte UUID and two `f32`s.
pub const NAIVE_ID_BYTES: usize = 16;
pub const NAIVE_POS_BYTES: usize = 8;

impl Packet {
  /// Despawns as two sorted id sets (one per reason) in varint deltas, which is
  /// what a real implementation would put on the wire.
  ///
  /// The generation is deliberately not carried here. Ordered delivery plus an
  /// explicit death announcement makes it redundant (measured: zero stale handle
  /// references across hundreds of kills), and [`Packet::visible_digest`] is the
  /// backstop if that assumption ever stops holding.
  fn despawn_bytes(&self) -> usize {
    let died: Vec<u32> = self.left.iter().filter(|(_, r)| *r == LeaveReason::Died).map(|(h, _)| h.idx).collect();
    let ranged: Vec<u32> = self.left.iter().filter(|(_, r)| *r == LeaveReason::OutOfRange).map(|(h, _)| h.idx).collect();
    delta_varint_bytes(died) + delta_varint_bytes(ranged)
  }

  /// Where the bytes actually go: (samples, spawns, despawns, shots, per-player).
  /// Worth having, because it is easy to optimise a stream that turns out to be a
  /// rounding error of the packet, and because the reverse happened here: the
  /// per-player slot was the whole story at scale and nobody had looked.
  pub fn bytes_breakdown(&self) -> [usize; 5] {
    [
      self.samples.len() * SAMPLE_BYTES,
      self.entered.len() * SPAWN_BYTES,
      self.despawn_bytes(),
      self.shot_bytes(),
      self.wallets.len() * (1 + 3) + 10,
    ]
  }

  /// A fire event is an id, an origin, a velocity and a fire time; an early end
  /// is just the id.
  fn shot_bytes(&self) -> usize {
    self.shots_fired.len() * SHOT_BYTES + self.shots_ended.len() * ID_BYTES
  }

  pub fn bytes(&self) -> usize {
    self.entered.len() * SPAWN_BYTES
      + self.despawn_bytes()
      + self.samples.len() * SAMPLE_BYTES
      + self.shot_bytes()
      + self.crowds.len() * CROWD_BYTES
      + self.coins.len() * (ID_BYTES + POS_BYTES)
      // An id, a balance, and a bitmask of upgrades, for the wallets that
      // actually changed.
      + self.wallets.len() * (1 + 3)
      + self.claims.len() * (1 + ID_BYTES)
      + self.denied_buys.len()
      // A quantized position and a damage byte per hit.
      + self.hits.len() * (POS_BYTES + 1)
      + 8 // the digest
      + 2 // the sequence number, delta coded
  }

  pub fn naive_bytes(&self) -> usize {
    let per = NAIVE_ID_BYTES + NAIVE_POS_BYTES;
    self.entered.len() * (per + 1)
      + self.left.len() * NAIVE_ID_BYTES
      + self.samples.len() * per
      + self.shots_fired.len() * (NAIVE_POS_BYTES * 2)
  }
}

/// How a remote enemy is drawn on the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteMode {
  /// Run the shared behaviour rule locally every frame, correcting from samples.
  /// Renders in the *present*.
  Simulate,
  /// Dead-reckon along the last known velocity between samples.
  DeadReckon,
  /// Interpolate between the last two samples: accurate, but a send interval behind.
  Interpolate,
}

#[derive(Clone, Copy, Debug)]
pub struct Controls {
  pub latency_ms: u64,
  pub jitter_ms: u64,
  /// Packets dropped on the way down.
  ///
  /// Worth its own note, because adding this slider is what exposed the flaw the
  /// rest of this example had been carrying. A delta-relevance stream assumes
  /// every packet arrives: the server diffs against what it *last sent*, so one
  /// dropped packet leaves the client permanently missing whatever that packet
  /// carried, and nothing in the stream ever mentions it again.
  pub loss_pct: f32,
  /// Diff against the last packet the client *acknowledged* rather than the last
  /// one sent, so a dropped packet's contents are re-derived by the next diff.
  pub ack_recovery: bool,
  /// Coins drop, are contested by proximity, and buy upgrades.
  pub coins: bool,
  /// Predict your own balance and pickups locally instead of waiting for the
  /// server to confirm them.
  ///
  /// Off by default, and that default is the recommendation rather than an
  /// oversight. Nobody is frame-sensitive about a counter, so the honest design
  /// is to show the coin vanish immediately (a local cosmetic) and let the number
  /// arrive a round trip later. Turning this on makes the number instant and buys
  /// a correction that cannot be eased: you cannot smoothly un-collect a coin.
  pub predict_balance: bool,
  /// Buy whatever is affordable, so the purchase path runs without a human.
  pub auto_buy: bool,
  /// Opening angle for crowd level of detail: enemies outside the view radius are
  /// summarised into stand-ins rather than dropped entirely. Zero is off, and
  /// off is what relevance culling alone gives you: nothing at all out there.
  pub crowd_lod_theta: f32,
  /// How often the **entity** stream goes out: enemies entering, leaving and
  /// being resampled. The expensive one, and the one relevance and crowd LOD
  /// exist to make cheap.
  pub sync_hz: u32,
  /// How often the **player** stream goes out, separately and much faster.
  ///
  /// Two knobs rather than one because they answer different questions, and
  /// collapsing them is what makes a low entity rate look far worse than it is.
  /// Enemy positions can be stale, because every client runs the enemies' own
  /// rule locally and only needs correcting. Player positions cannot, because
  /// they are the **input** to that rule: an enemy aims at where it thinks a
  /// player is, so a stale player position turns into a whole horde changing
  /// heading at once, every time the stream ticks. Players are also a handful of
  /// entities rather than thousands, so sending them often is nearly free.
  ///
  /// This is the case study's own principle applied honestly: sync the input to
  /// the behaviour, not just the behaviour's output.
  pub player_sync_hz: u32,
  /// How long the server holds an input before executing it, in ms.
  ///
  /// The playout buffer, and the reason it exists is fairness rather than
  /// smoothness. Applied on arrival, an input from a 20 ms player lands on the
  /// tick after they pressed it and one from a 200 ms player lands nine ticks
  /// later, so any outcome decided by who was where first (a contested pickup)
  /// is decided by ping. Scheduling every input to execute at the moment it was
  /// *pressed* plus this delay puts everyone on the same footing, as long as the
  /// delay covers their latency.
  ///
  /// It is not free: it is added to how long the world takes to react to you.
  /// Prediction hides it for your own movement and cannot hide it for anything
  /// the server adjudicates.
  pub playout_delay_ms: u64,
  /// How far behind the server's clock every client displays the world.
  ///
  /// A property of the timeline, not of anybody's link: the same instant is on
  /// every screen, so the server can reason about what a client has yet to play.
  /// It must cover `one_way + jitter + one send interval`, because the newest
  /// sample a client holds is already a trip old; short of that, T sits ahead of
  /// every sample and peers snap to the raw newest instead of interpolating.
  ///
  /// It used to be sized from measured arrival jitter, which let the transport
  /// decide which moment was on screen and hid a bad link by showing that player
  /// an older world. Now a link too slow for the declared delay produces
  /// [`Client::underruns`](crate::sim::client::Client::underruns) instead.
  pub render_delay_ms: u64,
  /// How many ticks late an input may be named for and still be accepted.
  ///
  /// The accepting window, and a setting rather than a constant because it is a
  /// genre decision. Tight is what a competitive shooter wants: a closed tick
  /// stays closed, and a player who cannot reach the window loses inputs and
  /// rubber-bands. Loose forgives a jittery link at the cost of letting a
  /// slightly stale input take effect. Widening it is also what a lag switch
  /// wants, so it should be sized from what honest links actually do.
  pub input_max_late_ticks: u64,
  /// How many ticks ahead of the server's current tick an input may be named for.
  ///
  /// Has to cover the playout depth, since that is exactly how far ahead an
  /// honest client aims. Beyond it, a client is parking inputs in the future.
  pub input_max_early_ticks: u64,
  /// Whether to use the playout buffer at all. Off is the naive behaviour,
  /// apply-on-arrival, kept so the difference is measurable rather than argued.
  pub input_playout: bool,
  pub relevance: bool,
  pub mode: RemoteMode,
  pub smooth: bool,
  pub spread_players: bool,
  /// How many players the world has, bots filling whatever nobody occupies.
  ///
  /// **Structural**, like `enemy_count`: enemies aim at a player index and every
  /// player owns a relevance stream, so changing it rebuilds the world and
  /// reseats everyone rather than being absorbed live. Capped at
  /// [`MAX_PLAYERS`].
  pub player_count: usize,
  pub enemy_count: usize,
  /// Whether handles carry a generation. Turn it off to watch a recycled slot be
  /// corrupted by a packet that was already in flight.
  pub generational_ids: bool,
  /// Auto-firing weapons and the periodic nova.
  pub combat: bool,
  /// Send a movement input only when the direction changes (plus a low-rate
  /// keepalive), instead of one every tick.
  ///
  /// Off is the simple, loss-robust default: a fresh input every tick means a
  /// dropped one is covered by the next, and the prediction stays in lock-step
  /// with the server. On cuts the idle upstream chatter to near nothing, which is
  /// safe here only because the local player has no server-side forces, so the
  /// client predicts it exactly and a coalesced stream cannot drift the position.
  pub coalesce_input: bool,
  /// Draw where everything is **going** to be, faintly, ahead of where it is.
  ///
  /// The solid marker is the actual position: the server's resolved state at the
  /// render instant, played out of the buffer. The ghost is the future the client
  /// already holds but has not reached, so the gap is the playout delay rather
  /// than an error.
  ///
  /// The drawing half only. Whether a ghost can exist is
  /// [`ServerPolicy::allow_ghost`](crate::sim::protocol::ServerPolicy::allow_ghost).
  pub show_ghost: bool,
  /// Server side: send frames ahead of the instant a client is rendering, which
  /// is what makes a ghost possible at all. See
  /// [`ServerPolicy::allow_ghost`](crate::sim::protocol::ServerPolicy::allow_ghost).
  pub allow_ghost: bool,
  /// Ship the server's exact visible key set on every frame so a client that
  /// detects a digest mismatch can print precisely which enemies it holds in
  /// error (extra) or is short of (missing), and log every prediction correction,
  /// instead of only counting them. Off by default: it adds real wire weight. Turn
  /// it on from the panel to chase a mismatch or a jump down to specifics.
  pub debug_digest: bool,
}

impl Default for Controls {
  fn default() -> Self {
    Self {
      latency_ms: 80,
      jitter_ms: 20,
      loss_pct: 0.0,
      ack_recovery: true,
      crowd_lod_theta: 0.0,
      coins: true,
      predict_balance: false,
      auto_buy: true,
      sync_hz: 16,
      // Above the entity rate on purpose: the players are few, and every enemy
      // in the world aims at one of them, so their positions are the input to
      // the rule every client runs.
      player_sync_hz: 8,
      playout_delay_ms: 100,
      // one_way (80) + jitter (20) + an 8 Hz player interval (125), rounded up.
      // A whole send interval is part of the budget because interpolation needs
      // two samples bracketing the target, so lowering `player_sync_hz` without
      // raising this draws every peer, and your own marker, off the timeline.
      // Checked by `the_shipped_defaults_cover_each_other`.
      render_delay_ms: 250,
      input_playout: true,
      // Roughly the playout depth in 16 ms steps, plus slack for jitter.
      input_max_late_ticks: 4,
      input_max_early_ticks: 10,
      relevance: true,
      mode: RemoteMode::Simulate,
      smooth: true,
      spread_players: true,
      player_count: DEFAULT_PLAYERS,
      enemy_count: 3000,
      generational_ids: true,
      combat: true,
      coalesce_input: false,
      show_ghost: true,
      allow_ghost: true,
      debug_digest: false,
    }
  }
}

impl Controls {
  pub fn sync_interval_ms(&self) -> u64 {
    (1000 / self.sync_hz.max(1)) as u64
  }

  pub fn player_sync_interval_ms(&self) -> u64 {
    (1000 / self.player_sync_hz.max(1)) as u64
  }
}
