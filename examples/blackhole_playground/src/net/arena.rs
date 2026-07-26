//! The authoritative arena, as `plaza` core wants it: one `StateType` that owns
//! everything mutable, and one stateless `StateLogic` that acts on it.
//!
//! The adaptation is small because the simulation was already shaped for it.
//! `sim::Server` never reads client state, its `advance_seats` is a tick
//! function, and it already returns per-recipient packets. What this module adds
//! is the part a function argument was standing in for: seats that fill and
//! empty as people arrive, and inputs that arrive *between* ticks rather than
//! with them.
//!
//! That last point is the one structural difference from the offline loop.
//! Plaza delivers ops and time steps as **separate** inputs, so an input cannot
//! be handed to `advance` as it arrives. It is buffered on the seat and drained
//! when the tick comes, which is what a real server does anyway: inputs land
//! whenever the network feels like it, and the simulation consumes them on its
//! own clock.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{RateMeter, SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::server::{Seat, Server};
use crate::sim::types::{BlackHole, Controls, Packet, Pellet, PlayerId, Vec2, CLUSTER_BYTES, HOLE_BYTES};

/// How a connection is identified. Assigned by the server on accept, never
/// supplied by the client.
pub type PlayerKey = u64;

/// The seed for the impairment jitter. Fixed, so a host that drags the jitter
/// slider gets the same distribution every run rather than one that depends on
/// when it started.
const IMPAIR_SEED: u64 = 0x8177_1E55;

/// Everything the omniscient half of a host needs, published by the arena and
/// read by the host's UI and renderer.
///
/// The host is the server *and* a client in one process, so unlike a joiner it
/// legitimately has both sides: the authoritative truth here, and its own
/// believed state in its [`NetClient`]. This is the truth half. It is cloned into
/// a shared slot once per send round rather than per tick, because it carries the
/// whole pellet field and a joiner-rate copy is plenty for a readout.
///
/// [`NetClient`]: crate::net::client::NetClient
#[derive(Clone, Debug, Default)]
pub struct HostView {
  /// The authoritative holes.
  pub holes: Vec<BlackHole>,
  /// Where the server actually has every pellet, for the faint truth overlay a
  /// joiner never gets.
  pub pellets: Vec<Pellet>,
  /// Pellets eaten per player.
  pub scores: Vec<u32>,
  /// Whether each seat's dash is off cooldown.
  pub dash_ready: Vec<bool>,
  /// Which seats are mid-dash right now, for the burst effect.
  pub dashing: Vec<bool>,

  pub swallow_count: u64,
  pub collision_count: u64,
  pub eliminations: u64,
  pub mass_drained: f32,
  /// Sum of `effective_mass` over the live holes: the real pull a client's field
  /// is measured against, and the number culling quietly drops below.
  pub truth_field_weight: f32,

  bytes: RateMeter,
  corrections: RateMeter,
  hole_bytes: RateMeter,
}

impl HostView {
  pub fn bytes_per_sec(&self) -> f64 {
    self.bytes.per_sec()
  }

  /// The same traffic averaged over the whole life of the meter, shown beside
  /// the current rate rather than instead of it. A session mean sitting below
  /// the current rate is still climbing toward it, which is a fact about the
  /// average and not about the traffic, and reading one as the other cost this
  /// project an afternoon.
  pub fn lifetime_bytes_per_sec(&self) -> f64 {
    self.bytes.lifetime_per_sec()
  }

  /// What share of the wire the field itself costs, which is the example's whole
  /// question: sending a field instead of its consequences is only a win if the
  /// field is small.
  pub fn hole_bytes_share(&self) -> f64 {
    self.hole_bytes.share_of(&self.bytes)
  }

  pub fn mean_corrections_per_packet(&self) -> f64 {
    self.corrections.mean()
  }
}

/// The outbound impairment for one connection: frames held for `latency ± jitter`
/// before they are released.
///
/// The offline playground delayed the down-link with a client-side `LatencyLink`.
/// Here the host is a real server, so the impairment has to sit on the real
/// outbound path, which is what makes the latency and jitter sliders act on a
/// link instead of a simulation.
///
/// [`LatencyLink`] defaults to ordered delivery, which is what this needs and why
/// it is used directly rather than reimplemented here. A WebSocket is TCP: frames
/// arrive in the order they were sent and jitter shows up as bursty delay, not
/// shuffling. A private copy of this used to add jitter without clamping against
/// the previous frame's delivery time, so at the default 15 ms of jitter against a
/// ~16 ms send interval it could hand the client an older frame after a newer one.
/// Nothing downstream tolerates that: [`Packet::swallowed`] and [`Packet::spawned`]
/// are order-sensitive, so a late frame could resurrect a swallowed pellet or drop
/// a respawned one. It modelled a UDP link this transport is not.
///
/// [`Packet::swallowed`]: crate::sim::types::Packet::swallowed
/// [`Packet::spawned`]: crate::sim::types::Packet::spawned
type Downlink = LatencyLink<Packet>;

