//! The authoritative arena, as `plaza` core wants it: one `StateType` that owns
//! everything mutable, and one stateless `StateLogic` that acts on it.
//!
//! The horde server was already shaped for this. Its `advance_seats` is a tick
//! function, it already produces per-recipient packets, and it already consumes
//! acknowledgements and purchase requests as separate upward messages. What this
//! module adds is the part a function argument stood in for: seats that fill and
//! empty as people arrive, and inputs (movement, acks, buys) that arrive
//! *between* ticks rather than with them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{RateMeter, SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::server::{Seat, Server};
use crate::sim::types::{
  Coin, Controls, Downstream, EnemyKind, Handle, PlayerId, Projectile, Vec2, Wallet, CROWD_BYTES, MAX_PLAYERS,
};

/// How a connection is identified. Assigned by the server on accept, never
/// supplied by the client.
pub type PlayerKey = u64;

/// The seat count the panel is asking for, in range. A zero-seat arena serves
/// nobody, and the ceiling is [`MAX_PLAYERS`].
///
/// Live, but **structural**: enemies aim at a player index and every player owns
/// a relevance stream, so a change rebuilds the world and reseats everyone
/// through [`Arena::reconfigure`] rather than being absorbed in place.
fn requested_seats(controls: &Controls) -> usize {
  controls.player_count.clamp(1, MAX_PLAYERS)
}

/// How many round trips the transport must have measured before a decision.
/// One sample on a jittery link decides nothing, and the transport probes fast
/// at first, so this is about a second.
/// The arena's step, in ms, for turning the late window into a time budget.
const SIM_STEP_MS: u64 = (crate::sim::types::SIM_DT * 1000.0) as u64;

/// How many round trips the transport must have measured before a decision.
/// One sample on a jittery link decides nothing, and the transport probes fast
/// at first, so this is about a second.
const ADMIT_SAMPLES: u64 = 8;

/// One connection waiting on the transport's measurements before it is offered a
/// seat. It holds nothing: the samples live in the transport, which is what took
/// them.
#[derive(Clone, Debug, Default)]
struct Admission;

/// The seed for the impairment jitter. Fixed, so a host that drags the jitter
/// slider gets the same distribution every run.
const IMPAIR_SEED: u64 = 0x8177_1E55;

/// Everything the omniscient half of a host or observer needs, published by the
/// arena and read by the UI and renderer.
///
/// The host is the server *and* a client in one process, so it legitimately has
/// both sides: the authoritative truth here, and its own believed state in its
/// [`NetClient`]. This is the truth half, cloned into a shared slot once per send
/// round rather than per tick.
///
/// [`NetClient`]: crate::net::client::NetClient
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub players: Vec<Vec2>,
  /// Every live enemy, for the faint truth overlay and for the cross-side
  /// readouts (render error, phantoms, omissions) a joiner cannot compute.
  pub truth: Vec<(Handle, Vec2, EnemyKind)>,
  pub projectiles: Vec<Projectile>,
  pub coins: Vec<Coin>,
  pub wallets: Vec<Wallet>,
  pub coins_claimed: Vec<u32>,
  pub player_health: Vec<u8>,
  pub player_invuln: Vec<bool>,
  pub player_deaths: Vec<u64>,
  pub difficulty: f32,

  pub alive: usize,
  pub kills: u64,
  pub nova_kills_last: usize,
  pub last_nova_ms: Option<u64>,
  /// Handles that died within the deepest render delay, with when. A client
  /// drawing the past legitimately still holds these, so a check for a drifted
  /// mirror has to exclude them or it reports the render delay as a fault.
  pub recently_dead: Vec<(Handle, u64)>,
  pub server_now_ms: u64,
  pub coins_expired: u64,
  pub denied_purchases: u64,
  pub full_resends: u64,
  /// Seats currently throttled because they stopped acknowledging: a hidden
  /// tab, a stalled machine. Zero on a healthy arena.
  pub stalled_seats: usize,

  bytes: RateMeter,
  naive_bytes: RateMeter,
  crowd_bytes: RateMeter,
  relevant: RateMeter,
  spawns: RateMeter,
  despawns: RateMeter,
}

impl HostView {
  pub fn bytes_per_sec(&self) -> f64 {
    self.bytes.per_sec()
  }
  /// The same traffic averaged over the whole life of the meter. Shown beside
  /// the current rate rather than instead of it, because the two answer
  /// different questions and the difference between them is itself a reading:
  /// a session mean below the current rate is still climbing toward it.
  pub fn lifetime_bytes_per_sec(&self) -> f64 {
    self.bytes.lifetime_per_sec()
  }
  /// What the same world would have cost sent the obvious way. The comparison is
  /// the example's whole claim, so it is measured rather than argued.
  pub fn naive_bytes_per_sec(&self) -> f64 {
    self.naive_bytes.per_sec()
  }
  pub fn crowd_bytes_per_sec(&self) -> f64 {
    self.crowd_bytes.per_sec()
  }
  pub fn mean_relevant(&self) -> f64 {
    self.relevant.mean()
  }
  pub fn mean_spawns_per_packet(&self) -> f64 {
    self.spawns.mean()
  }
  pub fn mean_despawns_per_packet(&self) -> f64 {
    self.despawns.mean()
  }
  /// How long ago the last area pulse fired, in seconds, while still worth
  /// drawing. Computed at publish, which is fine for an observer without a clock.
  pub fn nova_flash_age(&self) -> Option<f32> {
    let fired = self.last_nova_ms?;
    let age = self.server_now_ms.saturating_sub(fired) as f32 / 1000.0;
    (age <= crate::sim::types::NOVA_RING_SECS).then_some(age)
  }
}

/// The outbound impairment for one connection: frames held for `latency ± jitter`
/// before they are released, so the host's latency and jitter sliders act on a
/// real link instead of a simulation.
///
/// [`LatencyLink`] defaults to ordered delivery, which is what this needs and
/// why it is used directly rather than reimplemented here. A WebSocket is
/// TCP: frames arrive in the order they were sent and jitter shows up as bursty
/// delay, not shuffling. Letting a jittered frame overtake an earlier one would
/// model a UDP link this transport is not, and it would desync the delta stream,
/// whose recovery is built for loss and not reordering.
type Downlink = LatencyLink<Downstream>;

