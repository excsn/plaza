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

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_client_utils::net_sim::Rng;

use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::server::{Seat, Server};
use crate::sim::types::{
  Coin, Controls, EnemyKind, Handle, Packet, PlayerId, Projectile, Vec2, Wallet, CROWD_BYTES, MAX_PLAYERS,
};

/// How a connection is identified. Assigned by the server on accept, never
/// supplied by the client.
pub type PlayerKey = u64;

/// Seats in the arena. Fixed, unlike the black hole example: horde's player count
/// is a property of the world, not a live slider, so joiners fill these four and
/// bots drive whatever is empty.
const PLAYER_COUNT: usize = MAX_PLAYERS;

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

  pub alive: usize,
  pub kills: u64,
  pub nova_kills_last: usize,
  pub last_nova_ms: Option<u64>,
  pub server_now_ms: u64,
  pub coins_expired: u64,
  pub denied_purchases: u64,
  pub full_resends: u64,

  bytes_sent: u64,
  naive_bytes_sent: u64,
  crowd_bytes: u64,
  packets_sent: u64,
  relevant_total: u64,
  relevant_samples: u64,
  spawns_total: u64,
  despawns_total: u64,
  uptime_ms: u64,
}

impl HostView {
  pub fn bytes_per_sec(&self) -> f64 {
    if self.uptime_ms == 0 {
      return 0.0;
    }
    self.bytes_sent as f64 / (self.uptime_ms as f64 / 1000.0)
  }
  pub fn naive_bytes_per_sec(&self) -> f64 {
    if self.uptime_ms == 0 {
      return 0.0;
    }
    self.naive_bytes_sent as f64 / (self.uptime_ms as f64 / 1000.0)
  }
  pub fn crowd_bytes_per_sec(&self) -> f64 {
    if self.uptime_ms == 0 {
      return 0.0;
    }
    self.crowd_bytes as f64 / (self.uptime_ms as f64 / 1000.0)
  }
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
  /// How long ago the last area pulse fired, in seconds, while still worth
  /// drawing. Computed at publish, which is fine for an observer without a clock.
  pub fn nova_flash_age(&self) -> Option<f32> {
    let fired = self.last_nova_ms?;
    let age = self.server_now_ms.saturating_sub(fired) as f32 / 1000.0;
    (age <= 0.45).then_some(age)
  }
}

/// The outbound impairment for one connection: frames held for `latency ± jitter`
/// before they are released, so the host's latency and jitter sliders act on a
/// real link instead of a simulation. Kept `Clone` because the arena state must
/// be, and `LatencyLink` is not.
#[derive(Clone, Debug, Default)]
struct Downlink {
  queue: Vec<(u64, Packet)>,
}

impl Downlink {
  fn send(&mut self, now_ms: u64, packet: Packet, latency_ms: u64, jitter_ms: u64, rng: &mut Rng) {
    let deliver_at = now_ms + latency_ms + rng.up_to(jitter_ms);
    self.queue.push((deliver_at, packet));
  }

  fn drain_due(&mut self, now_ms: u64) -> Vec<Packet> {
    let mut due: Vec<(u64, Packet)> = Vec::new();
    let mut kept: Vec<(u64, Packet)> = Vec::new();
    for (at, packet) in self.queue.drain(..) {
      if at <= now_ms {
        due.push((at, packet));
      } else {
        kept.push((at, packet));
      }
    }
    self.queue = kept;
    due.sort_by_key(|(at, _)| *at);
    due.into_iter().map(|(_, p)| p).collect()
  }
}