/// Everything the arena owns. `plaza` requires `Clone` for its state-query
/// command; nothing on the hot path clones it.
#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  /// Which hole each connected player is driving. Seats nobody holds are driven
  /// by bots, so an empty arena is still a game and a joiner never waits for the
  /// world to become interesting.
  seats: SeatTable<PlayerKey>,
  /// The newest input for each seat, applied on the next tick.
  pending: Vec<Seat>,
  /// Dash is an edge, not a level: it must fire once, on the tick after it is
  /// asked for, or holding the key would be a permanent dash.
  dash_requests: Vec<bool>,
  /// The newest input sequence accepted per player, echoed back so a client can
  /// replay only what the server has not seen.
  acked: HashMap<PlayerKey, u64>,

  /// Per-connection outbound impairment. Frames are held here for the host's
  /// latency and jitter before release; without a host driving those sliders it
  /// is a zero delay and a passthrough.
  down: HashMap<PlayerKey, Downlink>,
  rng: Rng,

  /// Wire accounting, summed across send rounds for the host's bandwidth
  /// readouts. Reset when the world is rebuilt, so a rate is over the current
  /// world rather than every world since launch.
  bytes: RateMeter,
  corrections: RateMeter,
  hole_bytes: RateMeter,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let sim = Server::new(controls.pellet_count, controls.player_count);
    let seats = controls.player_count;
    Self {
      sim,
      controls,
      seats: SeatTable::new(seats),
      pending: vec![Seat::Bot; seats],
      dash_requests: vec![false; seats],
      acked: HashMap::new(),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
      bytes: RateMeter::new(),
      corrections: RateMeter::new(),
      hole_bytes: RateMeter::new(),
    }
  }

  /// Rebuilds the world for a new pellet or hole count, reseating whoever is
  /// connected into a fresh hole and returning the seats that need a fresh
  /// `Welcome`.
  ///
  /// The offline playground handles a count change by building a new `World` and
  /// resetting; this is the same reset, and connected clients pick it up cleanly
  /// because a client rebuilds its own simulation on `Welcome`. Callers send one
  /// to every returned seat so nobody is left playing against a world that no
  /// longer exists.
  fn reconfigure(&mut self, controls: Controls) -> Vec<(PlayerKey, usize)> {
    let seats = controls.player_count;
    self.controls = controls;
    self.sim = Server::new(controls.pellet_count, seats);
    self.pending = vec![Seat::Bot; seats];
    self.dash_requests = vec![false; seats];
    self.acked.clear();
    self.down.clear();
    // The rates are over the current world, not every world since launch.
    for meter in self.meters() {
      meter.reset();
    }

    self.seats.reseat_all(seats)
  }

  /// Every counter, for the operations that apply to all of them at once.
  fn meters(&mut self) -> [&mut RateMeter; 3] {
    [&mut self.bytes, &mut self.corrections, &mut self.hole_bytes]
  }

  /// The omniscient snapshot the host reads. Cheap fields plus one clone of the
  /// hole and pellet vectors, taken at send rate rather than tick rate.
  fn host_view(&self) -> HostView {
    HostView {
      holes: self.sim.holes.clone(),
      pellets: self.sim.pellets.clone(),
      scores: self.sim.scores.clone(),
      dash_ready: (0..self.sim.holes.len()).map(|p| self.sim.dash_ready(p)).collect(),
      dashing: (0..self.sim.holes.len()).map(|p| self.sim.is_dashing(p)).collect(),
      swallow_count: self.sim.swallow_count,
      collision_count: self.sim.collision_count,
      eliminations: self.sim.eliminations,
      mass_drained: self.sim.mass_drained,
      truth_field_weight: self.sim.holes.iter().filter(|h| h.alive).map(|h| h.effective_mass()).sum(),
      bytes: self.bytes,
      corrections: self.corrections,
      hole_bytes: self.hole_bytes,
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    ServerPolicy {
      sync_hz: self.controls.sync_hz,
      mode: self.controls.mode,
      corrections_per_packet: self.controls.corrections_per_packet,
      pellet_count: self.controls.pellet_count,
      player_count: self.controls.player_count,
    }
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.seat_of(key)
  }

  /// Seats a joiner, or refuses when the arena is full.
  ///
  /// Refusing is a real outcome rather than an assertion: the arena has a fixed
  /// number of holes and a demo people can share is a demo people can overfill.
  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    let seating = self.seats.seat(key);
    if let Seating::Fresh(seat) = seating {
      // A rejoin keeps whatever it was doing; only a new occupant starts clean.
      self.pending[seat] = Seat::Bot;
      self.dash_requests[seat] = false;
    }
    seating.index()
  }

  fn unseat(&mut self, key: &PlayerKey) {
    if let Some(seat) = self.seats.unseat(key) {
      // Handed back to the bots rather than left frozen, so a disconnect does
      // not leave a statue in the arena.
      self.pending[seat] = Seat::Bot;
      self.dash_requests[seat] = false;
    }
    self.acked.remove(key);
    self.down.remove(key);
  }
}