/// Everything the arena owns. `plaza` requires `Clone` for its state-query
/// command; nothing on the hot path clones it.
#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  /// Which player each connection is driving.
  seats: SeatTable<PlayerKey>,
  /// The newest movement direction for each seat, applied on the next tick.
  pending: Vec<Seat>,
  /// The newest input sequence accepted per player, echoed back so a client can
  /// replay only what the server has not applied.
  input_acked: HashMap<PlayerKey, u64>,

  down: HashMap<PlayerKey, Downlink>,
  /// The inbound half of the impaired link, one per connection.
  ///
  /// A client's outbound traffic was already being *dropped* per the loss
  /// slider, but never delayed, so inputs and acknowledgements arrived
  /// instantly however far latency was dragged. That made the link asymmetric
  /// in a way nothing on screen admitted: a round trip read as one direction,
  /// and the input schedule, whose entire job is to cover a player's one way
  /// delay, was being tested against a path that had none.
  up: HashMap<PlayerKey, LatencyLink<Op>>,
  /// Connections being measured. They hold no seat while this runs, so a slow
  /// joiner cannot park one and make the arena look full.
  admitting: HashMap<PlayerKey, Admission>,
  rng: Rng,
  /// The last whole second a trace line was emitted for, so `HORDE_TRACE`
  /// prints one row per second rather than one per packet.
  traced_second: u64,


  /// What is actually going out, which is what makes the relevance claim
  /// checkable rather than asserted. `naive` is the counterfactual: the same
  /// world sent the obvious way.
  bytes: RateMeter,
  naive_bytes: RateMeter,
  crowd_bytes: RateMeter,
  /// Entities in a packet, spawns in a packet, despawns in a packet. Each counts
  /// every packet, including the empty ones, or the averages only describe the
  /// interesting frames.
  relevant: RateMeter,
  spawns: RateMeter,
  despawns: RateMeter,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let seats = requested_seats(&controls);
    let sim = Server::new(controls.enemy_count, seats, controls.spread_players);
    Self {
      sim,
      controls,
      seats: SeatTable::new(seats),
      pending: vec![Seat::Bot; seats],
      input_acked: HashMap::new(),
      down: HashMap::new(),
      up: HashMap::new(),
      admitting: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
      traced_second: 0,
      bytes: RateMeter::new(),
      naive_bytes: RateMeter::new(),
      crowd_bytes: RateMeter::new(),
      relevant: RateMeter::new(),
      spawns: RateMeter::new(),
      despawns: RateMeter::new(),
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    ServerPolicy {
      sync_hz: self.controls.sync_hz,
      playout_delay_ms: self.controls.playout_delay_ms,
      render_delay_ms: self.controls.render_delay_ms,
      player_sync_hz: self.controls.player_sync_hz,
      allow_ghost: self.controls.allow_ghost,
      coins: self.controls.coins,
      generational_ids: self.controls.generational_ids,
      crowd_lod_theta: self.controls.crowd_lod_theta,
      relevance: self.controls.relevance,
      enemy_count: self.controls.enemy_count,
      player_count: self.sim.players.len(),
    }
  }

  /// The most one-way delay this arena can carry, derived rather than declared.
  ///
  /// An input is named for `press + playout_delay` and rejected once it lands
  /// more than `input_max_late_ticks` past it, so this *is* the condition that
  /// breaks a player. A separate constant would drift out of step with the
  /// sliders and start admitting people who then cannot move.
  pub fn admission_budget_ms(&self) -> u64 {
    self.controls.playout_delay_ms + self.controls.input_max_late_ticks * SIM_STEP_MS
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.seat_of(key)
  }

  /// Seats a joiner, or refuses when the arena is full.
  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    let seating = self.seats.seat(key);
    if let Seating::Fresh(seat) = seating {
      self.pending[seat] = Seat::Bot;
      // The server has been advancing this seat's relevance baseline since
      // startup, occupied or not. Clear it so this fresh client's first frame is
      // a full dump rather than a delta against a world it never received. This
      // is the whole reason `seat` reports freshness instead of an index: a
      // rejoin must *not* do this, and the two are indistinguishable otherwise.
      self.sim.reset_seat(seat);
    }
    seating.index()
  }

  fn unseat(&mut self, key: &PlayerKey) {
    // A connection that leaves mid-measurement leaves nothing behind, or the
    // arena keeps probing an address nobody is at.
    self.admitting.remove(key);
    if let Some(seat) = self.seats.unseat(key) {
      // Whatever they had scheduled goes with them, or a bot inherits a direction
      // its former occupant pressed.
      self.sim.clear_input(seat);
      // Handed back to the bots rather than left frozen, so a disconnect does not
      // leave a statue in the arena.
      self.pending[seat] = Seat::Bot;
    }
    self.input_acked.remove(key);
    self.down.remove(key);
    self.up.remove(key);
  }

  /// Applies one client op whose uplink delay has expired.
  ///
  /// Returns a reply for the ops that have one. Split out of the receive path so
  /// the uplink can hold traffic: what a client sent and what the arena acts on
  /// are now separated by the same delay the downstream has.
  fn apply_client_op(&mut self, key: PlayerKey, op: Op) -> Option<TargetedOp<Op, PlayerKey>> {
    let seat = self.seat_of(&key)?;
    match op {
      Op::Input { seq, dx, dy, tick } => {
        // Out-of-order *arrivals* are still dropped, because a duplicate or a
        // straggler carries nothing new. Out-of-order execution is a different
        // matter and is the buffer's job: the schedule is keyed on when the
        // player pressed, not on when this turned up.
        if self.input_acked.get(&key).is_some_and(|newest| seq <= *newest) {
          return None;
        }
        self.input_acked.insert(key, seq);
        // Normalised here, not on the client: a client that sent a longer
        // vector would simply move faster.
        let len = (dx * dx + dy * dy).sqrt();
        let dir = if len > 1.0 { Vec2::new(dx / len, dy / len) } else { Vec2::new(dx, dy) };
        let controls = self.controls;
        self.sim.submit_input(seat, tick, dir, &controls);
        None
      }
      // The entity stream's acknowledgement and the one purchase a client may
      // request. Both go straight to the server, which is the only thing allowed
      // to move a baseline or spend a coin.
      Op::Ack { newest, mask, digest } => {
        self.sim.receive_ack(seat, newest, mask, digest);
        None
      }
      Op::Buy(upgrade) => {
        self.sim.receive_buy(seat, upgrade);
        None
      }
      Op::Ping { origin_ms } => {
        let server_ms = self.sim.now_ms();
        Some(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]))
      }
      // A client announcing a wire format that is not this one cannot be
      // reasoned with, only told. Said once, on the client's own first message,
      // instead of the per-op decode warnings a mismatch otherwise produces for
      // as long as it stays connected.
      Op::Hello { protocol } if protocol != PROTOCOL => {
        tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
        Some(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]))
      }
      // Server-to-client variants coming up mean a confused or hostile client;
      // not an error worth failing the tick over.
      _ => None,
    }
  }

  /// How many seats the arena will actually run: what the panel asked for, but
  /// **never fewer than the people already in them**.
  ///
  /// Lowering the count is a decision about how full the arena may get, not
  /// permission to throw somebody out of a game they are playing. So the request
  /// takes effect as players leave, and until then the arena stays as large as
  /// it has to be. The host sees this: the effective count is written back to
  /// the panel, so the slider springs back rather than showing a number the
  /// world is not running.
  fn seat_target(&self, controls: &Controls) -> usize {
    requested_seats(controls).max(self.seats.occupied_count())
  }

  /// Rebuilds the world for a new enemy count or player layout, reseating whoever
  /// is connected and returning the seats that need a fresh `Welcome`.
  fn reconfigure(&mut self, controls: Controls) -> Vec<(PlayerKey, usize)> {
    let clock = self.sim.now_ms();
    self.controls = controls;
    let seats = self.seat_target(&controls);
    self.sim = Server::new(controls.enemy_count, seats, controls.spread_players);
    // Keep time continuous across the rebuild, so a client's packet-age estimate
    // does not jump and fling the horde at the player.
    self.sim.set_clock(clock);
    self.pending = vec![Seat::Bot; seats];
    self.input_acked.clear();
    self.down.clear();
    // The rates are over the current world, not every world since launch.
    for meter in self.meters() {
      meter.reset();
    }

    self.seats.reseat_all(seats)
  }

  /// Every counter, for the operations that apply to all of them at once.
  fn meters(&mut self) -> [&mut RateMeter; 6] {
    [
      &mut self.bytes,
      &mut self.naive_bytes,
      &mut self.crowd_bytes,
      &mut self.relevant,
      &mut self.spawns,
      &mut self.despawns,
    ]
  }

  fn host_view(&self) -> HostView {
    HostView {
      players: self.sim.players.clone(),
      truth: self.sim.live_enemies().map(|(h, e)| (h, e.pos, e.kind)).collect(),
      projectiles: self.sim.projectiles.clone(),
      coins: self.sim.coins.clone(),
      wallets: self.sim.wallets.clone(),
      coins_claimed: self.sim.coins_claimed.clone(),
      player_health: (0..self.sim.players.len()).map(|p| self.sim.player_health(p)).collect(),
      player_invuln: (0..self.sim.players.len()).map(|p| self.sim.is_player_invuln(p)).collect(),
      player_deaths: self.sim.player_deaths.clone(),
      difficulty: self.sim.difficulty(),
      alive: self.sim.alive_count(),
      kills: self.sim.kills,
      nova_kills_last: self.sim.nova_kills_last,
      last_nova_ms: self.sim.last_nova_ms,
      recently_dead: self.sim.recently_dead_log(),
      server_now_ms: self.sim.now_ms(),
      coins_expired: self.sim.coins_expired,
      denied_purchases: self.sim.denied_purchases,
      full_resends: self.sim.full_resends(),
      stalled_seats: self.sim.stalled_seats(),
      bytes: self.bytes,
      naive_bytes: self.naive_bytes,
      crowd_bytes: self.crowd_bytes,
      relevant: self.relevant,
      spawns: self.spawns,
      despawns: self.despawns,
    }
  }
}