/// Everything the arena owns. `plaza` requires `Clone` for its state-query
/// command; nothing on the hot path clones it.
#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  /// Which player each connection is driving.
  seats: HashMap<PlayerKey, usize>,
  free: Vec<usize>,
  /// The newest movement direction for each seat, applied on the next tick.
  pending: Vec<Seat>,
  /// The newest input sequence accepted per player, echoed back so a client can
  /// replay only what the server has not applied.
  input_acked: HashMap<PlayerKey, u64>,

  down: HashMap<PlayerKey, Downlink>,
  rng: Rng,

  bytes_sent: u64,
  naive_bytes_sent: u64,
  crowd_bytes: u64,
  packets_sent: u64,
  relevant_total: u64,
  relevant_samples: u64,
  spawns_total: u64,
  despawns_total: u64,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let sim = Server::new(controls.enemy_count, PLAYER_COUNT, controls.spread_players);
    Self {
      sim,
      controls,
      seats: HashMap::new(),
      free: (0..PLAYER_COUNT).collect(),
      pending: vec![Seat::Bot; PLAYER_COUNT],
      input_acked: HashMap::new(),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
      bytes_sent: 0,
      naive_bytes_sent: 0,
      crowd_bytes: 0,
      packets_sent: 0,
      relevant_total: 0,
      relevant_samples: 0,
      spawns_total: 0,
      despawns_total: 0,
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    ServerPolicy {
      sync_hz: self.controls.sync_hz,
      coins: self.controls.coins,
      generational_ids: self.controls.generational_ids,
      crowd_lod_theta: self.controls.crowd_lod_theta,
      relevance: self.controls.relevance,
      enemy_count: self.controls.enemy_count,
      player_count: PLAYER_COUNT,
    }
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.get(key).copied()
  }

  /// Seats a joiner, or refuses when the arena is full.
  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    if let Some(seat) = self.seats.get(&key) {
      return Some(*seat);
    }
    let seat = self.free.pop()?;
    self.seats.insert(key, seat);
    self.pending[seat] = Seat::Bot;
    Some(seat)
  }

  fn unseat(&mut self, key: &PlayerKey) {
    if let Some(seat) = self.seats.remove(key) {
      self.pending[seat] = Seat::Bot;
      self.free.push(seat);
    }
    self.input_acked.remove(key);
    self.down.remove(key);
  }

  /// Rebuilds the world for a new enemy count or player layout, reseating whoever
  /// is connected and returning the seats that need a fresh `Welcome`.
  fn reconfigure(&mut self, controls: Controls) -> Vec<(PlayerKey, usize)> {
    let old_keys: Vec<PlayerKey> = self.seats.keys().copied().collect();
    let clock = self.sim.now_ms();
    self.controls = controls;
    self.sim = Server::new(controls.enemy_count, PLAYER_COUNT, controls.spread_players);
    // Keep time continuous across the rebuild, so a client's packet-age estimate
    // does not jump and fling the horde at the player.
    self.sim.set_clock(clock);
    self.free = (0..PLAYER_COUNT).collect();
    self.pending = vec![Seat::Bot; PLAYER_COUNT];
    self.seats.clear();
    self.input_acked.clear();
    self.down.clear();
    self.bytes_sent = 0;
    self.naive_bytes_sent = 0;
    self.crowd_bytes = 0;
    self.packets_sent = 0;
    self.relevant_total = 0;
    self.relevant_samples = 0;
    self.spawns_total = 0;
    self.despawns_total = 0;

    let mut welcomed = Vec::new();
    for key in old_keys {
      if let Some(seat) = self.free.pop() {
        self.seats.insert(key, seat);
        welcomed.push((key, seat));
      }
    }
    welcomed
  }

  fn host_view(&self) -> HostView {
    HostView {
      players: self.sim.players.clone(),
      truth: self.sim.live_enemies().map(|(h, e)| (h, e.pos, e.kind)).collect(),
      projectiles: self.sim.projectiles.clone(),
      coins: self.sim.coins.clone(),
      wallets: self.sim.wallets.clone(),
      coins_claimed: self.sim.coins_claimed.clone(),
      alive: self.sim.alive_count(),
      kills: self.sim.kills,
      nova_kills_last: self.sim.nova_kills_last,
      last_nova_ms: self.sim.last_nova_ms,
      server_now_ms: self.sim.now_ms(),
      coins_expired: self.sim.coins_expired,
      denied_purchases: self.sim.denied_purchases,
      full_resends: self.sim.full_resends.iter().sum(),
      bytes_sent: self.bytes_sent,
      naive_bytes_sent: self.naive_bytes_sent,
      crowd_bytes: self.crowd_bytes,
      packets_sent: self.packets_sent,
      relevant_total: self.relevant_total,
      relevant_samples: self.relevant_samples,
      spawns_total: self.spawns_total,
      despawns_total: self.despawns_total,
      uptime_ms: self.sim.now_ms(),
    }
  }
}