/// The stateless half plaza acts through.
///
/// It carries two shared slots rather than owning that state: `controls` is
/// written by the host's UI and read here every tick, so the panel's sliders
/// reach the running arena; `view` is written here every send round and read by
/// the host's UI and renderer, so the host keeps the omniscient half it had when
/// the whole game lived in one process. A headless server has neither a panel nor
/// a screen, so its `view` is `None` and its `controls` is the fixed set it
/// launched with.
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
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
              key,
              vec![Op::Welcome {
                player: seat as PlayerId,
                policy,
              }],
            )]))
          }
          // No seat: say nothing rather than pretend. The client's connection
          // stays open and it shows "arena full" instead of an empty world it
          // cannot explain.
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
          // The uplink, impaired where the sliders are. See the same note in the
          // horde example: a joiner has no reason to sabotage its own outbound,
          // so the loss is applied on arrival. `Hello` and `Ping` are control
          // plane and are left alone.
          let droppable = matches!(op, Op::Input { .. });
          if droppable && state.controls.loss_pct > 0.0 && state.rng.unit() * 100.0 < state.controls.loss_pct {
            continue;
          }
          match op {
            Op::Input { seq, dx, dy, dash } => {
              // Out-of-order inputs are dropped rather than applied. Under
              // reordering an older direction would otherwise overwrite a newer
              // one, which reads to the player as the controls sticking.
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              // Normalised here, not on the client: a client that sent a longer
              // vector would simply move faster.
              let len = (dx * dx + dy * dy).sqrt();
              let dir = if len > 1.0 { Vec2::new(dx / len, dy / len) } else { Vec2::new(dx, dy) };
              state.pending[seat] = Seat::Steered(dir);
              if dash {
                state.dash_requests[seat] = true;
              }
            }
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              replies.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            // A client announcing a wire format that is not this one cannot be
            // reasoned with, only told. Said once, on the client's own first
            // message, instead of the per-op decode warnings a mismatch
            // otherwise produces for as long as it stays connected.
            Op::Hello { protocol } if protocol != PROTOCOL => {
              tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
              replies.push(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]));
            }
            Op::Hello { .. } => {}
            // Everything else is server-to-client. A client sending one is
            // confused or hostile; either way it is not an error worth failing
            // the tick over.
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        // Pick up whatever the host's panel changed. A count change rebuilds the
        // world and re-welcomes everyone; anything else is a live edit the next
        // tick simply reads.
        let live = *self.controls.lock();
        let mut welcomes = Vec::new();
        if live.pellet_count != state.controls.pellet_count || live.player_count != state.controls.player_count {
          for (key, seat) in state.reconfigure(live) {
            welcomes.push(TargetedOp::new_system_to(key, vec![Op::Welcome { player: seat as PlayerId, policy: state.policy() }]));
          }
        } else {
          state.controls = live;
        }

        // Dash is consumed here so it fires exactly once per request.
        for seat in 0..state.dash_requests.len() {
          if std::mem::replace(&mut state.dash_requests[seat], false) {
            state.sim.try_dash(seat);
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

        // Seat index back to the player holding it, so a packet built per seat
        // reaches the right connection.
        let by_seat = state.seats.by_seat();
        for (player, packet) in packets {
          state.bytes.add(packet.bytes() as u64);
          state.corrections.add(packet.corrections.len() as u64);
          state.hole_bytes.add((packet.holes.len() * HOLE_BYTES + packet.clusters.len() * CLUSTER_BYTES) as u64);
          if let Some(key) = by_seat.get(&(player as usize)) {
            let entry = state.down.entry(*key).or_default();
            entry.send(now, packet, controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
        }

        let mut out = welcomes;
        // Frames leave through the impairment link, so the host's latency and
        // jitter act on a real outbound path rather than being a number in a
        // panel. With no host, the delay is zero and this is a passthrough.
        for (key, link) in state.down.iter_mut() {
          for packet in link.drain_due(now) {
            out.push(TargetedOp::new_system_to(*key, vec![Op::Frame(packet)]));
          }
        }
        // Acknowledgements ride the tick, not the frame, because inputs arrive
        // far more often than frames go out, and they are not impaired: reeling
        // prediction back in should not itself be delayed.
        for (key, seq) in &state.acked {
          out.push(TargetedOp::new_system_to(*key, vec![Op::Ack { seq: *seq }]));
        }

        // Republish the omniscient half at send rate, which is frequent enough
        // for a readout and far cheaper than cloning the field every tick.
        if is_send_round && let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(out))
      }
    }
  }
}

