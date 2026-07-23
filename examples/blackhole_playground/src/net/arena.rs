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
use plaza_client_utils::net_sim::Rng;

use crate::sim::protocol::{Op, ServerPolicy};
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

  bytes_sent: u64,
  packets_sent: u64,
  corrections_sent: u64,
  hole_bytes: u64,
  /// Server clock, for turning byte totals into rates.
  uptime_ms: u64,
}

impl HostView {
  pub fn bytes_per_sec(&self) -> f64 {
    if self.uptime_ms == 0 {
      return 0.0;
    }
    self.bytes_sent as f64 / (self.uptime_ms as f64 / 1000.0)
  }

  pub fn hole_bytes_share(&self) -> f64 {
    if self.bytes_sent == 0 {
      return 0.0;
    }
    self.hole_bytes as f64 / self.bytes_sent as f64
  }

  pub fn mean_corrections_per_packet(&self) -> f64 {
    if self.packets_sent == 0 {
      return 0.0;
    }
    self.corrections_sent as f64 / self.packets_sent as f64
  }
}

/// The outbound impairment for one connection: frames held for `latency ± jitter`
/// before they are released.
///
/// The offline playground delayed the down-link with a client-side `LatencyLink`.
/// Here the host is a real server, so the impairment has to sit on the real
/// outbound path, which is what makes the latency and jitter sliders act on a
/// link instead of a simulation. Kept `Clone` because the arena state must be
/// (plaza clones it for a state query, never on the hot path), and `LatencyLink`
/// is not, so this is the same delay queue with a derivable clone.
#[derive(Clone, Debug, Default)]
struct Downlink {
  /// `(deliver_at_ms, frame)`, drained oldest-delivery-first.
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
  /// Which hole each connected player is driving.
  seats: HashMap<PlayerKey, usize>,
  /// Seats nobody holds. Bots drive these, so an empty arena is still a game
  /// and a joiner never waits for the world to become interesting.
  free: Vec<usize>,
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
  bytes_sent: u64,
  packets_sent: u64,
  corrections_sent: u64,
  hole_bytes: u64,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let sim = Server::new(controls.pellet_count, controls.player_count);
    let seats = controls.player_count;
    Self {
      sim,
      controls,
      seats: HashMap::new(),
      free: (0..seats).collect(),
      pending: vec![Seat::Bot; seats],
      dash_requests: vec![false; seats],
      acked: HashMap::new(),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
      bytes_sent: 0,
      packets_sent: 0,
      corrections_sent: 0,
      hole_bytes: 0,
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
    let old_keys: Vec<PlayerKey> = self.seats.keys().copied().collect();
    let seats = controls.player_count;
    self.controls = controls;
    self.sim = Server::new(controls.pellet_count, seats);
    self.free = (0..seats).collect();
    self.pending = vec![Seat::Bot; seats];
    self.dash_requests = vec![false; seats];
    self.seats.clear();
    self.acked.clear();
    self.down.clear();
    self.bytes_sent = 0;
    self.packets_sent = 0;
    self.corrections_sent = 0;
    self.hole_bytes = 0;

    let mut welcomed = Vec::new();
    for key in old_keys {
      if let Some(seat) = self.free.pop() {
        self.seats.insert(key, seat);
        welcomed.push((key, seat));
      }
    }
    welcomed
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
      bytes_sent: self.bytes_sent,
      packets_sent: self.packets_sent,
      corrections_sent: self.corrections_sent,
      hole_bytes: self.hole_bytes,
      uptime_ms: self.sim.now_ms(),
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
    self.seats.get(key).copied()
  }

  /// Seats a joiner, or refuses when the arena is full.
  ///
  /// Refusing is a real outcome rather than an assertion: the arena has a fixed
  /// number of holes and a demo people can share is a demo people can overfill.
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
      // Handed back to the bots rather than left frozen, so a disconnect does
      // not leave a statue in the arena.
      self.pending[seat] = Seat::Bot;
      self.dash_requests[seat] = false;
      self.free.push(seat);
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

        // Seat index back to the player holding it, so a packet built per seat
        // reaches the right connection.
        let by_seat: HashMap<usize, PlayerKey> = state.seats.iter().map(|(key, seat)| (*seat, *key)).collect();
        for (player, packet) in packets {
          state.bytes_sent += packet.bytes() as u64;
          state.packets_sent += 1;
          state.corrections_sent += packet.corrections.len() as u64;
          state.hole_bytes += (packet.holes.len() * HOLE_BYTES + packet.clusters.len() * CLUSTER_BYTES) as u64;
          if let Some(key) = by_seat.get(&(player as usize)) {
            let entry = state.down.entry(*key).or_default();
            entry.send(now, packet, controls.latency_ms, controls.jitter_ms, &mut state.rng);
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
    assert!(v.bytes_sent > 0 && v.uptime_ms > 0, "bandwidth accounting accrues");
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