/// The stateless half plaza acts through. Carries the shared control slot the
/// host's panel writes and the arena reads, and the optional view the arena
/// publishes for a windowed host to draw.
pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self { controls, view }
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
        match state.seat(key) {
          Some(seat) => {
            let policy = state.policy();
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![Op::Welcome { player: seat as PlayerId, policy }])]))
          }
          None => Ok(LogicOutput::none()),
        }
      }

      LogicInput::AgentLeft { agent_id } => {
        state.unseat(&agent_id);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let mut replies = Vec::new();
        for op in ops {
          match op {
            Op::Input { seq, dx, dy } => {
              // Out-of-order inputs are dropped; an older direction overwriting a
              // newer one reads to the player as the controls sticking.
              if state.input_acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.input_acked.insert(key, seq);
              let len = (dx * dx + dy * dy).sqrt();
              let dir = if len > 1.0 { Vec2::new(dx / len, dy / len) } else { Vec2::new(dx, dy) };
              state.pending[seat] = Seat::Steered(dir);
            }
            // The entity stream's acknowledgement and the one purchase a client
            // may request. Both go straight to the server, which is the only thing
            // allowed to move a baseline or spend a coin.
            Op::Ack { newest, mask } => state.sim.receive_ack(seat, newest, mask),
            Op::Buy(upgrade) => state.sim.receive_buy(seat, upgrade),
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              replies.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            // Server-to-client variants coming up mean a confused or hostile
            // client; not an error worth failing the tick over.
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        // Pick up whatever the host's panel changed. A structural change (enemy
        // count or player layout) rebuilds the world and re-welcomes everyone;
        // anything else is a live edit the next tick simply reads.
        let live = *self.controls.lock();
        let mut welcomes = Vec::new();
        if live.enemy_count != state.controls.enemy_count || live.spread_players != state.controls.spread_players {
          for (key, seat) in state.reconfigure(live) {
            welcomes.push(TargetedOp::new_system_to(key, vec![Op::Welcome { player: seat as PlayerId, policy: state.policy() }]));
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

        let by_seat: HashMap<usize, PlayerKey> = state.seats.iter().map(|(key, seat)| (*seat, *key)).collect();
        for (player, packet) in packets {
          state.bytes_sent += packet.bytes() as u64;
          state.naive_bytes_sent += packet.naive_bytes() as u64;
          state.crowd_bytes += (packet.crowds.len() * CROWD_BYTES) as u64;
          state.packets_sent += 1;
          state.relevant_total += (packet.samples.len() + packet.entered.len()) as u64;
          state.relevant_samples += 1;
          state.spawns_total += packet.entered.len() as u64;
          state.despawns_total += packet.left.len() as u64;
          if let Some(key) = by_seat.get(&(player as usize)) {
            let entry = state.down.entry(*key).or_default();
            entry.send(now, packet, controls.latency_ms, controls.jitter_ms, &mut state.rng);
          }
        }

        let mut out = welcomes;
        // Frames leave through the impairment link, so the host's latency and
        // jitter act on a real outbound path. With no host, the delay is zero.
        for (key, link) in state.down.iter_mut() {
          for packet in link.drain_due(now) {
            out.push(TargetedOp::new_system_to(*key, vec![Op::Frame(Box::new(packet))]));
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
        Ok(LogicOutput::ops(out))
      }
    }
  }
}

/// A snapshot provider that provides nothing: the world goes out as `Op::Frame`,
/// which is a per-recipient delta on a fixed cadence, so join snapshots are off.
pub struct NoSnapshots;

#[async_trait]
impl plaza::snapshot::SnapshotProvider<PlayerKey, Arena, ()> for NoSnapshots {
  async fn create_snapshot_data(
    &self,
    _full_state: &Arena,
    _target_agent: Option<&Agent<PlayerKey>>,
    _context: Option<plaza::snapshot::SnapshotContext>,
  ) -> Result<plaza::SnapshotData<()>, plaza::snapshot::SnapshotError<PlayerKey>> {
    Ok(plaza::SnapshotData { payload: () })
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

  #[test]
  fn the_host_view_fills_in_and_frames_reach_the_player() {
    let controls = small();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });
    assert!(welcomed(&joined), "a joiner is welcomed and given a seat");

    let mut frames = 0;
    for _ in 0..6 {
      frames += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
    }
    assert!(frames > 0, "frames reach the seated player at zero latency");

    let v = view.lock();
    assert!(!v.players.is_empty() && !v.truth.is_empty(), "the omniscient view is populated");
    assert!(v.bytes_sent > 0 && v.uptime_ms > 0, "bandwidth accounting accrues");
  }

  #[test]
  fn latency_holds_frames_back_without_dropping_them() {
    let controls = Controls { latency_ms: 200, ..small() };
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view));
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });

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
  fn changing_the_enemy_count_rebuilds_and_rewelcomes() {
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs.clone(), None);
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });
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
  fn a_purchase_request_reaches_the_server() {
    // Proves the Buy op is routed to the authoritative server: an unaffordable
    // request is refused there, which is the only place that count can move.
    let controls = small();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });

    assert_eq!(state.sim.denied_purchases, 0);
    step(&logic, &mut state, LogicInput::AgentOps { source: Agent::new_human(1u64, "p1"), ops: vec![Op::Buy(Upgrade::Repulsor)] });
    assert_eq!(state.sim.denied_purchases, 1, "an empty wallet's purchase is refused by the server");
  }
}