/// A snapshot provider that provides nothing.
///
/// The world goes out as `Op::Frame` rather than as snapshots, because frames
/// are per-recipient deltas on a fixed cadence and `LogicOutput.ops` already
/// targets individuals. Plaza asks for a snapshot on join anyway unless told
/// otherwise, so the controller is built with that turned off and this exists
/// only to satisfy the type.
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
  use std::time::Duration;

  /// Drives the async logic once. The tests never actually await anything, so a
  /// bare current-thread runtime is enough to turn the crank.
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

  #[test]
  fn the_host_view_fills_in_and_frames_reach_the_player() {
    // The point of the whole change: the arena publishes the omniscient half a
    // host reads, and a seated player gets frames.
    let controls = Controls { latency_ms: 0, jitter_ms: 0, sync_hz: 60, ..Controls::default() };
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });
    assert!(welcomed(&joined), "a joiner is welcomed and given a seat");

    let mut frames = 0;
    for _ in 0..10 {
      frames += frames_in(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) }));
    }
    assert!(frames > 0, "frames reach the seated player at zero latency");

    let v = view.lock();
    assert!(!v.holes.is_empty() && !v.pellets.is_empty(), "the omniscient view is populated");
    assert!(v.bytes_per_sec() > 0.0, "bandwidth accounting accrues");
  }

  #[test]
  fn latency_holds_frames_back_without_dropping_them() {
    // The impairment link is what makes the latency slider act on a real path.
    // Held for the delay, then delivered, never lost.
    let controls = Controls { latency_ms: 200, jitter_ms: 0, sync_hz: 60, ..Controls::default() };
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view));
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });

    let mut early = 0;
    for _ in 0..5 {
      // ~80 ms of ticks, well inside the 200 ms latency.
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
  fn jitter_delays_frames_but_never_reorders_them() {
    // Regression. The impairment used to be a private delay queue that drew a
    // delivery time per frame without clamping against its predecessor, so jitter
    // comparable to the send interval let an older frame land after a newer one.
    // A WebSocket cannot do that, and nothing downstream tolerates it: `swallowed`
    // and `spawned` are order-sensitive, so a late frame resurrects a pellet the
    // client already ate. Jitter far larger than the gap between sends is the case
    // that shuffles if the clamp is missing.
    let controls = Controls { latency_ms: 40, jitter_ms: 300, sync_hz: 60, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });

    let mut stamps: Vec<u64> = Vec::new();
    for _ in 0..200 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
      for op in &out.ops {
        if let Some(Op::Frame(packet)) = op.ops.first() {
          stamps.push(packet.server_time_ms);
        }
      }
    }

    assert!(stamps.len() > 20, "the test never delivered enough frames to judge: {}", stamps.len());
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    assert_eq!(stamps, sorted, "frames were delivered out of the order they were sent");
  }

  #[test]
  fn changing_the_hole_count_rebuilds_and_rewelcomes() {
    let controls = Controls { latency_ms: 0, jitter_ms: 0, sync_hz: 60, player_count: 8, ..Controls::default() };
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs.clone(), Some(view.clone()));
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64, "p1") });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert_eq!(view.lock().holes.len(), 8);

    // The host drags the black-holes slider. The world rebuilds and the still
    // connected player is welcomed into it rather than left in the old one.
    cs.lock().player_count = 16;
    let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert!(welcomed(&out), "the reseated player is welcomed into the new world");
    assert_eq!(view.lock().holes.len(), 16, "and the world actually grew");
  }

  #[test]
  fn a_live_edit_reaches_the_running_sim() {
    // No view: a headless-shaped logic with only a control slot, which the host's
    // panel writes and the arena reads on the next tick.
    let controls = Controls { corrections_per_packet: 40, ..Controls::default() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs.clone(), None);
    let mut state = Arena::new(controls);

    cs.lock().corrections_per_packet = 5;
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(16) });
    assert_eq!(state.controls.corrections_per_packet, 5, "the arena reads the panel's edit");
  }
}