/// The stateless half plaza acts through. Carries the shared control slot the
/// host's panel writes and the arena reads, and the optional view the arena
/// publishes for a windowed host to draw.
/// Where the arena gets a connection's measured latency from.
///
/// The **transport** measures it, by timing its own WebSocket ping, so no
/// application message is involved and a client cannot understate it. This is
/// just the arena's way of asking, and it is a closure rather than a session
/// handle so the logic stays testable without a socket.
///
/// Returns the minimum round trip seen and how many samples it rests on.
pub type LatencySource = Arc<dyn Fn(&PlayerKey) -> Option<(Duration, u64)> + Send + Sync>;

/// Where a connection that does not fit *this* arena should go instead.
///
/// Takes a measured one-way delay and returns the room that can carry it, or
/// `None` when nothing can. Refusal is the `None` case rather than the primary
/// behaviour, which is the whole reason placement is worth wiring at all.
pub type Router = Arc<dyn Fn(u32) -> Option<(u32, String, String)> + Send + Sync>;

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  latency: Option<LatencySource>,
  router: Option<Router>,
  /// Which room this arena *is*, so it can tell whether a placement is somewhere
  /// else or right here.
  room: u32,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      latency: None,
      router: None,
      room: 0,
    }
  }

  /// Where to send a connection this arena cannot carry.
  pub fn with_router(mut self, room: u32, router: Router) -> Self {
    self.room = room;
    self.router = Some(router);
    self
  }

  /// Supplies the transport's measurements, without which nothing is ever
  /// admitted: an arena that cannot measure a connection must not guess at it.
  pub fn with_latency(mut self, latency: LatencySource) -> Self {
    self.latency = Some(latency);
    self
  }
}

#[async_trait]
impl StateLogic<Op, PlayerKey, Arena> for ArenaLogic {
  async fn process_input(&self, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> Result<LogicOutput<Op, PlayerKey>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(key) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        // Measured before seated. A connection that cannot meet the input
        // schedule used to be welcomed and then have every input silently
        // rejected, which reads as a broken game rather than a refused one.
        state.admitting.insert(key, Admission);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentLeft { agent_id } => {
        state.unseat(&agent_id);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        if state.seat_of(&key).is_none() {
          return Ok(LogicOutput::none());
        }
        // Held, not applied. The uplink is impaired here rather than at the
        // client, because a joiner sends over a real socket and has no reason
        // to sabotage itself, and the panel that owns these sliders is on the
        // host. Ops come back out on the tick their delay expires.
        //
        // Control plane rides the same delay but never the loss: dropping a
        // version handshake makes a diagnostic flaky without teaching anything
        // about the netcode.
        let now = state.sim.now_ms();
        let (latency, jitter, loss) = (state.controls.latency_ms, state.controls.jitter_ms, state.controls.loss_pct);
        for op in ops {
          let droppable = matches!(op, Op::Input { .. } | Op::Ack { .. } | Op::Buy(_));
          let entry = state.up.entry(key).or_default();
          entry.send(now, op, latency, jitter, if droppable { loss } else { 0.0 }, &mut state.rng);
        }
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
        // Pick up whatever the host's panel changed. A structural change (enemy
        // count or player layout) rebuilds the world and re-welcomes everyone;
        // anything else is a live edit the next tick simply reads.
        let mut live = *self.controls.lock();
        // A lowered player count cannot evict anyone already playing, so it is
        // held at the number of occupied seats and written back, which is what
        // makes the slider spring back instead of reading as a promise the
        // arena is quietly refusing to keep.
        let target = state.seat_target(&live);
        if target != live.player_count {
          live.player_count = target;
          self.controls.lock().player_count = target;
        }
        let mut welcomes = Vec::new();

        // Whatever the uplink has finished holding, applied before the world
        // moves, so an input still lands on the tick it named when it is early
        // enough and is refused when the delay pushed it past the window. That
        // second case is the one the schedule exists for and was previously
        // unreachable from the panel.
        let now_in = state.sim.now_ms();
        let keys: Vec<PlayerKey> = state.up.keys().copied().collect();
        for key in keys {
          let due: Vec<Op> = state.up.get_mut(&key).map(|link| link.drain_due(now_in)).unwrap_or_default();
          for op in due {
            if let Some(reply) = state.apply_client_op(key, op) {
              welcomes.push(reply);
            }
          }
        }

        // Admission: ask the transport what it has measured, and decide whoever
        // it has measured enough of. Runs before seating so a decision made this
        // tick takes its seat this tick.
        //
        // The arena sends no probes of its own. The transport times its own
        // WebSocket ping, so this costs the wire format nothing and a client
        // cannot report a latency it does not have.
        let budget = state.admission_budget_ms();
        let mut decided: Vec<(PlayerKey, u64)> = Vec::new();
        for key in state.admitting.keys().copied().collect::<Vec<_>>() {
          let Some(source) = self.latency.as_ref() else {
            // Nothing is measuring, so nothing is admitted. An arena that cannot
            // measure a connection must not guess that it is fine.
            continue;
          };
          if let Some((min_rtt, samples)) = source(&key)
            && samples >= ADMIT_SAMPLES
          {
            decided.push((key, min_rtt.as_millis() as u64 / 2));
          }
        }
        for (key, estimate) in decided {
          state.admitting.remove(&key);
          // Somewhere else that can carry this link, before refusing it.
          if estimate > budget
            && let Some(router) = self.router.as_ref()
            && let Some((room, name, endpoint)) = router(estimate as u32)
            && room != self.room
          {
            tracing::info!(key, estimate_ms = estimate, room, "placing a connection in an arena that can carry it");
            welcomes.push(TargetedOp::new_system_to(
              key,
              vec![Op::Placed {
                room,
                name,
                endpoint,
                measured_ms: estimate as u32,
              }],
            ));
            continue;
          }
          if estimate > budget {
            tracing::info!(key, estimate_ms = estimate, budget_ms = budget, "refusing a connection that cannot meet the input schedule");
            welcomes.push(TargetedOp::new_system_to(
              key,
              vec![Op::Refused {
                measured_ms: estimate as u32,
                allowed_ms: budget as u32,
              }],
            ));
            continue;
          }
          if let Some(seat) = state.seat(key) {
            welcomes.push(TargetedOp::new_system_to(key, vec![Op::Welcome { player: seat as PlayerId, policy: state.policy() }]));
          } else {
            // Full. Said outright, because a seatless connection receives no
            // packets at all (they are built per seat) and would otherwise sit
            // on a black screen indistinguishable from a broken server.
            welcomes.push(TargetedOp::new_system_to(key, vec![Op::NoSeat { seats: state.policy().player_count }]));
          }
        }

        if live.enemy_count != state.controls.enemy_count
          || live.spread_players != state.controls.spread_players
          || target != state.sim.players.len()
        {
          // Everyone holding a seat is re-welcomed into the rebuilt world, and
          // everyone holding a seat keeps one: `target` is floored at the number
          // occupied, so `reseat_all` never has to drop anybody.
          let occupied = state.seats.occupied_count();
          let reseated = state.reconfigure(live);
          debug_assert_eq!(reseated.len(), occupied, "a resize must not unseat a player");
          for (key, seat) in &reseated {
            welcomes.push(TargetedOp::new_system_to(*key, vec![Op::Welcome { player: *seat as PlayerId, policy: state.policy() }]));
          }
        } else {
          // A non-structural edit still moves the policy a joiner reasons about
          // (send rate, whether coins exist, how far the crowd LOD reaches). Push
          // a fresh policy to everyone when it changes, so a joiner is never left
          // interpolating against a send rate the host has since altered.
          let before = state.policy();
          state.controls = live;
          let after = state.policy();
          if before != after {
            for key in state.seats.keys() {
              welcomes.push(TargetedOp::new_system_to(*key, vec![Op::Policy(after)]));
            }
          }
        }

        let pending = state.pending.clone();
        let controls = state.controls;
        let packets = state.sim.advance_seats(delta_time.as_millis() as u64, &pending, &controls);
        let is_send_round = !packets.is_empty();
        let now = state.sim.now_ms();
        // Rates are over the simulation's own clock, not wall time, so a test
        // that runs faster than real time still measures itself honestly.
        for meter in state.meters() {
          meter.elapsed(now);
        }

        let by_seat = state.seats.by_seat();
        for (player, packet) in packets {
          state.bytes.add(packet.bytes() as u64);
          state.naive_bytes.add(packet.naive_bytes() as u64);
          state.crowd_bytes.add((packet.crowds.len() * CROWD_BYTES) as u64);
          state.relevant.add((packet.samples.len() + packet.entered.len()) as u64);
          state.spawns.add(packet.entered.len() as u64);
          state.despawns.add(packet.left.len() as u64);
          if let Some(key) = by_seat.get(&(player as usize)) {
            let entry = state.down.entry(*key).or_default();
            entry.send(now, Downstream::Frame(Box::new(packet)), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          } else {
            // A seat nobody is connected to still has packets built for it, and
            // its "client" is this process: it holds exactly what it was sent,
            // over a wire that cannot lose anything. So it acknowledges.
            //
            // Without this, every unoccupied seat's baseline stays empty for
            // ever, and under ack recovery an empty baseline means the next
            // packet is a **full dump** rather than a delta. With one human and
            // 127 bots that made 127 of every 128 packets a complete re-send of
            // a whole visible set: the arena reported ~137 spawns per packet
            // where a well-behaved client sees ~17, and the bandwidth readout
            // charged a full arena for a defect none of its clients had.
            let seq = packet.seq;
            let digest = packet.visible_digest;
            state.sim.receive_ack(player as usize, seq, u64::MAX, digest);
          }
        }

        // The player stream, through the same impairment link so it is delayed
        // exactly as the entity stream is. One frame per recipient now: each
        // carries only the players that recipient can see, or that an enemy it
        // holds is chasing, which is what stops the player stream being the
        // largest line in the budget once the arena is large.
        if let Some(frames) = state.sim.take_player_frames() {
          for (seat, frame) in frames {
            let Some(key) = by_seat.get(&(seat as usize)) else { continue };
            // Metered on both sides of the comparison. Counting the real cost
            // here and not its counterfactual is what made the saving read
            // negative once player state moved onto this stream.
            state.bytes.add(frame.bytes() as u64);
            state.naive_bytes.add(frame.naive_bytes() as u64);
            let entry = state.down.entry(*key).or_default();
            entry.send(now, Downstream::Players(frame), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
        }

        let mut out = welcomes;
        // Frames leave through the impairment link, so the host's latency and
        // jitter act on a real outbound path. With no host, the delay is zero.
        for (key, link) in state.down.iter_mut() {
          for message in link.drain_due(now) {
            let op = match message {
              Downstream::Frame(packet) => Op::Frame(packet),
              Downstream::Players(frame) => Op::Players(frame),
            };
            out.push(TargetedOp::new_system_to(*key, vec![op]));
          }
        }
        // Movement acknowledgements ride the tick, not the frame: inputs arrive
        // far more often than frames go out, and reeling prediction back in
        // should not itself be delayed by the impairment.
        for (key, seq) in &state.input_acked {
          out.push(TargetedOp::new_system_to(*key, vec![Op::InputAck { seq: *seq }]));
        }

        if is_send_round && let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }

        // A machine-readable trace of the numbers the panel shows, once a
        // second, when `HORDE_TRACE=1` is set. Screenshots of a live readout
        // cannot settle an argument about a trend: they are two points, they
        // arrive with no timeline, and every explanation offered for them so
        // far has been a story fitted to two numbers. This is the raw series
        // from the machine that is actually running the thing.
        if is_send_round && std::env::var_os("HORDE_TRACE").is_some() {
          let second = now / 1000;
          if second > state.traced_second {
            state.traced_second = second;
            let v = state.host_view();
            println!(
              "TRACE,{},{:.0},{:.0},{},{},{:.1},{:.1},{},{}",
              second,
              v.bytes_per_sec(),
              v.lifetime_bytes_per_sec(),
              v.alive,
              v.kills,
              v.mean_relevant(),
              v.mean_spawns_per_packet(),
              state.sim.coin_count(),
              state.seats.occupied_count(),
            );
          }
        }
        Ok(LogicOutput::ops(out))
      }
    }
  }
}

/// A snapshot provider that provides nothing: the world goes out as `Op::Frame`,
/// which is a per-recipient delta on a fixed cadence, so join snapshots are off.
pub struct NoSnapshots;

#[async_trait]
impl plaza::snapshot::SnapshotProvider<PlayerKey, Arena, Op> for NoSnapshots {
  async fn create_snapshot(
    &self,
    _full_state: &Arena,
    _target_agent: Option<&Agent<PlayerKey>>,
    _context: Option<plaza::snapshot::SnapshotContext>,
  ) -> Result<Option<Op>, plaza::snapshot::SnapshotError<PlayerKey>> {
    // No snapshot concept: this arena streams deltas, and a joiner is caught
    // up by the relevance stream rather than by a whole-state message.
    Ok(None)
  }
}

/// Unused, but `MessageTarget` has to be nameable for the logic above to compile
/// against plaza's re-exports.
#[allow(dead_code)]
fn _target_is_used(_: MessageTarget<PlayerKey>) {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::Upgrade;
  use std::time::Duration;

  fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(logic.process_input(state, input)).unwrap()
  }

  fn slots(controls: Controls) -> (Arc<Mutex<Controls>>, Arc<Mutex<HostView>>) {
    (Arc::new(Mutex::new(controls)), Arc::new(Mutex::new(HostView::default())))
  }

  fn frames_in(out: &LogicOutput<Op, PlayerKey>) -> usize {
    out.ops.iter().filter(|op| matches!(op.ops.first(), Some(Op::Frame(_)))).count()
  }

  fn welcomed(out: &LogicOutput<Op, PlayerKey>) -> bool {
    out.ops.iter().any(|op| matches!(op.ops.first(), Some(Op::Welcome { .. })))
  }

  fn small() -> Controls {
    Controls { enemy_count: 200, sync_hz: 60, latency_ms: 0, jitter_ms: 0, ..Controls::default() }
  }

  /// A stand-in for the transport's measurements, which is the only thing the
  /// arena ever sees. The real one times a WebSocket ping; a test just states
  /// what the link is.
  fn link(one_way_ms: u64, samples: u64) -> LatencySource {
    Arc::new(move |_| Some((Duration::from_millis(one_way_ms * 2), samples)))
  }

  /// Joins and completes admission, which is now two steps rather than one.
  ///
  /// Every test below used to seat a player with a single `AgentJoined`. The
  /// server waits for the transport to measure the connection now, so a test
  /// that only joins gets one still being probed and no seat.
  fn admit(logic: &ArenaLogic, state: &mut Arena, agent: &Agent<PlayerKey>) {
    step(logic, state, LogicInput::AgentJoined { agent: agent.clone() });
    for _ in 0..8u64 {
      if agent.id_cloned().is_some_and(|k| state.seat_of(&k).is_some()) {
        return;
      }
      step(logic, state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    }
  }

  #[test]
  fn a_seat_nobody_is_connected_to_still_acknowledges() {
    // An unacknowledged baseline stays empty, and under ack recovery an empty
    // baseline means the next packet is a full dump rather than a delta. Seats
    // driven by bots have no client to acknowledge for them, so every packet
    // built for them was a complete re-send of a whole visible set, for ever.
    //
    // It is invisible on the wire (nothing is connected to those seats) and
    // loud in the readouts, which count every packet built: the arena reported
    // roughly eight times the spawns per packet that a well-behaved client
    // actually causes, and charged the bandwidth meter for all of it.
    let controls = Controls { player_count: 4, enemy_count: 400, ..small() };
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(0, ADMIT_SAMPLES));

    // Nobody joins: every seat is a bot.
    let mut spawns_early = 0usize;
    let mut spawns_late = 0usize;
    for tick in 0..240u32 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      let spawns: usize = state.sim.last_spawn_count();
      if tick < 60 {
        spawns_early += spawns;
      } else if tick >= 180 {
        spawns_late += spawns;
      }
    }
    assert!(spawns_early > 0, "the arena did fill, so there was something to spawn");
    // Not a larger factor, because the arena is genuinely still filling: waves
    // keep arriving, and a real arrival is a real spawn. What matters is that
    // the count falls at all, which it cannot do while a baseline stays empty.
    assert!(
      spawns_late * 3 < spawns_early,
      "once a seat's baseline is acknowledged its packets become deltas: {spawns_late} late against {spawns_early} early"
    );
  }

  #[test]
  fn lowering_the_player_count_does_not_throw_anyone_out_of_the_game() {
    // The count is a decision about how full the arena may get, not permission
    // to evict somebody mid-game, so it is floored at the number of seats
    // actually occupied and takes effect as people leave.
    let controls = Controls { player_count: 4, ..small() };
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let logic = ArenaLogic::new(cs.clone(), None).with_latency(link(0, ADMIT_SAMPLES));

    let agents: Vec<Agent<PlayerKey>> = (1..=3u64).map(|k| Agent::new_human(k)).collect();
    for agent in &agents {
      admit(&logic, &mut state, agent);
    }
    assert_eq!(state.seats.occupied_count(), 3);

    cs.lock().player_count = 1;
    let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });

    for agent in &agents {
      let key = agent.id_cloned().unwrap();
      assert!(state.seat_of(&key).is_some(), "player {key} was playing and must keep a seat");
    }
    assert!(
      !out.ops.iter().any(|op| matches!(op.ops.first(), Some(Op::NoSeat { .. }))),
      "nobody is told to leave"
    );
    assert_eq!(state.sim.players.len(), 3, "the world holds exactly the people in it");
    assert_eq!(cs.lock().player_count, 3, "and the panel is told what it actually got, so the slider springs back");
  }

  #[test]
  fn a_seat_freed_by_a_leaver_lets_a_lowered_count_take_effect() {
    // The other half: held, not ignored. Once the arena is no longer keeping a
    // seat for somebody, the request the host already made applies.
    let controls = Controls { player_count: 4, ..small() };
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let logic = ArenaLogic::new(cs.clone(), None).with_latency(link(0, ADMIT_SAMPLES));

    let agent = Agent::new_human(1u64);
    admit(&logic, &mut state, &agent);
    cs.lock().player_count = 1;
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert_eq!(state.sim.players.len(), 1, "one player, one seat requested, nothing to hold open");

    let latecomer = Agent::new_human(2u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: latecomer.clone() });
    let mut told = false;
    for _ in 0..8u64 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      told |= out.ops.iter().any(|op| matches!(op.ops.first(), Some(Op::NoSeat { .. })));
    }
    assert_eq!(state.seats.occupied_count(), 1, "the arena is one seat wide, so the joiner gets nothing");
    assert!(told, "and is told so rather than left on a black screen");
  }

  #[test]
  fn a_connection_that_cannot_meet_the_schedule_is_refused_at_the_door() {
    // It used to be welcomed and then have every input silently rejected: an
    // input named for `press + playout` lands past the accepting window once the
    // one-way delay exceeds the budget, so the player could not move and nothing
    // said why. Measured by the transport and refused instead.
    let controls = small();
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let budget = state.admission_budget_ms();
    let logic = ArenaLogic::new(cs, None).with_latency(link(budget + 100, ADMIT_SAMPLES));
    let agent = Agent::new_human(1u64);

    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    assert!(state.seat_of(&1).is_none(), "no seat is handed out before the connection is measured");

    let mut refused = None;
    for _ in 0..8u64 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      for op in &out.ops {
        if let Some(Op::Refused { measured_ms, allowed_ms }) = op.ops.first() {
          refused = Some((*measured_ms, *allowed_ms));
        }
      }
    }

    let (measured, allowed) = refused.expect("a link outside the budget is refused rather than seated");
    assert!(measured as u64 > budget, "the refusal states what was measured: {measured} ms against {allowed}");
    assert_eq!(allowed as u64, budget, "and what the arena allows, derived from the schedule rather than declared");
    assert!(state.seat_of(&1).is_none(), "and it still holds no seat");
  }

  #[test]
  fn a_healthy_connection_is_measured_and_then_seated() {
    // The other half: admission must not become a wall.
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    assert!(state.seat_of(&1).is_some(), "a link inside the budget is seated once it has been measured");
  }

  #[test]
  fn a_link_this_arena_cannot_carry_is_placed_rather_than_refused() {
    // Why placement is worth wiring at all. A room can only say yes or no, so a
    // single arena turns everybody past its budget away. Given somewhere that
    // can carry the link, the answer becomes an address instead of a door slam,
    // and refusal is left for the links no arena can take.
    let controls = small();
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let budget = state.admission_budget_ms();
    let elsewhere: Router = Arc::new(|_| Some((7, "relaxed".to_owned(), "/ws/7".to_owned())));
    let logic = ArenaLogic::new(cs, None)
      .with_latency(link(budget + 100, ADMIT_SAMPLES))
      .with_router(0, elsewhere);

    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let mut placed = None;
    for _ in 0..8u64 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      for op in &out.ops {
        match op.ops.first() {
          Some(Op::Placed { room, endpoint, .. }) => placed = Some((*room, endpoint.clone())),
          Some(Op::Refused { .. }) => panic!("refused a link another arena could carry"),
          _ => {}
        }
      }
    }
    assert_eq!(placed, Some((7, "/ws/7".to_owned())), "the connection is told where it can play");
    assert!(state.seat_of(&1).is_none(), "and takes no seat here");
  }

  #[test]
  fn a_link_no_arena_can_carry_is_still_refused() {
    // Placement does not become a way to never say no. When the router finds
    // nothing, the refusal is what is left, and it still carries both numbers.
    let controls = small();
    let (cs, _view) = slots(controls);
    let mut state = Arena::new(controls);
    let budget = state.admission_budget_ms();
    let nowhere: Router = Arc::new(|_| None);
    let logic = ArenaLogic::new(cs, None)
      .with_latency(link(budget + 100, ADMIT_SAMPLES))
      .with_router(0, nowhere);

    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let mut refused = false;
    for _ in 0..8u64 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      refused |= out.ops.iter().any(|o| matches!(o.ops.first(), Some(Op::Refused { .. })));
    }
    assert!(refused, "with nowhere to place it, the connection is refused");
  }

  #[test]
  fn an_arena_that_cannot_measure_admits_nobody() {
    // Failing closed, deliberately. Without a transport measurement the arena
    // has no basis to say a connection can meet the schedule, and guessing that
    // it can is how the silent exclusion happened in the first place.
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    assert!(state.seat_of(&1).is_none(), "no measurement, no seat");
  }

  #[test]
  fn the_host_view_fills_in_and_frames_reach_the_player() {
    let controls = small();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone())).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);

    let agent = Agent::new_human(1u64);
    admit(&logic, &mut state, &agent);
    assert!(state.seat_of(&1).is_some(), "a measured joiner is welcomed and given a seat");

    let mut frames = 0;
    for _ in 0..6 {
      frames += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
    }
    assert!(frames > 0, "frames reach the seated player at zero latency");

    let v = view.lock();
    assert!(!v.players.is_empty() && !v.truth.is_empty(), "the omniscient view is populated");
    assert!(v.bytes_per_sec() > 0.0, "bandwidth accounting accrues");
    assert!(v.mean_relevant() > 0.0, "the relevance readout has something in it");
  }

  #[test]
  fn latency_holds_frames_back_without_dropping_them() {
    let controls = Controls { latency_ms: 200, ..small() };
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view)).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));

    let mut early = 0;
    for _ in 0..5 {
      early += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
    }
    assert_eq!(early, 0, "nothing delivered before the latency has elapsed");
    let mut later = 0;
    for _ in 0..15 {
      later += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
    }
    assert!(later > 0, "the held frames arrive once the delay passes");
  }

  #[test]
  fn the_loss_slider_actually_drops_frames_on_the_real_path() {
    // Regression. The impairment link was given latency and jitter from the
    // panel and a hardcoded zero for loss, so the slider moved and nothing
    // happened. It worked in the offline world, which is where every measurement
    // was taken, and did nothing at all on the path a host or a joiner uses.
    fn delivered(loss: f32) -> usize {
      let controls = Controls { loss_pct: loss, latency_ms: 0, jitter_ms: 0, ..small() };
      let (cs, _view) = slots(controls);
      let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
      let mut state = Arena::new(controls);
      admit(&logic, &mut state, &Agent::new_human(1u64));
      let mut frames = 0;
      for _ in 0..200 {
        frames += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
      }
      frames
    }

    let clean = delivered(0.0);
    let lossy = delivered(40.0);
    assert!(clean > 20, "the test needs frames to lose: {clean}");
    assert!(
      (lossy as f32) < clean as f32 * 0.8,
      "40% loss should visibly thin the stream: {lossy} against {clean}"
    );
  }

  #[test]
  fn the_loss_slider_thins_the_uplink_too() {
    // The return path was perfect however far the slider was dragged, which is
    // exactly the traffic the recovery machinery exists for: acknowledgements are
    // what let the server tell a starved mirror from a healthy one.
    fn accepted(loss: f32) -> u64 {
      let controls = Controls { loss_pct: loss, latency_ms: 0, jitter_ms: 0, ..small() };
      let (cs, _view) = slots(controls);
      let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
      let mut state = Arena::new(controls);
      let agent = Agent::new_human(1u64);
      admit(&logic, &mut state, &agent);
      for i in 0..400u64 {
        let tick = state.sim.tick() + controls.playout_delay_ms / 16;
        step(
          &logic,
          &mut state,
          LogicInput::AgentOps { source: agent.clone(), ops: vec![Op::Input { seq: i + 1, dx: 1.0, dy: 0.0, tick }] },
        );
        step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      }
      state.sim.accepted_inputs()
    }

    let clean = accepted(0.0);
    let lossy = accepted(40.0);
    assert!(clean > 300, "the test needs inputs to lose: {clean}");
    assert!((lossy as f32) < clean as f32 * 0.8, "40% loss should thin the uplink: {lossy} against {clean}");
  }

  #[test]
  fn changing_the_enemy_count_rebuilds_and_rewelcomes() {
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs.clone(), None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    // Let the clock run first, so the preservation across the rebuild is testable.
    for _ in 0..40 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    }
    let before = state.sim.now_ms();
    assert!(before > 0);

    cs.lock().enemy_count = 400;
    let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert!(welcomed(&out), "the reseated player is welcomed into the new world");
    assert_eq!(state.sim.alive_count(), 400, "the world was rebuilt");
    // The clock must not have jumped back to zero, or a client's packet-age
    // estimate would spike and fling the horde at the player.
    assert!(state.sim.now_ms() >= before, "the rebuilt world kept time continuous: {} vs {before}", state.sim.now_ms());
  }

  #[test]
  fn a_clients_own_traffic_is_delayed_by_the_hosts_slider() {
    // The link was asymmetric and nothing said so: outbound traffic was dropped
    // per the loss slider but never delayed, so a client's inputs and
    // acknowledgements arrived instantly however far latency was dragged. A
    // round trip therefore read as one direction, and the input schedule, whose
    // whole job is to cover a player's one way delay, was being exercised
    // against a path that had none.
    let controls = Controls { latency_ms: 200, jitter_ms: 0, ..small() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(0, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    let agent = Agent::new_human(1u64);
    admit(&logic, &mut state, &agent);

    let before = state.sim.denied_purchases;
    step(&logic, &mut state, LogicInput::AgentOps { source: agent.clone(), ops: vec![Op::Buy(Upgrade::Repulsor)] });

    // Nothing for the first stretch, because the op is still in flight.
    for _ in 0..6u32 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    }
    assert_eq!(state.sim.denied_purchases, before, "a 200 ms uplink should not have delivered after 96 ms");

    // And then it lands.
    for _ in 0..10u32 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    }
    assert_eq!(state.sim.denied_purchases, before + 1, "and should have delivered by 256 ms");
  }

  #[test]
  fn a_purchase_request_reaches_the_server() {
    // Proves the Buy op is routed to the authoritative server: an unaffordable
    // request is refused there, which is the only place that count can move.
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));

    assert_eq!(state.sim.denied_purchases, 0);
    step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64), ops: vec![Op::Buy(Upgrade::Repulsor)] });
    // A tick, because an op is held on the impaired uplink and acted on when its
    // delay expires rather than the instant it arrives. At zero latency that is
    // the very next tick, which is the point: the path is the same either way.
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert_eq!(state.sim.denied_purchases, 1, "an empty wallet's purchase is refused by the server");
  }

  #[test]
  fn a_client_joining_a_warm_arena_gets_the_whole_world_at_once() {
    // Every seat's relevance baseline advances from startup, occupied or not, so
    // a client that joins after the arena has been running a while must still be
    // handed the whole visible world at once. Without the seat reset on join, its
    // first frame is a diff against a baseline it never held: almost nothing is
    // sent as `entered`, and the world arrives only as the slow trickle of what
    // later becomes newly relevant, hundreds of frames short of the truth.
    let controls = Controls { enemy_count: 300, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    // Warm the arena well past a full baseline's worth of frames with nobody in
    // seat 0, so its `prev_vis` fills up before the client arrives.
    for _ in 0..150u64 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    }

    admit(&logic, &mut state, &Agent::new_human(1u64));
    let seat = state.seat_of(&1).unwrap();
    let mut client = crate::sim::client::Client::new(seat as PlayerId, 4);

    // Where the server actually is: the warm-up plus the admission probes.
    let mut recv = state.sim.now_ms();
    let mut pending_ack: Option<Op> = None;
    let mut frames_seen = 0u32;
    let mut synced_on_frame: Option<u32> = None;
    for _ in 0..60u64 {
      if let Some(ack) = pending_ack.take() {
        step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64), ops: vec![ack] });
      }
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      recv += 16;
      for op in &out.ops {
        if let Some(Op::Frame(packet)) = op.ops.first() {
          frames_seen += 1;
          client.receive_packet((**packet).clone(), recv);
          client.tick(16, &controls);
          if synced_on_frame.is_none() && client.known_entities() > 0 && client.last_digest() == packet.visible_digest {
            synced_on_frame = Some(frames_seen);
          }
          if let Some((newest, mask)) = client.acks().encode() {
            pending_ack = Some(Op::Ack { newest, mask, digest: client.last_digest() });
          }
        }
      }
    }
    // Whole the moment it applies anything, rather than converging over several
    // packets, which is what the warm-arena bug looked like.
    //
    // Counted in packets *received*, while the client applies them at its render
    // instant, so the first packet is played out a tick or two after it lands
    // and the count is not always one. What the bug produced was a trickle over
    // many frames, so a small bound still catches it and an exact one only
    // tracked whatever the render delay happened to be.
    let synced = synced_on_frame.expect("the joiner never agreed with the server at all");
    assert!(synced <= 3, "the joiner took {synced} frames to hold the whole world, which is a trickle rather than a dump");
    assert_eq!(client.digest_mismatches(), 0, "the mirror disagreed after a warm-arena join {} times", client.digest_mismatches());
  }

  #[test]
  fn a_networked_client_mirror_stays_in_sync_through_the_arena() {
    // Reproduces the digest-mismatch report by driving the real ArenaLogic and a
    // real client through the frame/ack loop, without a socket: extract each
    // Frame the arena emits, apply it, feed the client's ack back as an Op.
    let controls = Controls { enemy_count: 300, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    // The first joiner takes the top free seat.
    let seat = state.seat_of(&1).unwrap();
    let mut client = crate::sim::client::Client::new(seat as PlayerId, 4);

    // Admission has already advanced the arena, and this stands in for the
    // client's estimate of server time, so it starts where the server is rather
    // than at zero.
    let mut recv = state.sim.now_ms();
    let mut pending_ack: Option<Op> = None;
    for _ in 0..1200u64 {
      // The ack from the previous frame arrives before this tick's frame is built.
      if let Some(ack) = pending_ack.take() {
        step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64), ops: vec![ack] });
      }
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      recv += 16;
      for op in &out.ops {
        if let Some(Op::Frame(packet)) = op.ops.first() {
          client.receive_packet((**packet).clone(), recv);
          client.tick(16, &controls);
          if let Some((newest, mask)) = client.acks().encode() {
            pending_ack = Some(Op::Ack { newest, mask, digest: client.last_digest() });
          }
        }
      }
    }
    assert_eq!(client.digest_mismatches(), 0, "the client mirror disagreed with the server digest {} times", client.digest_mismatches());
  }

  /// Drives a real client through the real arena and reports what it saw: the
  /// most unresolved frames it ever held (the ghost), and how many arrived after
  /// the instant they describe had passed (the underruns).
  fn ghost_and_underruns(jitter_ms: u64, render_delay_ms: u64) -> (usize, u64) {
    let controls = Controls { enemy_count: 300, jitter_ms, render_delay_ms, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    let seat = state.seat_of(&1).unwrap();
    let mut client = crate::sim::client::Client::new(seat as PlayerId, 4);
    client.set_render_delay(render_delay_ms);

    // Admission has already advanced the arena, and this stands in for the
    // client's estimate of server time, so it starts where the server is rather
    // than at zero.
    let mut recv = state.sim.now_ms();
    let mut peak = 0usize;
    // The ack loop is load-bearing and its absence is invisible: without it the
    // baseline never advances, every frame is a full rebuild carrying no samples,
    // and any measurement about samples reads zero for an unrelated reason.
    let mut pending_ack: Option<Op> = None;
    for _ in 0..600u64 {
      if let Some(ack) = pending_ack.take() {
        step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64), ops: vec![ack] });
      }
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      recv += 16;
      for op in &out.ops {
        if let Some(Op::Frame(packet)) = op.ops.first() {
          client.receive_packet((**packet).clone(), recv);
        }
      }
      client.tick(16, &controls);
      if let Some((newest, mask)) = client.acks().encode() {
        pending_ack = Some(Op::Ack { newest, mask, digest: client.last_digest() });
      }
      peak = peak.max(client.ghost_enemies().len());
    }
    (peak, client.underruns())
  }

  #[test]
  fn the_render_delay_is_the_servers_and_a_jittery_link_underruns_rather_than_hiding() {
    // The point of fixing T on the server's timeline. Latency and jitter say when
    // bytes arrive; the render delay says which moment is on screen. Letting the
    // first move the second is what let a bad link quietly show one player an
    // older world than everybody else, reporting nothing.
    //
    // With T fixed, a declared delay wide enough for the link carries it, and one
    // too narrow produces a countable event instead of silent degradation.
    let (ghost_wide, late_wide) = ghost_and_underruns(20, 200);
    let (_, late_narrow) = ghost_and_underruns(200, 30);

    assert!(ghost_wide > 0, "a delay wider than the link should leave unresolved frames to ghost");
    assert_eq!(late_wide, 0, "a delay wider than the link should not underrun");
    assert!(late_narrow > 0, "a delay narrower than the jitter should underrun rather than absorb it");
  }

  #[test]
  fn a_drifted_mirror_is_caught_by_its_digest_and_rebuilt() {
    // Same loop as the sync test, but partway through we reach into the client and
    // drop an enemy it holds, exactly the silent drift a real socket produces: the
    // entity stays in view, is only sampled again, and the client discards a
    // sample for something it no longer holds. The delta stream can never re-send
    // it, because the server believes the client already has it. Only the
    // acknowledged digest can catch the disagreement and force a clean rebuild.
    let controls = Controls { enemy_count: 300, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None).with_latency(link(10, ADMIT_SAMPLES));
    let mut state = Arena::new(controls);
    admit(&logic, &mut state, &Agent::new_human(1u64));
    let seat = state.seat_of(&1).unwrap();
    let mut client = crate::sim::client::Client::new(seat as PlayerId, 4);

    // Admission has already advanced the arena, and this stands in for the
    // client's estimate of server time, so it starts where the server is rather
    // than at zero.
    let mut recv = state.sim.now_ms();
    let mut pending_ack: Option<Op> = None;
    let mut injected_at: Option<u64> = None;
    let mut rebuilt = false;
    let mut mismatches_at_mark = 0u64;
    for i in 0..1200u64 {
      if let Some(ack) = pending_ack.take() {
        step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64), ops: vec![ack] });
      }
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      recv += 16;
      for op in &out.ops {
        if let Some(Op::Frame(packet)) = op.ops.first() {
          // A rebuild after the injection is the repair firing.
          if injected_at.is_some() && packet.full_baseline {
            rebuilt = true;
          }
          client.receive_packet((**packet).clone(), recv);
          client.tick(16, &controls);
          // Once the mirror is well populated, corrupt it exactly once.
          if injected_at.is_none() && i > 200 && client.force_drop_an_enemy() {
            injected_at = Some(i);
          }
          if let Some((newest, mask)) = client.acks().encode() {
            pending_ack = Some(Op::Ack { newest, mask, digest: client.last_digest() });
          }
        }
      }
      // A margin after the injection, freeze the mismatch count. From here on it
      // must not grow: the mirror is rebuilt and stays in agreement.
      if let Some(at) = injected_at
        && i == at + 80
      {
        mismatches_at_mark = client.digest_mismatches();
      }
    }
    assert!(injected_at.is_some(), "the test never managed to drop an enemy");
    // The drift was actually noticed, not silently tolerated.
    assert!(mismatches_at_mark > 0, "the injected drift went undetected");
    // The repair fired: the server sent a clean full baseline after the drift.
    assert!(rebuilt, "no full baseline was sent to repair the drifted mirror");
    // And it healed: no new disagreement once the rebuild landed.
    assert_eq!(
      client.digest_mismatches(), mismatches_at_mark,
      "the mirror kept disagreeing after it should have been rebuilt"
    );
  }

  #[cfg(feature = "websocket")]
  #[test]
  fn every_frame_survives_the_json_wire() {
    // The client drops any frame it cannot deserialize, so a frame that fails to
    // parse is a silent hole in the delta stream: the mirror diverges and the
    // digest ticks until recovery re-derives it. serde_json writes NaN/Infinity
    // as `null`, which then fails to parse back into f32, so a single bad float
    // anywhere in a packet loses the whole frame. Run the server long enough to
    // ramp difficulty and churn the field, and round-trip every frame.
    let controls = Controls { enemy_count: 400, sync_hz: 30, ..Controls::default() };
    let mut server = crate::sim::server::Server::new(controls.enemy_count, 4, controls.spread_players);
    let mut checked = 0u32;
    for i in 0..(60 * 60) {
      let t = i as f32 * 0.05;
      for (_, packet) in server.advance(16, Vec2::new(t.cos(), t.sin()), &controls) {
        let ops = vec![Op::Frame(Box::new(packet.clone()))];
        let mut bytes = Vec::new();
        plaza_wire::frame::begin(plaza_wire::frame::Kind::Ops, &mut bytes);
        bytes.extend_from_slice(&serde_json::to_vec(&ops).expect("encode"));
        let (tag, body) = plaza_wire::frame::split(&bytes).expect("a non-empty frame");
        assert_eq!(plaza_wire::frame::Kind::from_byte(tag), Some(plaza_wire::frame::Kind::Ops));
        let back = serde_json::from_slice::<Vec<Op>>(body);
        assert!(back.is_ok(), "a frame failed to deserialize: {:?}", back.err());
        if let Ok(ops) = back
          && let Some(Op::Frame(p2)) = ops.first()
        {
          assert_eq!(p2.visible_digest, packet.visible_digest, "digest survived the wire");
        }
        checked += 1;
      }
    }
    assert!(checked > 100, "actually exercised the wire: {checked} frames");
  }

  // The impairment link's ordering guarantee is now `LatencyLink`'s to keep, and
  // is covered by `an_ordered_link_delays_but_never_reorders` in
  // `plaza_client_utils::net_sim`.
}

#[cfg(test)]
mod wire_size {
  use super::*;
  use plaza_wire::{frame, JsonCodec, MsgPackCodec, WireCodec};

  /// What the codec is worth on this game's real traffic.
  ///
  /// Every earlier figure in this project was measured on invented op types.
  /// This one encodes the `Packet` the arena actually sends, so the claim is
  /// about horde rather than about a benchmark's imagination.
  #[test]
  fn msgpack_against_json_on_a_real_frame() {
    let controls = Controls::default();
    let mut server = crate::sim::server::Server::new(controls.enemy_count, 4, controls.spread_players);
    let mut json_total = 0usize;
    let mut mp_total = 0usize;
    let mut frames = 0usize;

    for i in 0..600 {
      let t = i as f32 * 0.05;
      for (_, packet) in server.advance(16, Vec2::new(t.cos(), t.sin()), &controls) {
        let ops = vec![Op::Frame(Box::new(packet.clone()))];
        let mut j = Vec::new();
        frame::begin(frame::Kind::Ops, &mut j);
        JsonCodec.encode_into(&ops, &mut j).expect("json");
        let mut m = Vec::new();
        frame::begin(frame::Kind::Ops, &mut m);
        MsgPackCodec.encode_into(&ops, &mut m).expect("msgpack");

        // Same frame, same ops, both ends of the codec choice.
        let back: Vec<Op> = MsgPackCodec.decode(frame::split(&m).unwrap().1).expect("round trip");
        assert_eq!(back.len(), 1, "msgpack round-trips the real packet");

        json_total += j.len();
        mp_total += m.len();
        frames += 1;
      }
    }

    assert!(frames > 0, "the arena produced no frames to measure");
    let json_avg = json_total / frames;
    let mp_avg = mp_total / frames;
    eprintln!(
      "PACKET_WIRE frames={frames} json={json_avg} B/frame msgpack={mp_avg} B/frame ({}% of json)",
      mp_total * 100 / json_total
    );
    assert!(
      mp_total < json_total,
      "the codec swap has to pay on real traffic, not only on a benchmark's toy ops"
    );
  }
}

#[cfg(test)]
mod client_server_wire {
  use super::*;
  use plaza_wire::{frame, MsgPackCodec, WireCodec};

  /// The client's outbound bytes, decoded exactly the way the server's
  /// deserialize bridge does. A black screen in the browser is what happens
  /// when these two disagree, so the agreement is a test rather than a hope.
  #[test]
  fn what_the_client_sends_is_what_the_server_reads() {
    for op in [
      Op::Hello { protocol: PROTOCOL },
      Op::Input { seq: 7, dx: -0.5, dy: 0.5, tick: 3 },
      Op::Ack { newest: 9, mask: 0xff, digest: 1234 },
      Op::Ping { origin_ms: 42 },
    ] {
      // Exactly `Client::send_op`.
      let mut out = Vec::new();
      frame::begin(frame::Kind::Ops, &mut out);
      MsgPackCodec.encode_into(&std::slice::from_ref(&op), &mut out).expect("client encode");

      // Exactly the server bridge.
      let (tag, body) = frame::split(&out).expect("non-empty");
      assert_eq!(frame::Kind::from_byte(tag), Some(frame::Kind::Ops));
      let ops: Vec<Op> = MsgPackCodec.decode(body).expect("server decode");
      assert_eq!(ops.len(), 1, "one op in, one op out: {op:?}");
    }
  }

  /// And the other direction: what the server broadcasts is what the client
  /// reads, which is the half that decides whether anything renders.
  #[test]
  fn what_the_server_sends_is_what_the_client_reads() {
    let controls = Controls::default();
    let mut server = crate::sim::server::Server::new(controls.enemy_count, 4, controls.spread_players);
    let mut checked = 0;
    for i in 0..60 {
      let t = i as f32 * 0.05;
      for (_, packet) in server.advance(16, Vec2::new(t.cos(), t.sin()), &controls) {
        let ops = vec![Op::Frame(Box::new(packet.clone()))];
        // Exactly `TransportSession::encode_message`.
        let mut out = Vec::new();
        frame::begin(frame::Kind::Ops, &mut out);
        MsgPackCodec.encode_into(&ops, &mut out).expect("server encode");

        // Exactly `Client::on_frame`.
        let (tag, body) = frame::split(&out).expect("non-empty");
        assert_eq!(frame::Kind::from_byte(tag), Some(frame::Kind::Ops));
        let back: Vec<Op> = MsgPackCodec.decode(body).expect("client decode");
        assert!(matches!(back.first(), Some(Op::Frame(_))), "a frame survived the round trip");
        checked += 1;
      }
    }
    assert!(checked > 0, "no frames were produced to check");
  }
}
